//! The set of OS services the engine runs on. Windows builds fill it with
//! `deskbridge-platform-windows`, tests with the mocks.

use deskbridge_display::mock::MockDisplayManager;
use deskbridge_platform::audio::{
    AudioCapture, AudioDevices, AudioPlayback, DeviceKind, LocalOutput, VirtualAudio,
};
use deskbridge_platform::clipboard::ClipboardBackend;
use deskbridge_platform::display::DisplayManager;
use deskbridge_platform::input::{InputCapture, InputInjector};
use deskbridge_platform::mock::{
    MockAudioCapture, MockAudioDevices, MockAudioPlayback, MockCapture, MockClipboard,
    MockInjector, MockLocalOutput, MockScreens, MockVirtualAudio, MockWebClient, audio_device,
    screen,
};
use deskbridge_platform::power::{PowerEvent, PowerManager};
use deskbridge_platform::screen::ScreenProvider;
use deskbridge_platform::secret::{PlainProtector, SecretProtector};
use deskbridge_platform::system::{NullSystemServices, SystemServices};
use deskbridge_platform::web::WebClient;
use deskbridge_platform::{Result, UnboundedSender};
use deskbridge_video::backend::{VideoBackend, mock::MockVideoBackend};
use std::sync::Arc;

#[derive(Clone)]
pub struct Platform {
    pub capture: Arc<dyn InputCapture>,
    pub injector: Arc<dyn InputInjector>,
    pub screens: Arc<dyn ScreenProvider>,
    pub clipboard: Arc<dyn ClipboardBackend>,
    pub power: Arc<dyn PowerManager>,
    pub secrets: Arc<dyn SecretProtector>,
    pub audio_devices: Arc<dyn AudioDevices>,
    pub audio_capture: Arc<dyn AudioCapture>,
    pub audio_playback: Arc<dyn AudioPlayback>,
    pub virtual_audio: Arc<dyn VirtualAudio>,
    pub local_output: Arc<dyn LocalOutput>,
    pub system: Arc<dyn SystemServices>,
    pub video: Arc<dyn VideoBackend>,
    pub display: Arc<dyn DisplayManager>,
    /// HTTPS GET for the release check.
    pub web: Arc<dyn WebClient>,
    /// OS name reported in `Hello`.
    pub os: String,
}

/// Power manager that never fires (tests).
#[derive(Default)]
pub struct NullPower;

impl PowerManager for NullPower {
    fn watch(&self, _tx: UnboundedSender<PowerEvent>) -> Result<()> {
        Ok(())
    }

    fn set_lid_action_do_nothing(&self, _on: bool) -> Result<()> {
        Ok(())
    }

    fn set_keep_awake(&self, _on: bool) {}
}

/// Handles to the mocks inside a mock [`Platform`], so tests can drive and inspect them.
pub struct MockHandles {
    pub capture: Arc<MockCapture>,
    pub injector: Arc<MockInjector>,
    pub screens: Arc<MockScreens>,
    pub clipboard: Arc<MockClipboard>,
    pub audio_devices: Arc<MockAudioDevices>,
    pub audio_capture: Arc<MockAudioCapture>,
    pub audio_playback: Arc<MockAudioPlayback>,
    pub virtual_audio: Arc<MockVirtualAudio>,
    pub local_output: Arc<MockLocalOutput>,
    pub video: Arc<MockVideoBackend>,
    pub display: Arc<MockDisplayManager>,
    pub web: Arc<MockWebClient>,
}

impl Platform {
    /// Fully mocked platform with one 1920×1080 screen.
    #[must_use]
    pub fn mock(width: u32, height: u32) -> (Self, MockHandles) {
        let capture = Arc::new(MockCapture::default());
        let injector = Arc::new(MockInjector::default());
        let mut s = screen(0, 0, 0, width, height);
        s.primary = true;
        let screens = Arc::new(MockScreens::with(vec![s]));
        let clipboard = Arc::new(MockClipboard::default());
        let audio_devices = Arc::new(MockAudioDevices::with(vec![
            audio_device("out1", "Headset (Mock)", DeviceKind::Render, true),
            audio_device("out2", "Lautsprecher (Mock)", DeviceKind::Render, false),
            audio_device("mic1", "Headset-Mikro (Mock)", DeviceKind::Capture, true),
        ]));
        let audio_capture = Arc::new(MockAudioCapture::default());
        let audio_playback = Arc::new(MockAudioPlayback::default());
        let virtual_audio = Arc::new(MockVirtualAudio::default());
        let local_output = Arc::new(MockLocalOutput::default());
        let video = Arc::new(MockVideoBackend::new((64, 36)));
        let display = Arc::new(MockDisplayManager::new());
        let web = Arc::new(MockWebClient::default());
        let p = Self {
            capture: capture.clone(),
            injector: injector.clone(),
            screens: screens.clone(),
            clipboard: clipboard.clone(),
            power: Arc::new(NullPower),
            secrets: Arc::new(PlainProtector),
            audio_devices: audio_devices.clone(),
            audio_capture: audio_capture.clone(),
            audio_playback: audio_playback.clone(),
            virtual_audio: virtual_audio.clone(),
            local_output: local_output.clone(),
            system: Arc::new(NullSystemServices),
            video: video.clone(),
            display: display.clone(),
            web: web.clone(),
            os: "mock".into(),
        };
        (
            p,
            MockHandles {
                capture,
                injector,
                screens,
                clipboard,
                audio_devices,
                audio_capture,
                audio_playback,
                virtual_audio,
                local_output,
                video,
                display,
                web,
            },
        )
    }
}
