//! Small RAII / COM helpers shared by the audio submodules.

use deskbridge_platform::PlatformError;
use deskbridge_platform::audio::{DeviceKind, RATE};
use std::sync::OnceLock;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_RESOURCES_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
    DEVICE_STATE_ACTIVE, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMEndpoint,
    MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eCapture,
    eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
};
use windows::core::{GUID, HSTRING, PCWSTR, w};

use super::util::qpc_ticks_to_us;

/// Converts a `windows` error to the platform error type.
pub(crate) fn os_err(e: windows::core::Error) -> PlatformError {
    PlatformError::Os {
        code: i64::from(e.code().0 as u32),
        msg: e.message(),
    }
}

/// Like [`os_err`] with context prepended.
pub(crate) fn os_ctx(ctx: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Os {
        code: i64::from(e.code().0 as u32),
        msg: format!("{ctx}: {}", e.message()),
    }
}

/// True for errors meaning "the endpoint is gone / must be reopened" (unplug, power-off, driver
/// reset, audio service restart).
pub(crate) fn is_device_lost(e: &windows::core::Error) -> bool {
    let c = e.code();
    c == AUDCLNT_E_DEVICE_INVALIDATED
        || c == AUDCLNT_E_RESOURCES_INVALIDATED
        || c == AUDCLNT_E_SERVICE_NOT_RUNNING
}

/// Initialises COM as MTA on the current thread for the guard's lifetime. If the thread already
/// has a different apartment model (`RPC_E_CHANGED_MODE`) COM is still usable and nothing is
/// uninitialised on drop. Declare the guard *before* any COM object so those are released first.
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub fn init() -> Self {
        // SAFETY: plain FFI call; balanced by `CoUninitialize` in `Drop` only when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `CoInitializeEx` of this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Owned Win32 event handle.
pub(crate) struct Event(HANDLE);

// SAFETY: event handles are process-wide kernel objects usable from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub fn new(manual_reset: bool) -> windows::core::Result<Self> {
        // SAFETY: no security attributes / name; the handle is owned and closed in `Drop`.
        let h = unsafe { CreateEventW(None, manual_reset, false, None)? };
        Ok(Self(h))
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }

    pub fn set(&self) {
        // SAFETY: the handle is valid for the lifetime of `self`.
        let _ = unsafe { SetEvent(self.0) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Registers the current thread with MMCSS ("Pro Audio") and reverts on drop.
pub(crate) struct Mmcss(Option<HANDLE>);

impl Mmcss {
    pub fn pro_audio() -> Self {
        let mut index = 0u32;
        // SAFETY: valid NUL-terminated task name and out pointer.
        match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) } {
            Ok(h) => Self(Some(h)),
            Err(e) => {
                tracing::warn!("AvSetMmThreadCharacteristics(Pro Audio) failed: {e}");
                Self(None)
            }
        }
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(h) = self.0 {
            // SAFETY: `h` came from `AvSetMmThreadCharacteristicsW` on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
        }
    }
}

/// Current QPC time in microseconds (same clock as WASAPI capture timestamps).
pub(crate) fn qpc_now_us() -> u64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f
    });
    let mut t = 0i64;
    // SAFETY: valid out pointer.
    let _ = unsafe { QueryPerformanceCounter(&mut t) };
    qpc_ticks_to_us(t, freq)
}

pub(crate) fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    // SAFETY: standard COM activation; COM is initialised by the caller's `ComGuard`.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

pub(crate) fn data_flow(kind: DeviceKind) -> EDataFlow {
    match kind {
        DeviceKind::Render => eRender,
        DeviceKind::Capture => eCapture,
    }
}

/// Reads and frees the endpoint id string of `dev`.
pub(crate) fn device_id(dev: &IMMDevice) -> windows::core::Result<String> {
    // SAFETY: `GetId` returns a CoTaskMem string that we copy and free.
    unsafe {
        let p = dev.GetId()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(s)
    }
}

/// Data flow of an endpoint device.
pub(crate) fn device_kind(dev: &IMMDevice) -> windows::core::Result<DeviceKind> {
    let ep: IMMEndpoint = windows_core::Interface::cast(dev)?;
    // SAFETY: simple getter.
    let flow = unsafe { ep.GetDataFlow()? };
    Ok(if flow == eCapture {
        DeviceKind::Capture
    } else {
        DeviceKind::Render
    })
}

/// Opens an *active* endpoint of the given kind: the explicit `id`, or the default for `role`.
pub(crate) fn open_device(
    en: &IMMDeviceEnumerator,
    kind: DeviceKind,
    id: Option<&str>,
    role: ERole,
) -> Result<IMMDevice, PlatformError> {
    match id {
        Some(id) => {
            let h = HSTRING::from(id);
            // SAFETY: `h` outlives the call and is NUL-terminated.
            let dev = unsafe { en.GetDevice(PCWSTR(h.as_ptr())) }
                .map_err(|_| PlatformError::DeviceNotFound(id.to_string()))?;
            // SAFETY: simple getter.
            let state = unsafe { dev.GetState() }.map_err(os_err)?;
            if state != DEVICE_STATE_ACTIVE {
                return Err(PlatformError::DeviceNotFound(format!(
                    "{id} (state {:#x}, not active)",
                    state.0
                )));
            }
            if device_kind(&dev).map_err(os_err)? != kind {
                return Err(PlatformError::DeviceNotFound(format!(
                    "{id} (wrong data flow)"
                )));
            }
            Ok(dev)
        }
        None => {
            // SAFETY: simple getter.
            unsafe { en.GetDefaultAudioEndpoint(data_flow(kind), role) }.map_err(|e| {
                PlatformError::DeviceNotFound(format!("no default {kind:?} device ({e})"))
            })
        }
    }
}

/// `WAVE_FORMAT_EXTENSIBLE` format tag.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// 48 kHz 32-bit float PCM `WAVEFORMATEXTENSIBLE` for 1 or 2 channels.
pub(crate) fn float_format(channels: u16) -> WAVEFORMATEXTENSIBLE {
    let block = channels * 4;
    let mask = match channels {
        1 => 0x4, // SPEAKER_FRONT_CENTER
        _ => 0x3, // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
    };
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE,
            nChannels: channels,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: mask,
        SubFormat: SUBTYPE_IEEE_FLOAT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_format_layout() {
        let f = float_format(2);
        // Packed struct: copy fields by value.
        assert_eq!({ f.Format.nChannels }, 2);
        assert_eq!({ f.Format.nSamplesPerSec }, 48_000);
        assert_eq!({ f.Format.nBlockAlign }, 8);
        assert_eq!({ f.Format.nAvgBytesPerSec }, 384_000);
        assert_eq!({ f.Format.wBitsPerSample }, 32);
        assert_eq!({ f.Format.cbSize }, 22);
        assert_eq!({ f.dwChannelMask }, 3);
        assert_eq!({ f.SubFormat }, SUBTYPE_IEEE_FLOAT);
        let m = float_format(1);
        assert_eq!({ m.Format.nBlockAlign }, 4);
        assert_eq!({ m.dwChannelMask }, 4);
    }

    #[test]
    fn qpc_is_monotonic() {
        let a = qpc_now_us();
        let b = qpc_now_us();
        assert!(b >= a);
        assert!(a > 0);
    }
}
