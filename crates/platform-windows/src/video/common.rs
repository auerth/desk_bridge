//! Shared D3D11/DXGI helpers of the video backends: device creation, output listing,
//! the zero-copy frame type and the monotonic microsecond clock.

use std::any::Any;
use std::sync::OnceLock;

use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ERROR_NOT_FOUND, DXGI_OUTPUT_DESC, IDXGIAdapter,
    IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::core::Interface;

/// Maps a `windows` error to the platform error type.
pub(crate) fn os_err(what: &str, e: &windows::core::Error) -> PlatformError {
    PlatformError::Os {
        code: i64::from(e.code().0),
        msg: format!("{what}: {e}"),
    }
}

fn qpc_freq() -> i64 {
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer; cannot fail on Windows XP and later.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// Monotonic clock in microseconds (QueryPerformanceCounter, same time base as
/// `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime`), used for all frame timestamps on this machine.
#[must_use]
pub fn now_us() -> u64 {
    let mut t = 0i64;
    // SAFETY: valid out pointer.
    let _ = unsafe { QueryPerformanceCounter(&mut t) };
    qpc_to_us(t)
}

/// Converts QPC ticks to microseconds.
#[must_use]
pub fn qpc_to_us(ticks: i64) -> u64 {
    let f = qpc_freq();
    let ticks = ticks.max(0) as u128;
    (ticks * 1_000_000 / f as u128) as u64
}

/// A video picture that lives on the GPU. Produced by the capturer (BGRA) and by the hardware
/// decoder (NV12 texture-array slice); consumed by the encoder and the presenter.
pub struct GpuFrame {
    pub texture: ID3D11Texture2D,
    pub device: ID3D11Device,
    /// LUID of the adapter the texture lives on (`LowPart | HighPart << 32`).
    pub adapter_luid: i64,
    /// Array slice of `texture` (decoder pools are texture arrays; 0 otherwise).
    pub subresource: u32,
    pub format: DXGI_FORMAT,
    pub width: u32,
    pub height: u32,
    /// Keeps the pool slot this picture lives in reserved (returned to its pool when dropped).
    pub hold: Option<Box<dyn Any + Send>>,
}

impl GpuFrame {
    /// Downcasts the opaque payload of a `FrameData::Gpu`.
    #[must_use]
    pub fn from_any(data: &(dyn Any + Send)) -> Option<&GpuFrame> {
        data.downcast_ref::<GpuFrame>()
    }
}

/// A D3D11 device together with its immediate context and adapter identity.
#[derive(Clone)]
pub struct D3dDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_luid: i64,
    pub adapter_name: String,
}

pub(crate) fn luid_to_i64(l: windows::Win32::Foundation::LUID) -> i64 {
    (i64::from(l.HighPart) << 32) | i64::from(l.LowPart)
}

pub(crate) fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// Creates a D3D11 device (BGRA + video support, multithread-protected) on `adapter`
/// (`None` = default adapter).
pub fn create_device(adapter: Option<&IDXGIAdapter1>) -> Result<D3dDevice> {
    let levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let adapter_base: Option<IDXGIAdapter> = match adapter {
        Some(a) => Some(
            a.cast()
                .map_err(|e| os_err("IDXGIAdapter1 -> IDXGIAdapter", &e))?,
        ),
        None => None,
    };
    // With an explicit adapter the driver type must be UNKNOWN; with none, HARDWARE.
    let driver = if adapter_base.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE
    };
    // SAFETY: out pointers are valid for the duration of the call; `levels` outlives it.
    unsafe {
        D3D11CreateDevice(
            adapter_base.as_ref(),
            driver,
            HMODULE::default(),
            flags,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| os_err("D3D11CreateDevice", &e))?;
    let device =
        device.ok_or_else(|| PlatformError::other("D3D11CreateDevice returned no device"))?;
    let context =
        context.ok_or_else(|| PlatformError::other("D3D11CreateDevice returned no context"))?;
    enable_multithread_protection(&context);
    let (luid, name) = device_adapter_info(&device);
    Ok(D3dDevice {
        device,
        context,
        adapter_luid: luid,
        adapter_name: name,
    })
}

/// Several threads (capture, encode, decode, present) share one immediate context, so the
/// device must serialise calls itself.
pub fn enable_multithread_protection(context: &ID3D11DeviceContext) {
    if let Ok(mt) = context.cast::<ID3D11Multithread>() {
        // SAFETY: plain COM call.
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
}

/// `(luid, description)` of the adapter a device was created on.
#[must_use]
pub fn device_adapter_info(device: &ID3D11Device) -> (i64, String) {
    let r = (|| -> windows::core::Result<(i64, String)> {
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: plain COM calls.
        let adapter = unsafe { dxgi.GetAdapter()? };
        let desc = unsafe { adapter.GetDesc()? };
        Ok((
            luid_to_i64(desc.AdapterLuid),
            wide_to_string(&desc.Description),
        ))
    })();
    r.unwrap_or((0, String::from("unknown adapter")))
}

/// One display output as seen by DXGI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter_name: String,
    pub adapter_luid: i64,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device_name: String,
    /// Position and size on the virtual desktop in physical pixels.
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub attached: bool,
    /// Adapter is the WARP / basic render driver (never use it for capture).
    pub software: bool,
}

impl OutputInfo {
    #[must_use]
    pub fn rect(&self) -> RECT {
        RECT {
            left: self.x,
            top: self.y,
            right: self.x + self.w as i32,
            bottom: self.y + self.h as i32,
        }
    }
}

/// Lists all outputs of all adapters (attached or not).
pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    // SAFETY: plain DXGI enumeration; every interface is owned and dropped here.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| os_err("CreateDXGIFactory1", &e))?;
        let mut out = Vec::new();
        let mut ai = 0u32;
        loop {
            let adapter = match factory.EnumAdapters1(ai) {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(os_err("EnumAdapters1", &e)),
            };
            let desc: DXGI_ADAPTER_DESC1 =
                adapter.GetDesc1().map_err(|e| os_err("GetDesc1", &e))?;
            let software = desc.Flags & 2 != 0; // DXGI_ADAPTER_FLAG_SOFTWARE
            let name = wide_to_string(&desc.Description);
            let luid = luid_to_i64(desc.AdapterLuid);
            let mut oi = 0u32;
            loop {
                let output = match adapter.EnumOutputs(oi) {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(os_err("EnumOutputs", &e)),
                };
                let d: DXGI_OUTPUT_DESC = output.GetDesc().map_err(|e| os_err("GetDesc", &e))?;
                let r = d.DesktopCoordinates;
                out.push(OutputInfo {
                    adapter_index: ai,
                    output_index: oi,
                    adapter_name: name.clone(),
                    adapter_luid: luid,
                    device_name: wide_to_string(&d.DeviceName),
                    x: r.left,
                    y: r.top,
                    w: (r.right - r.left).max(0) as u32,
                    h: (r.bottom - r.top).max(0) as u32,
                    attached: d.AttachedToDesktop.as_bool(),
                    software,
                });
                oi += 1;
            }
            ai += 1;
        }
        Ok(out)
    }
}

/// Returns the DXGI adapter with the given index.
pub fn adapter_by_index(index: u32) -> Result<IDXGIAdapter1> {
    // SAFETY: plain DXGI calls.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| os_err("CreateDXGIFactory1", &e))?;
        factory
            .EnumAdapters1(index)
            .map_err(|e| os_err("EnumAdapters1", &e))
    }
}

/// Finds the adapter with the given LUID.
pub fn adapter_by_luid(luid: i64) -> Result<IDXGIAdapter1> {
    // SAFETY: plain DXGI calls.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| os_err("CreateDXGIFactory1", &e))?;
        let mut i = 0;
        loop {
            let a = factory
                .EnumAdapters1(i)
                .map_err(|_| PlatformError::DeviceNotFound(format!("adapter luid {luid:#x}")))?;
            if luid_to_i64(
                a.GetDesc1()
                    .map_err(|e| os_err("GetDesc1", &e))?
                    .AdapterLuid,
            ) == luid
            {
                return Ok(a);
            }
            i += 1;
        }
    }
}

/// Selects the output whose desktop rectangle equals `(x, y, w, h)`; falls back to the output
/// with the largest overlap.
#[must_use]
pub fn find_output_by_rect(
    outputs: &[OutputInfo],
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Option<&OutputInfo> {
    if let Some(o) = outputs
        .iter()
        .find(|o| !o.software && o.attached && o.x == x && o.y == y && o.w == w && o.h == h)
    {
        return Some(o);
    }
    let area = |o: &OutputInfo| -> i64 {
        let l = x.max(o.x);
        let t = y.max(o.y);
        let r = (x + w as i32).min(o.x + o.w as i32);
        let b = (y + h as i32).min(o.y + o.h as i32);
        if r > l && b > t {
            i64::from(r - l) * i64::from(b - t)
        } else {
            0
        }
    };
    outputs
        .iter()
        .filter(|o| !o.software && o.attached)
        .map(|o| (area(o), o))
        .filter(|(a, _)| *a > 0)
        .max_by_key(|(a, _)| *a)
        .map(|(_, o)| o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(a: u32, o: u32, x: i32, y: i32, w: u32, h: u32) -> OutputInfo {
        OutputInfo {
            adapter_index: a,
            output_index: o,
            adapter_name: "gpu".into(),
            adapter_luid: 1,
            device_name: format!("\\\\.\\DISPLAY{}", o + 1),
            x,
            y,
            w,
            h,
            attached: true,
            software: false,
        }
    }

    #[test]
    fn rect_matching() {
        let v = vec![out(0, 0, 0, 0, 1920, 1080), out(0, 1, 1920, 0, 5120, 1440)];
        assert_eq!(
            find_output_by_rect(&v, 1920, 0, 5120, 1440)
                .unwrap()
                .output_index,
            1
        );
        // slightly different rect: largest overlap wins
        assert_eq!(
            find_output_by_rect(&v, 2000, 0, 2560, 1440)
                .unwrap()
                .output_index,
            1
        );
        assert!(find_output_by_rect(&v, 10_000, 0, 100, 100).is_none());
    }

    #[test]
    fn clock_is_monotonic() {
        let a = now_us();
        let b = now_us();
        assert!(b >= a);
        assert_eq!(qpc_to_us(0), 0);
    }

    #[test]
    fn luid_packing() {
        let l = windows::Win32::Foundation::LUID {
            LowPart: 5,
            HighPart: 2,
        };
        assert_eq!(luid_to_i64(l), (2i64 << 32) | 5);
        assert_eq!(wide_to_string(&[104, 105, 0, 120]), "hi");
    }
}
