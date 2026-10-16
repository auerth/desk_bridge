//! USB HID usage codes (usage page 0x07) – the internal key representation (plan section 17).
//! Every OS maps its own scancodes/keycodes to and from these.

pub const A: u16 = 0x04;
pub const Z: u16 = 0x1D;
pub const N1: u16 = 0x1E;
pub const N0: u16 = 0x27;
pub const ENTER: u16 = 0x28;
pub const ESCAPE: u16 = 0x29;
pub const BACKSPACE: u16 = 0x2A;
pub const TAB: u16 = 0x2B;
pub const SPACE: u16 = 0x2C;
pub const CAPS_LOCK: u16 = 0x39;
pub const F1: u16 = 0x3A;
pub const F12: u16 = 0x45;
pub const PRINT_SCREEN: u16 = 0x46;
pub const SCROLL_LOCK: u16 = 0x47;
pub const PAUSE: u16 = 0x48;
pub const INSERT: u16 = 0x49;
pub const HOME: u16 = 0x4A;
pub const PAGE_UP: u16 = 0x4B;
pub const DELETE: u16 = 0x4C;
pub const END: u16 = 0x4D;
pub const PAGE_DOWN: u16 = 0x4E;
pub const RIGHT: u16 = 0x4F;
pub const LEFT: u16 = 0x50;
pub const DOWN: u16 = 0x51;
pub const UP: u16 = 0x52;
pub const NUM_LOCK: u16 = 0x53;

pub const L_CTRL: u16 = 0xE0;
pub const L_SHIFT: u16 = 0xE1;
pub const L_ALT: u16 = 0xE2;
pub const L_GUI: u16 = 0xE3;
pub const R_CTRL: u16 = 0xE4;
pub const R_SHIFT: u16 = 0xE5;
pub const R_ALT: u16 = 0xE6;
pub const R_GUI: u16 = 0xE7;

/// Consumer / media keys are mapped into a private range above 0x100.
pub const MEDIA_PLAY_PAUSE: u16 = 0x1CD;
pub const MEDIA_NEXT: u16 = 0x1B5;
pub const MEDIA_PREV: u16 = 0x1B6;
pub const MEDIA_STOP: u16 = 0x1B7;
pub const VOLUME_UP: u16 = 0x1E9;
pub const VOLUME_DOWN: u16 = 0x1EA;
pub const VOLUME_MUTE: u16 = 0x1E2;

#[must_use]
pub const fn is_modifier(hid: u16) -> bool {
    hid >= L_CTRL && hid <= R_GUI
}

/// Modifier bit for a modifier usage (ctrl=1, shift=2, alt=4, gui=8), 0 for others.
#[must_use]
pub const fn modifier_bit(hid: u16) -> u8 {
    match hid {
        L_CTRL | R_CTRL => 1,
        L_SHIFT | R_SHIFT => 2,
        L_ALT | R_ALT => 4,
        L_GUI | R_GUI => 8,
        _ => 0,
    }
}

/// Human-readable name for UI and hotkey strings.
#[must_use]
pub fn name(hid: u16) -> String {
    match hid {
        A..=Z => ((b'A' + (hid - A) as u8) as char).to_string(),
        N1..=0x26 => ((b'1' + (hid - N1) as u8) as char).to_string(),
        N0 => "0".into(),
        ENTER => "Enter".into(),
        ESCAPE => "Esc".into(),
        BACKSPACE => "Backspace".into(),
        TAB => "Tab".into(),
        SPACE => "Space".into(),
        CAPS_LOCK => "CapsLock".into(),
        F1..=F12 => format!("F{}", hid - F1 + 1),
        PRINT_SCREEN => "PrintScreen".into(),
        SCROLL_LOCK => "ScrollLock".into(),
        PAUSE => "Pause".into(),
        INSERT => "Insert".into(),
        HOME => "Home".into(),
        PAGE_UP => "PageUp".into(),
        DELETE => "Delete".into(),
        END => "End".into(),
        PAGE_DOWN => "PageDown".into(),
        RIGHT => "Right".into(),
        LEFT => "Left".into(),
        DOWN => "Down".into(),
        UP => "Up".into(),
        NUM_LOCK => "NumLock".into(),
        L_CTRL | R_CTRL => "Ctrl".into(),
        L_SHIFT | R_SHIFT => "Shift".into(),
        L_ALT | R_ALT => "Alt".into(),
        L_GUI | R_GUI => "Win".into(),
        other => format!("Key{other:#x}"),
    }
}

/// Parses the name produced by [`name`] (case-insensitive, a few aliases).
#[must_use]
pub fn from_name(s: &str) -> Option<u16> {
    let l = s.trim().to_ascii_lowercase();
    if l.len() == 1 {
        let c = l.as_bytes()[0];
        return match c {
            b'a'..=b'z' => Some(A + u16::from(c - b'a')),
            b'1'..=b'9' => Some(N1 + u16::from(c - b'1')),
            b'0' => Some(N0),
            _ => None,
        };
    }
    if let Some(n) = l.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        if (1..=12).contains(&n) {
            return Some(F1 + n - 1);
        }
    }
    Some(match l.as_str() {
        "enter" | "return" => ENTER,
        "esc" | "escape" => ESCAPE,
        "backspace" | "rücktaste" => BACKSPACE,
        "tab" => TAB,
        "space" | "leertaste" => SPACE,
        "capslock" => CAPS_LOCK,
        "printscreen" | "druck" => PRINT_SCREEN,
        "scrolllock" | "rollen" => SCROLL_LOCK,
        "pause" => PAUSE,
        "insert" | "einfg" => INSERT,
        "home" | "pos1" => HOME,
        "pageup" | "bildauf" => PAGE_UP,
        "delete" | "entf" => DELETE,
        "end" | "ende" => END,
        "pagedown" | "bildab" => PAGE_DOWN,
        "right" => RIGHT,
        "left" => LEFT,
        "down" => DOWN,
        "up" => UP,
        "numlock" => NUM_LOCK,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        for hid in [A, Z, N1, N0, ENTER, SPACE, F1, F12, END, BACKSPACE, SCROLL_LOCK] {
            assert_eq!(from_name(&name(hid)), Some(hid), "{hid:#x}");
        }
    }
}
