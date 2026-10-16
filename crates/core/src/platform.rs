//! The set of OS services the engine runs on. Windows builds fill it with
//! `deskbridge-platform-windows`, tests with the mocks.

use deskbridge_platform::clipboard::ClipboardBackend;
use deskbridge_platform::input::{InputCapture, InputInjector};
use deskbridge_platform::mock::{MockCapture, MockClipboard, MockInjector, MockScreens, screen};
use deskbridge_platform::power::{PowerEvent, PowerManager};
use deskbridge_platform::screen::ScreenProvider;
use deskbridge_platform::secret::{PlainProtector, SecretProtector};
use deskbridge_platform::{Result, UnboundedSender};
use std::sync::Arc;

#[derive(Clone)]
pub struct Platform {
    pub capture: Arc<dyn InputCapture>,
    pub injector: Arc<dyn InputInjector>,
    pub screens: Arc<dyn ScreenProvider>,
    pub clipboard: Arc<dyn ClipboardBackend>,
    pub power: Arc<dyn PowerManager>,
    pub secrets: Arc<dyn SecretProtector>,
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
        let p = Self {
            capture: capture.clone(),
            injector: injector.clone(),
            screens: screens.clone(),
            clipboard: clipboard.clone(),
            power: Arc::new(NullPower),
            secrets: Arc::new(PlainProtector),
            os: "mock".into(),
        };
        (p, MockHandles { capture, injector, screens, clipboard })
    }
}
