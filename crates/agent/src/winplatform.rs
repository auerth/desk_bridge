//! Assembles the Windows implementations into the engine's [`Platform`].

use deskbridge_core::platform::Platform;
use deskbridge_platform_windows::{
    clipboard::WinClipboard,
    input::{WinInputCapture, WinInputInjector},
    power::WinPowerManager,
    screen::WinScreenProvider,
    secret::DpapiProtector,
};
use std::sync::Arc;

pub fn build() -> Platform {
    Platform {
        capture: Arc::new(WinInputCapture::new()),
        injector: Arc::new(WinInputInjector::new()),
        screens: Arc::new(WinScreenProvider::new()),
        clipboard: Arc::new(WinClipboard::new()),
        power: Arc::new(WinPowerManager::new()),
        secrets: Arc::new(DpapiProtector::new()),
        os: "Windows".to_string(),
    }
}
