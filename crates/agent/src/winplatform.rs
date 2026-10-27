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
///
/// "CABLE Output" is switched on/off through the service (LocalSystem, also keeps it off while
/// the agent is not running); without the service (dev runs, older service) directly.
fn virtual_audio() -> Arc<dyn deskbridge_platform::audio::VirtualAudio> {
    let service = ServiceClient::new();
    Arc::new(CableVirtualAudio::with_endpoint_switch(Arc::new(
        move |enabled, managed| match service.set_cable_mic(enabled, managed) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!("cable mic via service failed ({e}); switching directly");
                deskbridge_platform_windows::audio::set_cable_mic_enabled(enabled || !managed)
            }
        },
    )))
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
        // UAC, lock screen and logon are captured by the SYSTEM helper and handed over as shared
        // textures; without a (current) helper the viewer keeps the "interruption" placeholder.
        video: Arc::new(
            deskbridge_video::WindowsVideoBackend::new()
                .with_secure_source(Arc::new(crate::secure::HelperFrameSource::new())),
        ),
        display: Arc::new(
            deskbridge_platform_windows::display::WinDisplayManager::new()
                .with_activation_fallback(Arc::new(crate::secure::show_real_displays_via_helper)),
        ),
        web: Arc::new(deskbridge_platform_windows::web::WinHttpClient::new(
            format!("DeskBridge/{}", deskbridge_core::brand::VERSION),
        )),
        os: "Windows".to_string(),
    }
}

/// Platform of the agent at the logon screen (ADR 0008). The same backends: the engine never
/// starts clipboard, audio, focus or capture there (`EngineParams::logon_screen`), and input goes
/// through the secure-desktop helper because the logon screen is on `winsta0\Winlogon`. Only the
/// identity copy differs: it is encrypted for the machine, SYSTEM has no user DPAPI key.
pub fn build_logon() -> Platform {
    Platform {
        secrets: Arc::new(DpapiProtector::machine()),
        ..build()
    }
}
