//! Webcam forwarding (see `deskbridge_platform::camera` and `deskbridge-camera`).

mod capture;
pub mod register;
mod usage;
mod virtual_cam;

pub use capture::WinCameraCapture;
pub use usage::camera_app;
pub use virtual_cam::WinVirtualCamera;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// COM (MTA) for the guard's lifetime; nothing is uninitialised if the thread had COM already.
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub(crate) fn init() -> Self {
        // SAFETY: balanced by `CoUninitialize` in `Drop` only when it succeeded.
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `CoInitializeEx`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Monotonic microseconds (picture timestamps).
pub(crate) fn now_us() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}
