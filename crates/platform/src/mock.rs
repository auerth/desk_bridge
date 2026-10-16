//! In-memory implementations of the platform traits for tests (no hardware needed).

use crate::clipboard::{
    ClipFormatInfo, ClipboardBackend, ClipboardEvent, ClipboardProvider, FileOffer,
    VirtualFileProvider,
};
use crate::hotkey::{Hotkey, HotkeyMatcher, HotkeyOutcome};
use crate::input::{Button, CaptureEvent, GrabMode, InputCapture, InputInjector};
use crate::screen::{Point, ScreenInfo, ScreenProvider};
use crate::{Result, UnboundedSender};
use parking_lot::Mutex;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

/// What a [`MockInjector`] was asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum InjectedOp {
    Key { hid: u16, down: bool },
    MouseRel { dx: i32, dy: i32 },
    MouseAbs { x: i32, y: i32 },
    Button { button: Button, down: bool },
    Wheel { dx: i32, dy: i32 },
}

#[derive(Default)]
pub struct MockInjector {
    ops: Mutex<Vec<InjectedOp>>,
    keys: Mutex<BTreeSet<u16>>,
    buttons: Mutex<Vec<Button>>,
    toggles: Mutex<(bool, bool, bool)>,
    pub release_all_calls: Mutex<u32>,
}

impl MockInjector {
    #[must_use]
    pub fn ops(&self) -> Vec<InjectedOp> {
        self.ops.lock().clone()
    }

    pub fn clear(&self) {
        self.ops.lock().clear();
    }

    #[must_use]
    pub fn pressed_keys(&self) -> Vec<u16> {
        self.keys.lock().iter().copied().collect()
    }

    pub fn set_toggles(&self, caps: bool, num: bool, scroll: bool) {
        *self.toggles.lock() = (caps, num, scroll);
    }
}

impl InputInjector for MockInjector {
    fn key(&self, hid: u16, down: bool) {
        if down {
            self.keys.lock().insert(hid);
        } else {
            self.keys.lock().remove(&hid);
        }
        self.ops.lock().push(InjectedOp::Key { hid, down });
    }

    fn mouse_rel(&self, dx: i32, dy: i32) {
        self.ops.lock().push(InjectedOp::MouseRel { dx, dy });
    }

    fn mouse_abs_px(&self, x: i32, y: i32) {
        self.ops.lock().push(InjectedOp::MouseAbs { x, y });
    }

    fn button(&self, button: Button, down: bool) {
        let mut b = self.buttons.lock();
        if down {
            if !b.contains(&button) {
                b.push(button);
            }
        } else {
            b.retain(|x| *x != button);
        }
        drop(b);
        self.ops.lock().push(InjectedOp::Button { button, down });
    }

    fn wheel(&self, dx: i32, dy: i32) {
        self.ops.lock().push(InjectedOp::Wheel { dx, dy });
    }

    fn release_all(&self) {
        *self.release_all_calls.lock() += 1;
        let keys: Vec<u16> = self.keys.lock().iter().copied().collect();
        for k in keys {
            self.key(k, false);
        }
        let buttons: Vec<Button> = self.buttons.lock().clone();
        for b in buttons {
            self.button(b, false);
        }
    }

    fn toggles(&self) -> (bool, bool, bool) {
        *self.toggles.lock()
    }
}

/// Input capture driven by the test: call [`MockCapture::feed`].
pub struct MockCapture {
    sink: Mutex<Option<UnboundedSender<CaptureEvent>>>,
    matcher: Mutex<HotkeyMatcher>,
    pub grab: Mutex<GrabMode>,
    pub cursor: Mutex<Point>,
    pub cursor_visible: Mutex<bool>,
}

impl Default for MockCapture {
    fn default() -> Self {
        Self {
            sink: Mutex::new(None),
            matcher: Mutex::new(HotkeyMatcher::default()),
            grab: Mutex::new(GrabMode::None),
            cursor: Mutex::new(Point::default()),
            cursor_visible: Mutex::new(true),
        }
    }
}

impl MockCapture {
    /// Simulates a physical event, honouring hotkeys like the real hook.
    pub fn feed(&self, ev: CaptureEvent) {
        let Some(sink) = self.sink.lock().clone() else { return };
        if let CaptureEvent::Key { hid, down } = ev {
            match self.matcher.lock().feed(hid, down) {
                HotkeyOutcome::Fired(id) => {
                    let _ = sink.send(CaptureEvent::Hotkey(id));
                    return;
                }
                HotkeyOutcome::Swallow => return,
                HotkeyOutcome::Pass => {}
            }
        }
        let _ = sink.send(ev);
    }
}

impl InputCapture for MockCapture {
    fn start(&self, sink: UnboundedSender<CaptureEvent>) -> Result<()> {
        *self.sink.lock() = Some(sink);
        Ok(())
    }

    fn stop(&self) {
        *self.sink.lock() = None;
    }

    fn set_grab(&self, mode: GrabMode) {
        *self.grab.lock() = mode;
    }

    fn set_hotkeys(&self, hotkeys: Vec<Hotkey>) {
        self.matcher.lock().set_hotkeys(hotkeys);
    }

    fn set_cursor_pos(&self, x: i32, y: i32) {
        *self.cursor.lock() = Point { x, y };
    }

    fn set_cursor_visible(&self, visible: bool) {
        *self.cursor_visible.lock() = visible;
    }
}

#[derive(Default)]
pub struct MockScreens {
    screens: Mutex<Vec<ScreenInfo>>,
    cursor: Mutex<Point>,
    watchers: Mutex<Vec<UnboundedSender<()>>>,
}

impl MockScreens {
    #[must_use]
    pub fn with(screens: Vec<ScreenInfo>) -> Self {
        Self { screens: Mutex::new(screens), ..Self::default() }
    }

    pub fn set(&self, screens: Vec<ScreenInfo>) {
        *self.screens.lock() = screens;
        self.watchers.lock().retain(|w| w.send(()).is_ok());
    }

    pub fn set_cursor(&self, p: Point) {
        *self.cursor.lock() = p;
    }
}

impl ScreenProvider for MockScreens {
    fn screens(&self) -> Vec<ScreenInfo> {
        self.screens.lock().clone()
    }

    fn cursor_pos(&self) -> Point {
        *self.cursor.lock()
    }

    fn watch(&self, tx: UnboundedSender<()>) -> Result<()> {
        self.watchers.lock().push(tx);
        Ok(())
    }
}

/// Convenience constructor for a screen.
#[must_use]
pub fn screen(id: u32, x: i32, y: i32, w: u32, h: u32) -> ScreenInfo {
    ScreenInfo {
        id,
        x,
        y,
        w,
        h,
        dpi: 96,
        refresh_mhz: 60_000,
        primary: id == 0,
        name: format!("Screen {id}"),
    }
}

/// In-memory clipboard. `user_copy_*` simulates the user copying something locally.
#[derive(Default)]
pub struct MockClipboard {
    tx: Mutex<Option<UnboundedSender<ClipboardEvent>>>,
    pub content: Mutex<Vec<(String, Vec<u8>)>>,
    pub offered: Mutex<Vec<ClipFormatInfo>>,
    provider: Mutex<Option<Arc<dyn ClipboardProvider>>>,
    pub offered_files: Mutex<Vec<FileOffer>>,
    pub local_files: Mutex<Vec<PathBuf>>,
    pub file_provider: Mutex<Option<Arc<dyn VirtualFileProvider>>>,
}

impl MockClipboard {
    pub fn user_copy_text(&self, text: &str) {
        *self.content.lock() = vec![("text/plain".into(), text.as_bytes().to_vec())];
        if let Some(tx) = self.tx.lock().as_ref() {
            let _ = tx.send(ClipboardEvent::Changed {
                formats: vec![ClipFormatInfo { mime: "text/plain".into(), size: text.len() as u64 }],
                sensitive: false,
                text: Some(text.to_string()),
                files: vec![],
            });
        }
    }

    pub fn user_paste(&self, mime: &str) -> Option<Vec<u8>> {
        if let Some(d) = self.content.lock().iter().find(|(m, _)| m == mime) {
            return Some(d.1.clone());
        }
        let p = self.provider.lock().clone()?;
        p.fetch(mime).ok()
    }
}

impl ClipboardBackend for MockClipboard {
    fn start(&self, tx: UnboundedSender<ClipboardEvent>) -> Result<()> {
        *self.tx.lock() = Some(tx);
        Ok(())
    }

    fn stop(&self) {
        *self.tx.lock() = None;
    }

    fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.content.lock().iter().find(|(m, _)| m == mime).map(|(_, d)| d.clone()))
    }

    fn set_text(&self, text: &str) -> Result<()> {
        *self.content.lock() = vec![("text/plain".into(), text.as_bytes().to_vec())];
        Ok(())
    }

    fn offer(&self, formats: Vec<ClipFormatInfo>, provider: Arc<dyn ClipboardProvider>) -> Result<()> {
        self.content.lock().clear();
        *self.offered.lock() = formats;
        *self.provider.lock() = Some(provider);
        Ok(())
    }

    fn offer_files(&self, files: Vec<FileOffer>, provider: Arc<dyn VirtualFileProvider>) -> Result<()> {
        *self.offered_files.lock() = files;
        *self.file_provider.lock() = Some(provider);
        Ok(())
    }

    fn offer_local_files(&self, paths: Vec<PathBuf>) -> Result<()> {
        *self.local_files.lock() = paths;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_all_clears_everything() {
        let inj = MockInjector::default();
        inj.key(4, true);
        inj.key(0xE1, true);
        inj.button(Button::Left, true);
        inj.release_all();
        assert!(inj.pressed_keys().is_empty());
        assert!(inj.ops().contains(&InjectedOp::Button { button: Button::Left, down: false }));
    }
}
