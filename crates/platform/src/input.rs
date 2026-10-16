use crate::{Result, UnboundedSender, hotkey::Hotkey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl Button {
    /// Protocol number (1=left … 5=x2).
    #[must_use]
    pub fn to_wire(self) -> u32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 3,
            Self::X1 => 4,
            Self::X2 => 5,
        }
    }

    #[must_use]
    pub fn from_wire(n: u32) -> Option<Self> {
        Some(match n {
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Middle,
            4 => Self::X1,
            5 => Self::X2,
            _ => return None,
        })
    }
}

/// Event produced by the local input capture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureEvent {
    /// Keyboard key (HID usage). Already passed through the hotkey matcher.
    Key { hid: u16, down: bool },
    /// Absolute cursor position in physical virtual-desktop pixels (local mode only).
    MouseMove { x: i32, y: i32 },
    /// Raw relative motion (reported in both modes; the router decides what to use).
    MouseRel { dx: i32, dy: i32 },
    Button { button: Button, down: bool },
    /// 120 = one notch.
    Wheel { dx: i32, dy: i32 },
    /// A configured hotkey fired.
    Hotkey(u32),
}

/// What the capture blocks from reaching local applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrabMode {
    /// Nothing is blocked (local mode).
    #[default]
    None,
    /// Keyboard and mouse are blocked; mouse motion is reported as raw deltas (remote mode).
    All,
    /// Only the keyboard is blocked; the mouse keeps moving locally (focus mode with an
    /// absolutely mapped pointer).
    KeyboardOnly,
}

/// Local input capture (PC side). Implementations run their own thread.
pub trait InputCapture: Send + Sync {
    fn start(&self, sink: UnboundedSender<CaptureEvent>) -> Result<()>;
    fn stop(&self);
    fn set_grab(&self, mode: GrabMode);
    fn set_hotkeys(&self, hotkeys: Vec<Hotkey>);
    /// Moves the local cursor (used to park/restore it when switching).
    fn set_cursor_pos(&self, x: i32, y: i32);
    fn set_cursor_visible(&self, visible: bool);
}

/// Input injection (target side).
pub trait InputInjector: Send + Sync {
    fn key(&self, hid: u16, down: bool);
    fn mouse_rel(&self, dx: i32, dy: i32);
    /// Absolute position in physical virtual-desktop pixels.
    fn mouse_abs_px(&self, x: i32, y: i32);
    fn button(&self, button: Button, down: bool);
    fn wheel(&self, dx: i32, dy: i32);
    /// Release every key and button this injector has pressed.
    fn release_all(&self);
    /// Current lock key state and a way to toggle it by injecting the toggle key.
    fn toggles(&self) -> (bool, bool, bool);
}
