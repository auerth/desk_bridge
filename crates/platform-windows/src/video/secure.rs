//! Picture of the secure desktop (UAC consent, lock screen, logon) handed from the SYSTEM helper
//! to the agent without leaving the GPU (ADR 0007).
//!
//! * [`SecureProducer`] (helper side): duplicates the output from a thread that is attached to the
//!   input desktop and copies every new picture into a ring of D3D11 textures created with
//!   `SHARED_NTHANDLE | SHARED_KEYEDMUTEX`. Each texture has a *name* (`CreateSharedHandle`), so
//!   only a string has to cross the pipe, never a process-local handle value.
//! * [`SecureConsumer`] (agent side): opens a texture by name on the agent's own device and copies
//!   it into a pool texture, which then travels the normal `CaptureOutput::Frame` path.
//! * [`SecureFrameSource`] is the transport-neutral control surface (start / poll / stop) the
//!   capturer uses; the agent implements it over the helper pipe.
//!
//! Keyed-mutex protocol per ring texture: the producer acquires key 0, writes, releases with key 1;
//! the consumer acquires key 1, reads, releases with key 0. A texture whose picture was never read
//! (key still 1) is taken back by the producer (acquire 1, release 0) before it is reused, so the
//! consumer can skip pictures without blocking the helper.

use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11Device1, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIKeyedMutex,
    IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, IDXGIResource1,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::core::{Interface, PCWSTR};

use super::common::{
    OutputInfo, adapter_by_index, create_device, find_output_by_rect, list_outputs, os_err,
};

/// Number of shared textures in the ring (one being written, one being read, one spare).
pub const RING_SIZE: usize = 3;
/// `DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE`; both sides need write access because
/// acquiring and releasing a keyed mutex changes it.
const SHARED_ACCESS: u32 = 0x8000_0001;
/// `AcquireSync` reports a timeout as the *success* code `WAIT_TIMEOUT`.
const WAIT_TIMEOUT_HR: i32 = 0x102;

// ---------------------------------------------------------------------------------------------
// Control surface used by the capturer
// ---------------------------------------------------------------------------------------------

/// Answer to starting a secure capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecureStartInfo {
    /// Adapter the shared textures live on (`LowPart | HighPart << 32`).
    pub luid: i64,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Something new on the secure desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecureUpdate {
    pub generation: u32,
    /// Shared texture that holds the latest picture.
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub pointer_x: i32,
    pub pointer_y: i32,
    pub pointer_visible: bool,
    /// The picture changed since the polled generation (otherwise only the pointer moved).
    pub image: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurePoll {
    Update(SecureUpdate),
    /// Nothing new (the secure desktop stands still most of the time).
    Idle,
}

/// Where secure-desktop pictures come from. Implemented by the agent over the helper pipe.
pub trait SecureFrameSource: Send + Sync {
    /// Starts capturing the output that covers the desktop rectangle `(x, y, w, h)`.
    fn start(&self, rect: (i32, i32, u32, u32)) -> std::result::Result<SecureStartInfo, String>;
    /// Waits up to `wait_ms` for something newer than `generation` (0 = anything).
    fn poll(&self, generation: u32, wait_ms: u16) -> std::result::Result<SecurePoll, String>;
    /// Ends the capture; the helper drops its textures.
    fn stop(&self);
}

// ---------------------------------------------------------------------------------------------
// Keyed mutex
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acquired {
    Yes,
    TimedOut,
}

fn acquire_sync(m: &IDXGIKeyedMutex, key: u64, ms: u32) -> Result<Acquired> {
    // SAFETY: plain vtable call on a live interface. The generated `AcquireSync` wrapper maps every
    // success code to `Ok(())`, which cannot tell S_OK from WAIT_TIMEOUT, hence the raw call.
    let hr = unsafe { (Interface::vtable(m).AcquireSync)(Interface::as_raw(m), key, ms) };
    match hr.0 {
        0 => Ok(Acquired::Yes),
        WAIT_TIMEOUT_HR => Ok(Acquired::TimedOut),
        code => Err(PlatformError::Os {
            code: i64::from(code),
            msg: format!("IDXGIKeyedMutex::AcquireSync({key}): {hr:?}"),
        }),
    }
}

fn release_sync(m: &IDXGIKeyedMutex, key: u64) -> Result<()> {
    // SAFETY: plain COM call; the caller holds the mutex.
    unsafe { m.ReleaseSync(key) }.map_err(|e| os_err("IDXGIKeyedMutex::ReleaseSync", &e))
}

/// Producer side: takes the texture for writing (key 0), reclaiming a picture nobody read (key 1).
/// `false` = the consumer is reading it right now.
fn acquire_for_write(m: &IDXGIKeyedMutex) -> Result<bool> {
    if acquire_sync(m, 0, 0)? == Acquired::Yes {
        return Ok(true);
    }
    if acquire_sync(m, 1, 0)? == Acquired::Yes {
        release_sync(m, 0)?;
        return Ok(acquire_sync(m, 0, 0)? == Acquired::Yes);
    }
    Ok(false)
}

// ---------------------------------------------------------------------------------------------
// Producer (helper)
// ---------------------------------------------------------------------------------------------

/// Pointer state of the secure desktop (output pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

/// What a producer captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProducerInfo {
    pub luid: i64,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Result of one [`SecureProducer::pump`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pump {
    /// No new picture and no pointer change.
    Idle,
    Update {
        /// Name of the ring texture that now holds the latest picture.
        image: Option<String>,
        pointer: Option<Pointer>,
    },
    /// `DXGI_ERROR_ACCESS_LOST` (desktop or mode switch): drop the producer and open a new one.
    Lost,
}

struct RingSlot {
    tex: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    /// Keeps the named object alive; closed with the slot.
    handle: HANDLE,
    name: String,
}

impl Drop for RingSlot {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateSharedHandle` and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Duplicates an output and publishes its pictures in named shared textures. Must be created and
/// used on one thread that is attached to the desktop to capture.
pub struct SecureProducer {
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    ring: Vec<RingSlot>,
    next: usize,
    info: ProducerInfo,
    have_image: bool,
    pointer: Pointer,
}

/// The output for a rectangle: exact or best overlap, else the first real attached one (the
/// helper's idea of the topology on the lock screen can differ from the agent's).
fn pick_output(outputs: &[OutputInfo], rect: (i32, i32, u32, u32)) -> Option<&OutputInfo> {
    find_output_by_rect(outputs, rect.0, rect.1, rect.2, rect.3)
        .or_else(|| outputs.iter().find(|o| o.attached && !o.software))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl SecureProducer {
    /// Opens the duplication of the output at `rect` and creates the ring. The textures are named
    /// `{name_prefix}-{slot}`; `security` is their DACL (the interactive user must be allowed).
    pub fn open(
        rect: (i32, i32, u32, u32),
        name_prefix: &str,
        security: Option<&SECURITY_ATTRIBUTES>,
    ) -> Result<Self> {
        crate::input::init_dpi_awareness();
        let outputs = list_outputs()?;
        let out = pick_output(&outputs, rect)
            .ok_or_else(|| PlatformError::DeviceNotFound("no attached output".into()))?
            .clone();
        if out.software {
            return Err(PlatformError::Unsupported(
                "software adapter cannot be captured",
            ));
        }
        let adapter = adapter_by_index(out.adapter_index)?;
        let dev = create_device(Some(&adapter))?;
        // SAFETY: plain COM calls on interfaces owned here.
        let (dup, w, h, format) = unsafe {
            let output: IDXGIOutput = adapter
                .EnumOutputs(out.output_index)
                .map_err(|e| os_err("EnumOutputs", &e))?;
            let output1: IDXGIOutput1 = output.cast().map_err(|e| os_err("IDXGIOutput1", &e))?;
            let dup = output1
                .DuplicateOutput(&dev.device)
                .map_err(|e| os_err("DuplicateOutput (secure desktop)", &e))?;
            let d = dup.GetDesc();
            (dup, d.ModeDesc.Width, d.ModeDesc.Height, d.ModeDesc.Format)
        };
        if format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(PlatformError::Unsupported(
                "secure desktop is not BGRA8 (HDR?)",
            ));
        }
        let ring = create_ring(&dev.device, w, h, name_prefix, security)?;
        Ok(Self {
            context: dev.context,
            dup,
            ring,
            next: 0,
            info: ProducerInfo {
                luid: dev.adapter_luid,
                x: out.x,
                y: out.y,
                w,
                h,
            },
            have_image: false,
            pointer: Pointer::default(),
        })
    }

    #[must_use]
    pub fn info(&self) -> ProducerInfo {
        self.info
    }

    /// Waits up to `timeout_ms` for the next picture or pointer change, copies a new picture into
    /// the ring and releases the duplicated frame at once.
    pub fn pump(&mut self, timeout_ms: u32) -> Result<Pump> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: valid out pointers.
        let r = unsafe { self.dup.AcquireNextFrame(timeout_ms, &mut info, &mut res) };
        match r {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Pump::Idle),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Ok(Pump::Lost),
            Err(e) => return Err(os_err("AcquireNextFrame (secure desktop)", &e)),
        }
        // A still desktop delivers no "new picture" event: take the first thing we are given.
        let want_image = info.LastPresentTime != 0 || (!self.have_image && res.is_some());
        let published = if want_image {
            self.publish(res.as_ref())
        } else {
            Ok(None)
        };
        let pointer = (info.LastMouseUpdateTime != 0)
            .then(|| Pointer {
                x: info.PointerPosition.Position.x,
                y: info.PointerPosition.Position.y,
                visible: info.PointerPosition.Visible.as_bool(),
            })
            .filter(|p| *p != self.pointer);
        if let Some(p) = pointer {
            self.pointer = p;
        }
        // SAFETY: a frame is currently acquired. Released before anything can fail, or the
        // compositor would be held up.
        let _ = unsafe { self.dup.ReleaseFrame() };
        drop(res);
        let image = published?;
        if image.is_some() {
            self.have_image = true;
        }
        if image.is_none() && pointer.is_none() {
            return Ok(Pump::Idle);
        }
        Ok(Pump::Update { image, pointer })
    }

    /// Copies the acquired desktop image into the next free ring texture. `Ok(None)`: every
    /// texture is being read (the picture is dropped, back-pressure).
    fn publish(&mut self, res: Option<&IDXGIResource>) -> Result<Option<String>> {
        let res =
            res.ok_or_else(|| PlatformError::other("AcquireNextFrame returned no resource"))?;
        let src: ID3D11Resource = res
            .cast()
            .map_err(|e| os_err("desktop resource -> resource", &e))?;
        let n = self.ring.len();
        for k in 0..n {
            let i = (self.next + k) % n;
            let slot = &self.ring[i];
            if !acquire_for_write(&slot.mutex)? {
                continue;
            }
            let copy = slot
                .tex
                .cast::<ID3D11Resource>()
                .map_err(|e| os_err("texture -> resource", &e))
                .map(|dst| {
                    // SAFETY: both resources live on this device; the ring was created with the
                    // size and format of the duplication.
                    unsafe {
                        self.context.CopyResource(&dst, &src);
                        self.context.Flush();
                    }
                });
            // Hand the picture over (key 1) even after a failed copy: the mutex must not stay
            // acquired by us.
            release_sync(&slot.mutex, 1)?;
            copy?;
            self.next = (i + 1) % n;
            return Ok(Some(slot.name.clone()));
        }
        Ok(None)
    }
}

fn create_ring(
    device: &ID3D11Device,
    w: u32,
    h: u32,
    name_prefix: &str,
    security: Option<&SECURITY_ATTRIBUTES>,
) -> Result<Vec<RingSlot>> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
            as u32,
    };
    let mut ring = Vec::with_capacity(RING_SIZE);
    for i in 0..RING_SIZE {
        let name = format!("{name_prefix}-{i}");
        let mut tex: Option<ID3D11Texture2D> = None;
        // SAFETY: valid descriptor and out pointer.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }
            .map_err(|e| os_err("CreateTexture2D (shared ring)", &e))?;
        let tex = tex.ok_or_else(|| PlatformError::other("CreateTexture2D returned null"))?;
        let resource: IDXGIResource1 = tex.cast().map_err(|e| os_err("IDXGIResource1", &e))?;
        let wname = wide(&name);
        // SAFETY: `wname` is NUL-terminated and outlives the call; the attributes are borrowed.
        let handle = unsafe {
            resource.CreateSharedHandle(
                security.map(std::ptr::from_ref),
                SHARED_ACCESS,
                PCWSTR(wname.as_ptr()),
            )
        }
        .map_err(|e| os_err("CreateSharedHandle", &e))?;
        let mutex: IDXGIKeyedMutex = match tex.cast() {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: just created above, not stored anywhere.
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(os_err("IDXGIKeyedMutex", &e));
            }
        };
        ring.push(RingSlot {
            tex,
            mutex,
            handle,
            name,
        });
    }
    Ok(ring)
}

// ---------------------------------------------------------------------------------------------
// Consumer (agent)
// ---------------------------------------------------------------------------------------------

/// Result of [`SecureConsumer::copy_to`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    /// The texture holds no unread picture right now (the helper reclaimed or is rewriting it);
    /// ask again.
    NotReady,
}

struct Opened {
    name: String,
    tex: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
}

/// `DeskBridgeSecure-<pid>-<epoch>` of a ring texture name; changes whenever the helper re-creates
/// its duplication and with it every texture.
fn epoch_of(name: &str) -> &str {
    name.rsplit_once('-').map_or(name, |(epoch, _)| epoch)
}

/// Opens the helper's shared textures on the agent's device and reads them.
pub struct SecureConsumer {
    device1: ID3D11Device1,
    context: ID3D11DeviceContext,
    epoch: String,
    opened: Vec<Opened>,
}

impl SecureConsumer {
    /// `device` must live on the adapter the helper reported (same LUID), or opening fails.
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self> {
        let device1: ID3D11Device1 = device
            .cast()
            .map_err(|e| os_err("ID3D11Device1 (Windows 8+ required)", &e))?;
        Ok(Self {
            device1,
            context: context.clone(),
            epoch: String::new(),
            opened: Vec::new(),
        })
    }

    fn open(&mut self, name: &str) -> Result<usize> {
        if let Some(i) = self.opened.iter().position(|o| o.name == name) {
            return Ok(i);
        }
        let wname = wide(name);
        // SAFETY: `wname` is NUL-terminated and outlives the call.
        let tex: ID3D11Texture2D = unsafe {
            self.device1
                .OpenSharedResourceByName(PCWSTR(wname.as_ptr()), SHARED_ACCESS)
        }
        .map_err(|e| os_err("OpenSharedResourceByName", &e))?;
        let mutex: IDXGIKeyedMutex = tex.cast().map_err(|e| os_err("IDXGIKeyedMutex", &e))?;
        self.opened.push(Opened {
            name: name.to_string(),
            tex,
            mutex,
        });
        Ok(self.opened.len() - 1)
    }

    /// Copies the picture in the shared texture `name` into `dst` (same size, BGRA). Textures are
    /// opened once and cached until the epoch in the name changes.
    pub fn copy_to(
        &mut self,
        name: &str,
        dst: &ID3D11Texture2D,
        size: (u32, u32),
        wait_ms: u32,
    ) -> Result<CopyOutcome> {
        let epoch = epoch_of(name);
        if self.epoch != epoch {
            self.opened.clear();
            self.epoch = epoch.to_string();
        }
        let i = self.open(name)?;
        let r = self.copy_opened(i, dst, size, wait_ms);
        if r.is_err() {
            // Whatever is wrong with this texture, open it afresh next time.
            self.opened.remove(i);
        }
        r
    }

    fn copy_opened(
        &self,
        i: usize,
        dst: &ID3D11Texture2D,
        size: (u32, u32),
        wait_ms: u32,
    ) -> Result<CopyOutcome> {
        let o = &self.opened[i];
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid out pointer.
        unsafe { o.tex.GetDesc(&mut desc) };
        if (desc.Width, desc.Height) != size {
            return Err(PlatformError::other(format!(
                "shared texture is {}x{}, expected {}x{}",
                desc.Width, desc.Height, size.0, size.1
            )));
        }
        let src: ID3D11Resource = o
            .tex
            .cast()
            .map_err(|e| os_err("texture -> resource", &e))?;
        let dst: ID3D11Resource = dst.cast().map_err(|e| os_err("texture -> resource", &e))?;
        if acquire_sync(&o.mutex, 1, wait_ms)? == Acquired::TimedOut {
            return Ok(CopyOutcome::NotReady);
        }
        // SAFETY: both resources live on this device and have the same size and format; the
        // mutex is held so the helper cannot be writing.
        unsafe { self.context.CopyResource(&dst, &src) };
        release_sync(&o.mutex, 0)?;
        Ok(CopyOutcome::Copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::common::{D3dDevice, adapter_by_luid};

    fn out(a: u32, o: u32, x: i32, w: u32, attached: bool, software: bool) -> OutputInfo {
        OutputInfo {
            adapter_index: a,
            output_index: o,
            adapter_name: "gpu".into(),
            adapter_luid: 1,
            device_name: format!("\\\\.\\DISPLAY{}", o + 1),
            x,
            y: 0,
            w,
            h: 1080,
            attached,
            software,
        }
    }

    #[test]
    fn epoch_is_the_name_without_the_slot() {
        assert_eq!(
            epoch_of("DeskBridgeSecure-4242-3-2"),
            "DeskBridgeSecure-4242-3"
        );
        assert_eq!(epoch_of("plain"), "plain");
    }

    #[test]
    fn output_choice_prefers_the_rectangle_then_any_real_output() {
        let v = vec![
            out(0, 0, 0, 1920, true, true),
            out(0, 1, 0, 1920, true, false),
            out(0, 2, 1920, 2560, true, false),
        ];
        assert_eq!(
            pick_output(&v, (1920, 0, 2560, 1080)).unwrap().output_index,
            2
        );
        // unknown rectangle: first real attached output
        assert_eq!(pick_output(&v, (9000, 0, 10, 10)).unwrap().output_index, 1);
        assert!(pick_output(&[out(0, 0, 0, 100, false, false)], (0, 0, 100, 1080)).is_none());
    }

    /// Producer and consumer in one process on the user's own desktop: exercises the real
    /// duplication, named shared handles and the keyed-mutex hand-over. Skipped on machines
    /// without an interactive GPU session.
    #[test]
    fn loopback_hands_a_picture_over_by_name() {
        let _one_at_a_time = crate::video::common::DUPLICATION_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prefix = format!("DeskBridgeSecureTest-{}", std::process::id());
        let mut producer = match SecureProducer::open((0, 0, 1, 1), &prefix, None) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skipped: cannot duplicate an output here: {e}");
                return;
            }
        };
        let info = producer.info();
        let adapter = adapter_by_luid(info.luid).expect("adapter of the producer");
        let D3dDevice {
            device, context, ..
        } = create_device(Some(&adapter)).expect("consumer device");
        let mut consumer = SecureConsumer::new(&device, &context).expect("consumer");

        let pool = {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: info.w,
                Height: info.h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            // SAFETY: valid descriptor and out pointer.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }.unwrap();
            t.unwrap()
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut name = None;
        while name.is_none() && std::time::Instant::now() < deadline {
            match producer.pump(100) {
                Ok(Pump::Update { image: Some(n), .. }) => name = Some(n),
                Ok(Pump::Lost) => {
                    eprintln!("skipped: duplication lost");
                    return;
                }
                Ok(_) => {}
                Err(e) => panic!("pump: {e}"),
            }
        }
        let name = name.expect("a picture within 5 s");
        assert!(name.starts_with(&prefix));
        let copied = consumer
            .copy_to(&name, &pool, (info.w, info.h), 1000)
            .expect("copy");
        assert_eq!(copied, CopyOutcome::Copied);
        // the picture was consumed (key back to 0): a second read has nothing to take
        assert_eq!(
            consumer.copy_to(&name, &pool, (info.w, info.h), 0).unwrap(),
            CopyOutcome::NotReady
        );
    }
}
