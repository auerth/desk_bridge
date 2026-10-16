//! Assembles the Windows implementations into the engine's [`Platform`].

use deskbridge_core::platform::Platform;
use deskbridge_platform_windows::{
    audio::{VirtualAudioStub, WasapiCapture, WasapiDevices, WasapiPlayback},
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
        audio_devices: Arc::new(WasapiDevices::new()),
        audio_capture: Arc::new(WasapiCapture::new()),
        audio_playback: Arc::new(WasapiPlayback::new()),
        virtual_audio: Arc::new(VirtualAudioStub::default()),
        system: Arc::new(deskbridge_platform::system::NullSystemServices),
        os: "Windows".to_string(),
    }
}
