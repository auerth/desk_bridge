//! Assembles the Windows implementations into the engine's [`Platform`].

use crate::secure::{SecureDesktopInjector, ServiceClient};
use deskbridge_core::platform::Platform;
use deskbridge_platform_windows::{
    audio::{CableVirtualAudio, WasapiCapture, WasapiDevices, WasapiPlayback, WinLocalOutput},
    clipboard::WinClipboard,
    input::{WinInputCapture, WinInputInjector},
    power::WinPowerManager,
    screen::WinScreenProvider,
    secret::DpapiProtector,
};
use std::sync::Arc;

/// A virtual audio cable is used when its (Microsoft-signed) driver is installed, otherwise the
/// microphone feature honestly reports "not available" (ADR 0003). The cable backend checks for
/// the driver on every call, so a cable installed (or enumerated late at logon) after the agent
/// started is picked up with the next connection.
fn virtual_audio() -> Arc<dyn deskbridge_platform::audio::VirtualAudio> {
    Arc::new(CableVirtualAudio::new())
}

pub fn build() -> Platform {
    Platform {
        capture: Arc::new(WinInputCapture::new()),
        injector: Arc::new(SecureDesktopInjector::new(
            Arc::new(WinInputInjector::new()),
        )),
        screens: Arc::new(WinScreenProvider::new()),
        clipboard: Arc::new(WinClipboard::new()),
        power: Arc::new(WinPowerManager::new()),
        secrets: Arc::new(DpapiProtector::new()),
        audio_devices: Arc::new(WasapiDevices::new()),
        audio_capture: Arc::new(WasapiCapture::new()),
        audio_playback: Arc::new(WasapiPlayback::new()),
        virtual_audio: virtual_audio(),
        local_output: Arc::new(WinLocalOutput::new()),
        system: Arc::new(ServiceClient::new()),
        video: Arc::new(deskbridge_video::WindowsVideoBackend::new()),
        display: Arc::new(deskbridge_platform_windows::display::WinDisplayManager::new()),
        os: "Windows".to_string(),
    }
}
