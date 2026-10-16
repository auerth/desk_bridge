//! In-memory implementations of the platform traits for tests (no hardware needed).

use crate::audio::{
    AudioCapture, AudioDeviceInfo, AudioDevices, AudioFrame, AudioPlayback, CaptureSource,
    DeviceEvent, DeviceKind, MicUsage, PullSource, StreamHandle, VirtualAudio,
};
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
        let Some(sink) = self.sink.lock().clone() else {
            return;
        };
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
        Self {
            screens: Mutex::new(screens),
            ..Self::default()
        }
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
                formats: vec![ClipFormatInfo {
                    mime: "text/plain".into(),
                    size: text.len() as u64,
                }],
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
        Ok(self
            .content
            .lock()
            .iter()
            .find(|(m, _)| m == mime)
            .map(|(_, d)| d.clone()))
    }

    fn set_text(&self, text: &str) -> Result<()> {
        *self.content.lock() = vec![("text/plain".into(), text.as_bytes().to_vec())];
        Ok(())
    }

    fn offer(
        &self,
        formats: Vec<ClipFormatInfo>,
        provider: Arc<dyn ClipboardProvider>,
    ) -> Result<()> {
        self.content.lock().clear();
        *self.offered.lock() = formats;
        *self.provider.lock() = Some(provider);
        Ok(())
    }

    fn offer_files(
        &self,
        files: Vec<FileOffer>,
        provider: Arc<dyn VirtualFileProvider>,
    ) -> Result<()> {
        *self.offered_files.lock() = files;
        *self.file_provider.lock() = Some(provider);
        Ok(())
    }

    fn offer_local_files(&self, paths: Vec<PathBuf>) -> Result<()> {
        *self.local_files.lock() = paths;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// audio mocks
// ---------------------------------------------------------------------------------------------

/// Device list + hot-plug events controlled by the test.
#[derive(Default)]
pub struct MockAudioDevices {
    devices: Mutex<Vec<AudioDeviceInfo>>,
    watchers: Mutex<Vec<UnboundedSender<DeviceEvent>>>,
}

impl MockAudioDevices {
    #[must_use]
    pub fn with(devices: Vec<AudioDeviceInfo>) -> Self {
        Self {
            devices: Mutex::new(devices),
            watchers: Mutex::default(),
        }
    }

    pub fn add(&self, d: AudioDeviceInfo) {
        self.devices.lock().push(d.clone());
        self.watchers
            .lock()
            .retain(|w| w.send(DeviceEvent::Added(d.clone())).is_ok());
    }

    pub fn remove(&self, id: &str) {
        self.devices.lock().retain(|d| d.id != id);
        let id = id.to_string();
        self.watchers
            .lock()
            .retain(|w| w.send(DeviceEvent::Removed { id: id.clone() }).is_ok());
    }
}

impl AudioDevices for MockAudioDevices {
    fn list(&self, kind: DeviceKind) -> Result<Vec<AudioDeviceInfo>> {
        Ok(self
            .devices
            .lock()
            .iter()
            .filter(|d| d.kind == kind)
            .cloned()
            .collect())
    }

    fn watch(&self, tx: UnboundedSender<DeviceEvent>) -> Result<()> {
        self.watchers.lock().push(tx);
        Ok(())
    }
}

/// A convenient device entry.
#[must_use]
pub fn audio_device(id: &str, name: &str, kind: DeviceKind, is_default: bool) -> AudioDeviceInfo {
    AudioDeviceInfo {
        id: id.into(),
        name: name.into(),
        kind,
        is_default,
        is_default_communications: is_default,
        is_virtual: false,
    }
}

struct FlagHandle(Arc<std::sync::atomic::AtomicBool>);

impl StreamHandle for FlagHandle {}

impl Drop for FlagHandle {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

type CaptureSink = (
    CaptureSource,
    UnboundedSender<AudioFrame>,
    Arc<std::sync::atomic::AtomicBool>,
);
type PlaybackSlot = (
    Option<String>,
    Arc<dyn PullSource>,
    Arc<std::sync::atomic::AtomicBool>,
);

/// Capture whose frames are pushed by the test.
#[derive(Default)]
pub struct MockAudioCapture {
    sinks: Mutex<Vec<CaptureSink>>,
}

impl MockAudioCapture {
    /// Sends a frame to every active stream whose source kind matches (`loopback` or mic).
    pub fn push(&self, loopback: bool, samples: Vec<f32>, channels: u16, ts_us: u64) {
        for (src, tx, alive) in self.sinks.lock().iter() {
            let is_loop = matches!(src, CaptureSource::Loopback(_));
            if is_loop == loopback && alive.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = tx.send(AudioFrame {
                    samples: samples.clone(),
                    channels,
                    ts_us,
                });
            }
        }
    }

    #[must_use]
    pub fn active(&self, loopback: bool) -> bool {
        self.sinks.lock().iter().any(|(s, _, a)| {
            matches!(s, CaptureSource::Loopback(_)) == loopback
                && a.load(std::sync::atomic::Ordering::SeqCst)
        })
    }
}

impl AudioCapture for MockAudioCapture {
    fn start(
        &self,
        source: CaptureSource,
        sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>> {
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.sinks.lock().push((source, sink, alive.clone()));
        Ok(Box::new(FlagHandle(alive)))
    }
}

/// Playback that is pulled by the test.
#[derive(Default)]
pub struct MockAudioPlayback {
    current: Mutex<Option<PlaybackSlot>>,
    pub starts: Mutex<Vec<Option<String>>>,
}

impl MockAudioPlayback {
    /// Pulls `frames` stereo frames from the active stream, or `None` if no stream is running.
    pub fn pull(&self, frames: usize) -> Option<Vec<f32>> {
        let g = self.current.lock();
        let (_, src, alive) = g.as_ref()?;
        if !alive.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let mut out = vec![0.0f32; frames * 2];
        src.fill(&mut out);
        Some(out)
    }

    #[must_use]
    pub fn active_device(&self) -> Option<Option<String>> {
        let g = self.current.lock();
        let (d, _, a) = g.as_ref()?;
        a.load(std::sync::atomic::Ordering::SeqCst)
            .then(|| d.clone())
    }
}

impl AudioPlayback for MockAudioPlayback {
    fn start(
        &self,
        device_id: Option<&str>,
        source: Arc<dyn PullSource>,
    ) -> Result<Box<dyn StreamHandle>> {
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.starts.lock().push(device_id.map(str::to_string));
        *self.current.lock() = Some((device_id.map(str::to_string), source, alive.clone()));
        Ok(Box::new(FlagHandle(alive)))
    }
}

/// Virtual speaker/microphone driver stand-in.
#[derive(Default)]
pub struct MockVirtualAudio {
    pub available: std::sync::atomic::AtomicBool,
    pub mic_written: Mutex<Vec<f32>>,
    in_use: std::sync::atomic::AtomicBool,
    watchers: Mutex<Vec<UnboundedSender<MicUsage>>>,
}

impl MockVirtualAudio {
    #[must_use]
    pub fn available() -> Self {
        let v = Self::default();
        v.available.store(true, std::sync::atomic::Ordering::SeqCst);
        v
    }

    pub fn set_in_use(&self, in_use: bool) {
        self.in_use
            .store(in_use, std::sync::atomic::Ordering::SeqCst);
        let u = MicUsage {
            in_use,
            process: in_use.then(|| "Teams.exe".to_string()),
        };
        self.watchers.lock().retain(|w| w.send(u.clone()).is_ok());
    }
}

impl VirtualAudio for MockVirtualAudio {
    fn available(&self) -> bool {
        self.available.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn start_speaker_capture(
        &self,
        _sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>> {
        Err(crate::PlatformError::Unsupported("mock virtual speaker"))
    }

    fn write_mic(&self, frames: &[f32]) {
        self.mic_written.lock().extend_from_slice(frames);
    }

    fn mic_in_use(&self) -> bool {
        self.in_use.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn watch_mic_in_use(&self, tx: UnboundedSender<MicUsage>) -> Result<()> {
        self.watchers.lock().push(tx);
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
        assert!(inj.ops().contains(&InjectedOp::Button {
            button: Button::Left,
            down: false
        }));
    }
}
