//! Windows scancode (set 1 make code + `extended` flag) <-> USB HID usage mapping.
//!
//! Conventions (these are what `KBDLLHOOKSTRUCT.scanCode` / `LLKHF_EXTENDED` deliver and what
//! `SendInput` with `KEYEVENTF_SCANCODE` / `KEYEVENTF_EXTENDEDKEY` consumes):
//!
//! * `extended == true` corresponds to the `E0` prefix of the set-1 make code.
//! * NumLock is `0x45` + extended (Windows' own convention, `kbd.h` marks it `KBDEXT`);
//!   Pause (physically `E1 1D 45`) shows up as `0x45` without the extended flag, and some
//!   drivers report it with a different scancode, so hooks classify it by virtual key first
//!   (see [`classify_hook_key`]).
//! * HID usages are page 0x07; media/browser/launch keys live in the private range
//!   `0x100 + consumer usage` that [`platform::hid`] already uses for `MEDIA_*` / `VOLUME_*`.
//!
//! AltGr: on layouts with AltGr (German!) Windows injects a *fake* left Ctrl (scancode `0x21D`)
//! right before the right-Alt event. That event must not be forwarded: the target gets
//! `R_ALT` (`0x38` + extended) alone and synthesises its own fake Ctrl. The hook code therefore
//! calls [`classify_hook_key`], which reports [`HookKey::FakeCtrl`] for it. Likewise Windows
//! wraps some navigation keys with fake shift events (`0x2A`/`0x36` + extended) when NumLock
//! is on; those are reported as [`HookKey::FakeShift`].

use deskbridge_platform::hid;

/// Consumer-usage based private HID range (`0x100 + usage`) for keys not defined in
/// [`platform::hid`].
pub mod consumer {
    pub const BROWSER_HOME: u16 = 0x100 + 0x223;
    pub const BROWSER_SEARCH: u16 = 0x100 + 0x221;
    pub const BROWSER_BACK: u16 = 0x100 + 0x224;
    pub const BROWSER_FORWARD: u16 = 0x100 + 0x225;
    pub const BROWSER_STOP: u16 = 0x100 + 0x226;
    pub const BROWSER_REFRESH: u16 = 0x100 + 0x227;
    pub const BROWSER_FAVORITES: u16 = 0x100 + 0x22A;
    pub const LAUNCH_MAIL: u16 = 0x100 + 0x18A;
    pub const LAUNCH_MEDIA_SELECT: u16 = 0x100 + 0x183;
    pub const LAUNCH_MY_COMPUTER: u16 = 0x100 + 0x194;
    pub const LAUNCH_CALCULATOR: u16 = 0x100 + 0x192;
}

/// Scancode of the fake left Ctrl that Windows inserts before AltGr (see module docs).
pub const SC_FAKE_LCTRL: u32 = 0x21D;

const NO_EXT: bool = false;
const EXT: bool = true;

/// Primary bijective table: (scancode, extended, HID usage).
#[rustfmt::skip]
const TABLE: &[(u16, bool, u16)] = &[
    // Letters (QWERTZ vs QWERTY is irrelevant: scancodes are positional).
    (0x1E, NO_EXT, 0x04), (0x30, NO_EXT, 0x05), (0x2E, NO_EXT, 0x06), (0x20, NO_EXT, 0x07),
    (0x12, NO_EXT, 0x08), (0x21, NO_EXT, 0x09), (0x22, NO_EXT, 0x0A), (0x23, NO_EXT, 0x0B),
    (0x17, NO_EXT, 0x0C), (0x24, NO_EXT, 0x0D), (0x25, NO_EXT, 0x0E), (0x26, NO_EXT, 0x0F),
    (0x32, NO_EXT, 0x10), (0x31, NO_EXT, 0x11), (0x18, NO_EXT, 0x12), (0x19, NO_EXT, 0x13),
    (0x10, NO_EXT, 0x14), (0x13, NO_EXT, 0x15), (0x1F, NO_EXT, 0x16), (0x14, NO_EXT, 0x17),
    (0x16, NO_EXT, 0x18), (0x2F, NO_EXT, 0x19), (0x11, NO_EXT, 0x1A), (0x2D, NO_EXT, 0x1B),
    (0x15, NO_EXT, 0x1C), (0x2C, NO_EXT, 0x1D),
    // Digit row 1..9, 0
    (0x02, NO_EXT, 0x1E), (0x03, NO_EXT, 0x1F), (0x04, NO_EXT, 0x20), (0x05, NO_EXT, 0x21),
    (0x06, NO_EXT, 0x22), (0x07, NO_EXT, 0x23), (0x08, NO_EXT, 0x24), (0x09, NO_EXT, 0x25),
    (0x0A, NO_EXT, 0x26), (0x0B, NO_EXT, 0x27),
    // Enter, Esc, Backspace, Tab, Space
    (0x1C, NO_EXT, 0x28), (0x01, NO_EXT, 0x29), (0x0E, NO_EXT, 0x2A), (0x0F, NO_EXT, 0x2B),
    (0x39, NO_EXT, 0x2C),
    // Punctuation: DE labels in comments (ß ´ ü + # ö ä ^ , . -)
    (0x0C, NO_EXT, 0x2D), // ß
    (0x0D, NO_EXT, 0x2E), // ´
    (0x1A, NO_EXT, 0x2F), // ü
    (0x1B, NO_EXT, 0x30), // +
    (0x2B, NO_EXT, 0x31), // # (ISO) / \ (ANSI); the ISO usage 0x32 is an alias below
    (0x27, NO_EXT, 0x33), // ö
    (0x28, NO_EXT, 0x34), // ä
    (0x29, NO_EXT, 0x35), // ^
    (0x33, NO_EXT, 0x36), // ,
    (0x34, NO_EXT, 0x37), // .
    (0x35, NO_EXT, 0x38), // -
    (0x3A, NO_EXT, 0x39), // CapsLock
    // F1..F10, F11, F12
    (0x3B, NO_EXT, 0x3A), (0x3C, NO_EXT, 0x3B), (0x3D, NO_EXT, 0x3C), (0x3E, NO_EXT, 0x3D),
    (0x3F, NO_EXT, 0x3E), (0x40, NO_EXT, 0x3F), (0x41, NO_EXT, 0x40), (0x42, NO_EXT, 0x41),
    (0x43, NO_EXT, 0x42), (0x44, NO_EXT, 0x43), (0x57, NO_EXT, 0x44), (0x58, NO_EXT, 0x45),
    // Print, Scroll, Pause
    (0x37, EXT, 0x46), (0x46, NO_EXT, 0x47), (0x45, NO_EXT, 0x48),
    // Navigation cluster (all extended)
    (0x52, EXT, 0x49), (0x47, EXT, 0x4A), (0x49, EXT, 0x4B), (0x53, EXT, 0x4C),
    (0x4F, EXT, 0x4D), (0x51, EXT, 0x4E), (0x4D, EXT, 0x4F), (0x4B, EXT, 0x50),
    (0x50, EXT, 0x51), (0x48, EXT, 0x52),
    // NumLock and keypad
    (0x45, EXT, 0x53),    // NumLock
    (0x35, EXT, 0x54),    // KP /
    (0x37, NO_EXT, 0x55), // KP *
    (0x4A, NO_EXT, 0x56), // KP -
    (0x4E, NO_EXT, 0x57), // KP +
    (0x1C, EXT, 0x58),    // KP Enter
    (0x4F, NO_EXT, 0x59), // KP 1
    (0x50, NO_EXT, 0x5A), // KP 2
    (0x51, NO_EXT, 0x5B), // KP 3
    (0x4B, NO_EXT, 0x5C), // KP 4
    (0x4C, NO_EXT, 0x5D), // KP 5
    (0x4D, NO_EXT, 0x5E), // KP 6
    (0x47, NO_EXT, 0x5F), // KP 7
    (0x48, NO_EXT, 0x60), // KP 8
    (0x49, NO_EXT, 0x61), // KP 9
    (0x52, NO_EXT, 0x62), // KP 0
    (0x53, NO_EXT, 0x63), // KP . / ,
    (0x56, NO_EXT, 0x64), // <>| key (ISO "non-US backslash")
    (0x5D, EXT, 0x65),    // Menu / Application
    (0x59, NO_EXT, 0x67), // KP =
    // F13..F24
    (0x64, NO_EXT, 0x68), (0x65, NO_EXT, 0x69), (0x66, NO_EXT, 0x6A), (0x67, NO_EXT, 0x6B),
    (0x68, NO_EXT, 0x6C), (0x69, NO_EXT, 0x6D), (0x6A, NO_EXT, 0x6E), (0x6B, NO_EXT, 0x6F),
    (0x6C, NO_EXT, 0x70), (0x6D, NO_EXT, 0x71), (0x6E, NO_EXT, 0x72), (0x76, NO_EXT, 0x73),
    // International keys (JIS): Ro, Kana, Yen, Henkan, Muhenkan
    (0x73, NO_EXT, 0x87), (0x70, NO_EXT, 0x88), (0x7D, NO_EXT, 0x89), (0x79, NO_EXT, 0x8A),
    (0x7B, NO_EXT, 0x8B),
    // Modifiers
    (0x1D, NO_EXT, 0xE0), (0x2A, NO_EXT, 0xE1), (0x38, NO_EXT, 0xE2), (0x5B, EXT, 0xE3),
    (0x1D, EXT, 0xE4), (0x36, NO_EXT, 0xE5), (0x38, EXT, 0xE6), (0x5C, EXT, 0xE7),
    // Media / browser / launch keys (all extended)
    (0x22, EXT, hid::MEDIA_PLAY_PAUSE),
    (0x19, EXT, hid::MEDIA_NEXT),
    (0x10, EXT, hid::MEDIA_PREV),
    (0x24, EXT, hid::MEDIA_STOP),
    (0x30, EXT, hid::VOLUME_UP),
    (0x2E, EXT, hid::VOLUME_DOWN),
    (0x20, EXT, hid::VOLUME_MUTE),
    (0x6A, EXT, consumer::BROWSER_BACK),
    (0x69, EXT, consumer::BROWSER_FORWARD),
    (0x68, EXT, consumer::BROWSER_STOP),
    (0x67, EXT, consumer::BROWSER_REFRESH),
    (0x65, EXT, consumer::BROWSER_SEARCH),
    (0x66, EXT, consumer::BROWSER_FAVORITES),
    (0x32, EXT, consumer::BROWSER_HOME),
    (0x6C, EXT, consumer::LAUNCH_MAIL),
    (0x6D, EXT, consumer::LAUNCH_MEDIA_SELECT),
    (0x6B, EXT, consumer::LAUNCH_MY_COMPUTER),
    (0x21, EXT, consumer::LAUNCH_CALCULATOR),
];

/// Extra scancode -> HID entries that are *not* reversible (several scancodes for one usage).
const SC_ALIASES: &[(u16, bool, u16)] = &[
    (0x54, NO_EXT, 0x46), // SysRq (Alt+Print) -> Print Screen
    (0x46, EXT, 0x48),    // Ctrl+Break -> Pause
];

/// Extra HID -> scancode entries (several usages for one scancode).
const HID_ALIASES: &[(u16, u16, bool)] = &[
    (0x32, 0x2B, NO_EXT), // Keyboard Non-US # (ISO) -> the same physical key as backslash
];

const SC_INDEX_LEN: usize = 512;
const HID_INDEX_LEN: usize = 0x400;
const NONE16: u16 = 0xFFFF;

const fn sc_slot(sc: u16, ext: bool) -> usize {
    (sc as usize & 0xFF) | if ext { 0x100 } else { 0 }
}

const fn build_sc_index() -> [u16; SC_INDEX_LEN] {
    let mut idx = [NONE16; SC_INDEX_LEN];
    let mut i = 0;
    while i < TABLE.len() {
        let (sc, ext, hid) = TABLE[i];
        idx[sc_slot(sc, ext)] = hid;
        i += 1;
    }
    let mut j = 0;
    while j < SC_ALIASES.len() {
        let (sc, ext, hid) = SC_ALIASES[j];
        idx[sc_slot(sc, ext)] = hid;
        j += 1;
    }
    idx
}

const fn build_hid_index() -> [u16; HID_INDEX_LEN] {
    // value: scancode | 0x100 if extended; NONE16 if unmapped
    let mut idx = [NONE16; HID_INDEX_LEN];
    let mut i = 0;
    while i < TABLE.len() {
        let (sc, ext, hid) = TABLE[i];
        idx[hid as usize] = sc_slot(sc, ext) as u16;
        i += 1;
    }
    let mut j = 0;
    while j < HID_ALIASES.len() {
        let (hid, sc, ext) = HID_ALIASES[j];
        idx[hid as usize] = sc_slot(sc, ext) as u16;
        j += 1;
    }
    idx
}

static SC_TO_HID: [u16; SC_INDEX_LEN] = build_sc_index();
static HID_TO_SC: [u16; HID_INDEX_LEN] = build_hid_index();

/// Windows scancode (set 1 make code, 8 bit) + extended flag -> HID usage.
#[must_use]
pub fn scancode_to_hid(scancode: u16, extended: bool) -> Option<u16> {
    if scancode > 0xFF {
        return None;
    }
    match SC_TO_HID[sc_slot(scancode, extended)] {
        NONE16 => None,
        hid => Some(hid),
    }
}

/// HID usage -> (scancode, extended) for `SendInput`.
#[must_use]
pub fn hid_to_scancode(hid: u16) -> Option<(u16, bool)> {
    match HID_TO_SC.get(hid as usize).copied() {
        None | Some(NONE16) => None,
        Some(v) => Some((v & 0xFF, v & 0x100 != 0)),
    }
}

// Virtual key codes used by the fallbacks (kept local so this module stays pure).
const VK_PAUSE: u32 = 0x13;
const VK_SNAPSHOT: u32 = 0x2C;
const VK_NUMLOCK: u32 = 0x90;
const VK_BROWSER_BACK: u32 = 0xA6;
const VK_BROWSER_FORWARD: u32 = 0xA7;
const VK_BROWSER_REFRESH: u32 = 0xA8;
const VK_BROWSER_STOP: u32 = 0xA9;
const VK_BROWSER_SEARCH: u32 = 0xAA;
const VK_BROWSER_FAVORITES: u32 = 0xAB;
const VK_BROWSER_HOME: u32 = 0xAC;
const VK_VOLUME_MUTE: u32 = 0xAD;
const VK_VOLUME_DOWN: u32 = 0xAE;
const VK_VOLUME_UP: u32 = 0xAF;
const VK_MEDIA_NEXT_TRACK: u32 = 0xB0;
const VK_MEDIA_PREV_TRACK: u32 = 0xB1;
const VK_MEDIA_STOP: u32 = 0xB2;
const VK_MEDIA_PLAY_PAUSE: u32 = 0xB3;
const VK_LAUNCH_MAIL: u32 = 0xB4;
const VK_LAUNCH_MEDIA_SELECT: u32 = 0xB5;
const VK_LAUNCH_APP1: u32 = 0xB6;
const VK_LAUNCH_APP2: u32 = 0xB7;

/// Keys whose scancode path is unreliable; they are injected/identified by virtual key.
const VK_TABLE: &[(u32, u16)] = &[
    (VK_PAUSE, hid::PAUSE),
    (VK_SNAPSHOT, hid::PRINT_SCREEN),
    (VK_NUMLOCK, hid::NUM_LOCK),
    (VK_BROWSER_BACK, consumer::BROWSER_BACK),
    (VK_BROWSER_FORWARD, consumer::BROWSER_FORWARD),
    (VK_BROWSER_REFRESH, consumer::BROWSER_REFRESH),
    (VK_BROWSER_STOP, consumer::BROWSER_STOP),
    (VK_BROWSER_SEARCH, consumer::BROWSER_SEARCH),
    (VK_BROWSER_FAVORITES, consumer::BROWSER_FAVORITES),
    (VK_BROWSER_HOME, consumer::BROWSER_HOME),
    (VK_VOLUME_MUTE, hid::VOLUME_MUTE),
    (VK_VOLUME_DOWN, hid::VOLUME_DOWN),
    (VK_VOLUME_UP, hid::VOLUME_UP),
    (VK_MEDIA_NEXT_TRACK, hid::MEDIA_NEXT),
    (VK_MEDIA_PREV_TRACK, hid::MEDIA_PREV),
    (VK_MEDIA_STOP, hid::MEDIA_STOP),
    (VK_MEDIA_PLAY_PAUSE, hid::MEDIA_PLAY_PAUSE),
    (VK_LAUNCH_MAIL, consumer::LAUNCH_MAIL),
    (VK_LAUNCH_MEDIA_SELECT, consumer::LAUNCH_MEDIA_SELECT),
    (VK_LAUNCH_APP1, consumer::LAUNCH_MY_COMPUTER),
    (VK_LAUNCH_APP2, consumer::LAUNCH_CALCULATOR),
];

/// Virtual key -> HID for the keys in [`VK_TABLE`] (Pause, Print, NumLock, media/browser keys).
#[must_use]
pub fn vk_to_hid(vk: u32) -> Option<u16> {
    VK_TABLE.iter().find(|(v, _)| *v == vk).map(|(_, h)| *h)
}

/// HID -> virtual key for keys that are injected by VK instead of by scancode.
///
/// Pause has no usable set-1 make code (`E1 1D 45`), and media keys are most reliably injected
/// by VK because they do not depend on the keyboard layout's scancode tables.
#[must_use]
pub fn hid_to_vk(hid: u16) -> Option<u32> {
    // Print Screen and NumLock work fine via scancode; only Pause and the consumer range use VK.
    if hid == hid::PRINT_SCREEN || hid == hid::NUM_LOCK {
        return None;
    }
    VK_TABLE.iter().find(|(_, h)| *h == hid).map(|(v, _)| *v)
}

/// Classification of a low-level keyboard hook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKey {
    /// A regular key with its HID usage.
    Hid(u16),
    /// The fake left Ctrl that Windows inserts before AltGr: do not forward.
    FakeCtrl,
    /// The fake shift Windows wraps around some navigation keys: do not forward.
    FakeShift,
    /// A key we cannot map (carries the virtual key code for bookkeeping).
    Unknown(u32),
}

/// Classifies a hook event from `vkCode`, `scanCode` (full DWORD) and `LLKHF_EXTENDED`.
#[must_use]
pub fn classify_hook_key(vk: u32, scancode: u32, extended: bool) -> HookKey {
    if scancode == SC_FAKE_LCTRL {
        return HookKey::FakeCtrl;
    }
    // Fake shifts: extended 0x2A / 0x36 never belong to a physical key.
    if extended && (scancode == 0x2A || scancode == 0x36) {
        return HookKey::FakeShift;
    }
    // Keys whose scancode representation is ambiguous are identified by virtual key.
    if vk == VK_PAUSE || vk == VK_NUMLOCK {
        return HookKey::Hid(vk_to_hid(vk).unwrap_or(hid::PAUSE));
    }
    if scancode <= 0xFF
        && let Some(h) = scancode_to_hid(scancode as u16, extended)
    {
        return HookKey::Hid(h);
    }
    match vk_to_hid(vk) {
        Some(h) => HookKey::Hid(h),
        None => HookKey::Unknown(vk),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_has_unique_keys() {
        let mut scs = HashSet::new();
        let mut hids = HashSet::new();
        for &(sc, ext, h) in TABLE {
            assert!(
                scs.insert((sc, ext)),
                "duplicate scancode {sc:#x} ext={ext}"
            );
            assert!(hids.insert(h), "duplicate hid {h:#x}");
            assert!((sc as usize) <= 0xFF);
            assert!((h as usize) < HID_INDEX_LEN, "hid {h:#x} outside index");
        }
    }

    #[test]
    fn every_entry_round_trips() {
        for &(sc, ext, h) in TABLE {
            assert_eq!(scancode_to_hid(sc, ext), Some(h), "sc {sc:#x} ext={ext}");
            assert_eq!(hid_to_scancode(h), Some((sc, ext)), "hid {h:#x}");
        }
    }

    #[test]
    fn spot_checks() {
        assert_eq!(scancode_to_hid(0x1E, false), Some(0x04)); // A
        assert_eq!(hid_to_scancode(0x04), Some((0x1E, false)));
        assert_eq!(scancode_to_hid(0x1D, true), Some(hid::R_CTRL));
        assert_eq!(scancode_to_hid(0x1D, false), Some(hid::L_CTRL));
        assert_eq!(scancode_to_hid(0x1C, true), Some(0x58)); // numpad enter
        assert_eq!(scancode_to_hid(0x1C, false), Some(hid::ENTER));
        assert_eq!(scancode_to_hid(0x53, true), Some(hid::DELETE));
        assert_eq!(scancode_to_hid(0x53, false), Some(0x63)); // numpad .
        assert_eq!(scancode_to_hid(0x38, true), Some(hid::R_ALT)); // AltGr
        assert_eq!(scancode_to_hid(0x56, false), Some(0x64)); // <>| key
        assert_eq!(scancode_to_hid(0x2B, false), Some(0x31)); // # key
        assert_eq!(hid_to_scancode(0x32), Some((0x2B, false))); // ISO # alias
        assert_eq!(scancode_to_hid(0x45, true), Some(hid::NUM_LOCK));
        assert_eq!(scancode_to_hid(0x76, false), Some(0x73)); // F24
        assert_eq!(scancode_to_hid(0x64, false), Some(0x68)); // F13
        assert_eq!(scancode_to_hid(0x5B, true), Some(hid::L_GUI));
        assert_eq!(scancode_to_hid(0x5D, true), Some(0x65)); // Menu
        assert_eq!(scancode_to_hid(0x20, true), Some(hid::VOLUME_MUTE));
        assert_eq!(scancode_to_hid(0x54, false), Some(hid::PRINT_SCREEN)); // SysRq
        assert_eq!(scancode_to_hid(0x7F, false), None);
        assert_eq!(scancode_to_hid(0x21D, false), None);
        assert_eq!(hid_to_scancode(0x00), None);
        assert_eq!(hid_to_scancode(0x3FF), None);
        assert_eq!(hid_to_scancode(0xFFFF), None);
    }

    #[test]
    fn full_german_105_key_layout_is_covered() {
        // Every HID usage of a 105-key ISO keyboard: letters .. Application (0x04..=0x65,
        // incl. the <>| key 0x64) plus the eight modifiers.
        for h in (0x04..=0x65).chain(hid::L_CTRL..=hid::R_GUI) {
            assert!(hid_to_scancode(h).is_some(), "hid {h:#x} unmapped");
        }
    }

    #[test]
    fn index_slots_are_sane() {
        for h in 0..HID_INDEX_LEN as u16 {
            if let Some((sc, _)) = hid_to_scancode(h) {
                assert!(sc != 0 && sc <= 0xFF);
            }
        }
    }

    #[test]
    fn hook_classification() {
        // Fake AltGr ctrl.
        assert_eq!(classify_hook_key(0xA2, 0x21D, false), HookKey::FakeCtrl);
        // Real right alt (AltGr) maps to R_ALT.
        assert_eq!(
            classify_hook_key(0xA5, 0x38, true),
            HookKey::Hid(hid::R_ALT)
        );
        // Fake shift around Insert/Home etc.
        assert_eq!(classify_hook_key(0xA0, 0x2A, true), HookKey::FakeShift);
        assert_eq!(classify_hook_key(0xA1, 0x36, true), HookKey::FakeShift);
        // Real shifts.
        assert_eq!(
            classify_hook_key(0xA0, 0x2A, false),
            HookKey::Hid(hid::L_SHIFT)
        );
        assert_eq!(
            classify_hook_key(0xA1, 0x36, false),
            HookKey::Hid(hid::R_SHIFT)
        );
        // Pause by vk regardless of scancode.
        assert_eq!(
            classify_hook_key(VK_PAUSE, 0x45, false),
            HookKey::Hid(hid::PAUSE)
        );
        assert_eq!(
            classify_hook_key(VK_PAUSE, 0x1D, false),
            HookKey::Hid(hid::PAUSE)
        );
        assert_eq!(
            classify_hook_key(VK_NUMLOCK, 0x45, true),
            HookKey::Hid(hid::NUM_LOCK)
        );
        // Zero scancode (some synthetic events) falls back to vk.
        assert_eq!(
            classify_hook_key(VK_VOLUME_UP, 0, false),
            HookKey::Hid(hid::VOLUME_UP)
        );
        // Totally unknown key.
        assert_eq!(classify_hook_key(0xFF, 0, false), HookKey::Unknown(0xFF));
    }

    #[test]
    fn vk_tables_are_consistent() {
        for &(vk, h) in VK_TABLE {
            assert_eq!(vk_to_hid(vk), Some(h));
        }
        assert_eq!(hid_to_vk(hid::PAUSE), Some(VK_PAUSE));
        assert_eq!(hid_to_vk(hid::MEDIA_PLAY_PAUSE), Some(VK_MEDIA_PLAY_PAUSE));
        assert_eq!(hid_to_vk(hid::PRINT_SCREEN), None);
        assert_eq!(hid_to_vk(hid::NUM_LOCK), None);
        assert_eq!(hid_to_vk(hid::A), None);
    }
}
