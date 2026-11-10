//! Webcam forwarding: the PC's webcam appears on the laptop as "Desk Bridge Kamera".
//!
//! * [`CameraCapture`]: the real webcam on the device that sends (the PC).
//! * [`VirtualCamera`]: the virtual camera on the device that receives (the laptop). It reports
//!   when an app streams from it ([`CameraUsage`]); only then does the PC open its webcam.

use crate::audio::StreamHandle;
use crate::video::Frame;
use crate::{Result, UnboundedSender};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDeviceInfo {
    /// Stable id (symbolic link of the device).
    pub id: String,
    pub name: String,
}

pub trait CameraCapture: Send + Sync {
    /// Physical webcams (virtual cameras, including our own, are left out).
    fn list(&self) -> Result<Vec<CameraDeviceInfo>>;
    /// Opens the webcam (`None` = the first one) and sends BGRA pictures of about `width` x
    /// `height` at `fps` to `sink` (the size the camera offers that comes closest; the encoder
    /// scales the rest). Dropping the handle closes the webcam.
    fn start(
        &self,
        device: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        sink: UnboundedSender<Frame>,
    ) -> Result<Box<dyn StreamHandle>>;
}

/// What the virtual camera shows while no pictures arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraFeedState {
    /// No partner connected (placeholder, grey dot).
    Idle,
    /// The partner's webcam is starting (placeholder, orange dot).
    Waiting,
    /// Pictures are flowing.
    Streaming,
    /// The partner has no webcam or it is switched off there.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraUsage {
    pub in_use: bool,
    /// Size and rate the app chose (0 while not in use).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The app, if it can be found out ("ms-teams.exe").
    pub process: Option<String>,
}

pub trait VirtualCamera: Send + Sync {
    /// The virtual camera exists on this machine (Windows 11 and registered).
    fn available(&self) -> bool;
    /// Why it is not available (shown in the UI), `None` if it is.
    fn unavailable_reason(&self) -> Option<String> {
        None
    }
    /// Shows or hides the camera for apps (like enabling/disabling a device).
    fn set_enabled(&self, _on: bool) -> Result<()> {
        Err(crate::PlatformError::Unsupported(
            "virtual camera switching",
        ))
    }
    fn set_feed_state(&self, state: CameraFeedState);
    /// Hands one picture to the camera; it is converted to the size the app chose.
    fn write_frame(&self, frame: &Frame);
    /// Current usage.
    fn usage(&self) -> CameraUsage;
    /// Usage changes (an app opened or closed the camera, or chose another size).
    fn watch_usage(&self, tx: UnboundedSender<CameraUsage>) -> Result<()>;
}

/// No camera support.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullCameraCapture;

impl CameraCapture for NullCameraCapture {
    fn list(&self) -> Result<Vec<CameraDeviceInfo>> {
        Ok(vec![])
    }

    fn start(
        &self,
        _device: Option<&str>,
        _width: u32,
        _height: u32,
        _fps: u32,
        _sink: UnboundedSender<Frame>,
    ) -> Result<Box<dyn StreamHandle>> {
        Err(crate::PlatformError::Unsupported("webcam capture"))
    }
}

/// Test webcam: one device, synthetic BGRA pictures (blue channel = picture counter).
#[derive(Default)]
pub struct MockCameraCapture {
    pub open: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub opened_count: std::sync::atomic::AtomicU32,
    /// Size and rate of the last `start`.
    pub last_request: parking_lot::Mutex<Option<(u32, u32, u32)>>,
    /// No webcam attached.
    pub none: std::sync::atomic::AtomicBool,
}

struct MockCameraHandle(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl StreamHandle for MockCameraHandle {}

impl Drop for MockCameraHandle {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl CameraCapture for MockCameraCapture {
    fn list(&self) -> Result<Vec<CameraDeviceInfo>> {
        if self.none.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(vec![]);
        }
        Ok(vec![CameraDeviceInfo {
            id: "cam1".into(),
            name: "Webcam (Mock)".into(),
        }])
    }

    fn start(
        &self,
        _device: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        sink: UnboundedSender<Frame>,
    ) -> Result<Box<dyn StreamHandle>> {
        use std::sync::atomic::Ordering;
        if self.none.load(Ordering::SeqCst) {
            return Err(crate::PlatformError::DeviceNotFound("Webcam".into()));
        }
        *self.last_request.lock() = Some((width, height, fps));
        self.opened_count.fetch_add(1, Ordering::SeqCst);
        let open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.open.store(true, Ordering::SeqCst);
        let flag = open.clone();
        let shared = self.open.clone();
        let (w, h) = (width.clamp(2, 64) & !1, height.clamp(2, 36) & !1);
        std::thread::spawn(move || {
            let mut n = 0u64;
            while flag.load(Ordering::SeqCst) {
                let mut data = vec![0u8; (w * h * 4) as usize];
                for px in data.chunks_exact_mut(4) {
                    px.copy_from_slice(&[(n % 251) as u8, 80, 160, 255]);
                }
                let f = Frame {
                    width: w,
                    height: h,
                    ts_us: n * 33_000,
                    data: crate::video::FrameData::Bgra {
                        data,
                        stride: (w * 4) as usize,
                    },
                };
                if sink.send(f).is_err() {
                    break;
                }
                n += 1;
                std::thread::sleep(std::time::Duration::from_millis(33));
            }
            shared.store(false, Ordering::SeqCst);
        });
        Ok(Box::new(MockCameraHandle(open)))
    }
}

/// Test virtual camera: usage is driven by the test, pictures are counted.
#[derive(Default)]
pub struct MockVirtualCamera {
    pub unavailable: std::sync::atomic::AtomicBool,
    pub frames: std::sync::atomic::AtomicU64,
    pub feed: parking_lot::Mutex<Option<CameraFeedState>>,
    pub enabled: parking_lot::Mutex<Option<bool>>,
    usage: parking_lot::Mutex<Option<CameraUsage>>,
    watchers: parking_lot::Mutex<Vec<UnboundedSender<CameraUsage>>>,
}

impl MockVirtualCamera {
    /// Simulates an app opening (`Some(w, h)`) or closing (`None`) the camera.
    pub fn set_in_use(&self, size: Option<(u32, u32)>) {
        let u = CameraUsage {
            in_use: size.is_some(),
            width: size.map_or(0, |s| s.0),
            height: size.map_or(0, |s| s.1),
            fps: if size.is_some() { 30 } else { 0 },
            process: size.map(|_| "ms-teams.exe".to_string()),
        };
        *self.usage.lock() = Some(u.clone());
        self.watchers.lock().retain(|w| w.send(u.clone()).is_ok());
    }
}

impl VirtualCamera for MockVirtualCamera {
    fn available(&self) -> bool {
        !self.unavailable.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn set_enabled(&self, on: bool) -> Result<()> {
        *self.enabled.lock() = Some(on);
        Ok(())
    }

    fn set_feed_state(&self, state: CameraFeedState) {
        *self.feed.lock() = Some(state);
    }

    fn write_frame(&self, _frame: &Frame) {
        self.frames
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn usage(&self) -> CameraUsage {
        self.usage.lock().clone().unwrap_or(CameraUsage {
            in_use: false,
            width: 0,
            height: 0,
            fps: 0,
            process: None,
        })
    }

    fn watch_usage(&self, tx: UnboundedSender<CameraUsage>) -> Result<()> {
        self.watchers.lock().push(tx);
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NullVirtualCamera;

impl VirtualCamera for NullVirtualCamera {
    fn available(&self) -> bool {
        false
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some("Virtuelle Kamera auf diesem System nicht verfügbar".into())
    }

    fn set_feed_state(&self, _state: CameraFeedState) {}

    fn write_frame(&self, _frame: &Frame) {}

    fn usage(&self) -> CameraUsage {
        CameraUsage {
            in_use: false,
            width: 0,
            height: 0,
            fps: 0,
            process: None,
        }
    }

    fn watch_usage(&self, _tx: UnboundedSender<CameraUsage>) -> Result<()> {
        Ok(())
    }
}
