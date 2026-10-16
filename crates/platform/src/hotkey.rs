//! Hotkey definition and matcher. The matcher is small and synchronous so that OS hook threads
//! can run it inline (hooks must decide "swallow or pass" immediately).

use crate::hid;
use serde::{Deserialize, Serialize};

pub const CTRL: u8 = 1;
pub const SHIFT: u8 = 2;
pub const ALT: u8 = 4;
pub const GUI: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hotkey {
    pub id: u32,
    /// Required modifier bitmask (exact match).
    pub mods: u8,
    /// Main key (HID usage).
    pub key: u16,
}

impl Hotkey {
    /// Parses strings like `Ctrl+Alt+Shift+F` or `ScrollLock`.
    pub fn parse(id: u32, s: &str) -> Option<Self> {
        let mut mods = 0u8;
        let mut key = None;
        for part in s.split('+') {
            match part.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "strg" | "control" => mods |= CTRL,
                "shift" => mods |= SHIFT,
                "alt" => mods |= ALT,
                "win" | "gui" | "meta" | "super" => mods |= GUI,
                other => {
                    if key.is_some() {
                        return None;
                    }
                    key = Some(hid::from_name(other)?);
                }
            }
        }
        Some(Self { id, mods, key: key? })
    }

    #[must_use]
    pub fn to_string_repr(&self) -> String {
        let mut parts = Vec::new();
        if self.mods & CTRL != 0 {
            parts.push("Ctrl".to_string());
        }
        if self.mods & ALT != 0 {
            parts.push("Alt".to_string());
        }
        if self.mods & SHIFT != 0 {
            parts.push("Shift".to_string());
        }
        if self.mods & GUI != 0 {
            parts.push("Win".to_string());
        }
        parts.push(hid::name(self.key));
        parts.join("+")
    }
}

/// Result of feeding a key event to the matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyOutcome {
    /// Not part of a hotkey: handle normally.
    Pass,
    /// Hotkey fired (on key-down); the event must be swallowed.
    Fired(u32),
    /// Key-up or repeat of a swallowed hotkey key; swallow without firing again.
    Swallow,
}

#[derive(Debug, Default)]
pub struct HotkeyMatcher {
    hotkeys: Vec<Hotkey>,
    mods: u8,
    /// Keys whose down event was swallowed (to also swallow repeat/up).
    swallowed: Vec<u16>,
}

impl HotkeyMatcher {
    #[must_use]
    pub fn new(hotkeys: Vec<Hotkey>) -> Self {
        Self { hotkeys, ..Self::default() }
    }

    pub fn set_hotkeys(&mut self, hotkeys: Vec<Hotkey>) {
        self.hotkeys = hotkeys;
    }

    #[must_use]
    pub fn modifiers(&self) -> u8 {
        self.mods
    }

    /// Feed a key event. Modifier keys themselves always pass (they must reach the target to
    /// stay consistent); only the main key of a matching hotkey is swallowed.
    pub fn feed(&mut self, key: u16, down: bool) -> HotkeyOutcome {
        let bit = hid::modifier_bit(key);
        if bit != 0 {
            if down {
                self.mods |= bit;
            } else {
                // Left/right variants share a bit; recompute conservatively on up.
                self.mods &= !bit;
            }
            return HotkeyOutcome::Pass;
        }
        if down {
            if let Some(h) = self.hotkeys.iter().find(|h| h.key == key && h.mods == self.mods) {
                let id = h.id;
                if self.swallowed.contains(&key) {
                    return HotkeyOutcome::Swallow; // auto-repeat
                }
                self.swallowed.push(key);
                return HotkeyOutcome::Fired(id);
            }
            HotkeyOutcome::Pass
        } else if let Some(pos) = self.swallowed.iter().position(|k| *k == key) {
            self.swallowed.swap_remove(pos);
            HotkeyOutcome::Swallow
        } else {
            HotkeyOutcome::Pass
        }
    }

    pub fn reset(&mut self) {
        self.mods = 0;
        self.swallowed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_print() {
        let h = Hotkey::parse(1, "Ctrl+Alt+Shift+F").unwrap();
        assert_eq!(h.mods, CTRL | ALT | SHIFT);
        assert_eq!(h.key, hid::A + 5);
        assert_eq!(h.to_string_repr(), "Ctrl+Alt+Shift+F");
        assert!(Hotkey::parse(1, "Ctrl+Nope").is_none());
        assert!(Hotkey::parse(1, "Ctrl").is_none());
    }

    #[test]
    fn fires_and_swallows_up() {
        let mut m = HotkeyMatcher::new(vec![Hotkey::parse(7, "Ctrl+Alt+Shift+F").unwrap()]);
        assert_eq!(m.feed(hid::L_CTRL, true), HotkeyOutcome::Pass);
        assert_eq!(m.feed(hid::L_ALT, true), HotkeyOutcome::Pass);
        assert_eq!(m.feed(hid::L_SHIFT, true), HotkeyOutcome::Pass);
        assert_eq!(m.feed(hid::A + 5, true), HotkeyOutcome::Fired(7));
        assert_eq!(m.feed(hid::A + 5, true), HotkeyOutcome::Swallow);
        assert_eq!(m.feed(hid::A + 5, false), HotkeyOutcome::Swallow);
        assert_eq!(m.feed(hid::A + 5, false), HotkeyOutcome::Pass);
    }

    #[test]
    fn exact_modifiers_required() {
        let mut m = HotkeyMatcher::new(vec![Hotkey::parse(1, "Ctrl+F").unwrap()]);
        m.feed(hid::L_CTRL, true);
        m.feed(hid::L_SHIFT, true);
        assert_eq!(m.feed(hid::A + 5, true), HotkeyOutcome::Pass);
    }

    #[test]
    fn bare_key_hotkey() {
        let mut m = HotkeyMatcher::new(vec![Hotkey::parse(2, "ScrollLock").unwrap()]);
        assert_eq!(m.feed(hid::SCROLL_LOCK, true), HotkeyOutcome::Fired(2));
    }
}
