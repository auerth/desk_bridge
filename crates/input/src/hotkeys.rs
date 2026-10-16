//! Hotkey actions and the default key bindings (plan 9.6).

use deskbridge_platform::hotkey::Hotkey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HotkeyAction {
    /// off → full → split → off
    FocusCycle,
    /// split mode directly on/off
    FocusSplit,
    /// lock the cursor to the current device
    LockCursor,
    /// jump to the other device
    JumpOther,
    MicMute,
    LaptopAudioMute,
    StatsOverlay,
    /// send Ctrl+Alt+Del to the target
    SendSas,
    /// emergency: everything local, focus mode off
    Emergency,
}

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 9] = [
        Self::FocusCycle,
        Self::FocusSplit,
        Self::LockCursor,
        Self::JumpOther,
        Self::MicMute,
        Self::LaptopAudioMute,
        Self::StatsOverlay,
        Self::SendSas,
        Self::Emergency,
    ];

    #[must_use]
    pub fn id(self) -> u32 {
        Self::ALL.iter().position(|a| *a == self).expect("listed") as u32 + 1
    }

    #[must_use]
    pub fn from_id(id: u32) -> Option<Self> {
        Self::ALL.get((id as usize).checked_sub(1)?).copied()
    }

    /// Stable key used in `config.toml`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::FocusCycle => "focus_cycle",
            Self::FocusSplit => "focus_split",
            Self::LockCursor => "lock_cursor",
            Self::JumpOther => "jump_other",
            Self::MicMute => "mic_mute",
            Self::LaptopAudioMute => "laptop_audio_mute",
            Self::StatsOverlay => "stats_overlay",
            Self::SendSas => "send_sas",
            Self::Emergency => "emergency",
        }
    }

    #[must_use]
    pub fn default_binding(self) -> &'static str {
        match self {
            Self::FocusCycle => "Ctrl+Alt+Shift+F",
            Self::FocusSplit => "Ctrl+Alt+Shift+D",
            Self::LockCursor => "ScrollLock",
            Self::JumpOther => "Ctrl+Alt+Shift+Space",
            Self::MicMute => "Ctrl+Alt+Shift+M",
            Self::LaptopAudioMute => "Ctrl+Alt+Shift+S",
            Self::StatsOverlay => "Ctrl+Alt+Shift+I",
            Self::SendSas => "Ctrl+Alt+End",
            Self::Emergency => "Ctrl+Alt+Shift+Backspace",
        }
    }
}

/// Builds the hotkey table from user bindings (`action key -> "Ctrl+Alt+X"`), falling back to the
/// defaults for missing or unparsable entries. Returns the hotkeys and the list of conflicts
/// (two actions bound to the same combination; the later one is dropped).
#[must_use]
pub fn build(bindings: &std::collections::BTreeMap<String, String>) -> (Vec<Hotkey>, Vec<(HotkeyAction, HotkeyAction)>) {
    let mut out: Vec<Hotkey> = Vec::new();
    let mut conflicts = Vec::new();
    for a in HotkeyAction::ALL {
        let spec = bindings.get(a.key()).map_or(a.default_binding(), String::as_str);
        let hk = Hotkey::parse(a.id(), spec).or_else(|| Hotkey::parse(a.id(), a.default_binding()));
        let Some(hk) = hk else { continue };
        if let Some(existing) = out.iter().find(|h| h.mods == hk.mods && h.key == hk.key) {
            conflicts.push((HotkeyAction::from_id(existing.id).expect("valid"), a));
            continue;
        }
        out.push(hk);
    }
    (out, conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn defaults_parse_without_conflicts() {
        let (hk, conflicts) = build(&BTreeMap::new());
        assert_eq!(hk.len(), HotkeyAction::ALL.len());
        assert!(conflicts.is_empty());
    }

    #[test]
    fn conflicts_are_reported() {
        let mut b = BTreeMap::new();
        b.insert("mic_mute".to_string(), "Ctrl+Alt+Shift+F".to_string());
        let (hk, conflicts) = build(&b);
        assert_eq!(conflicts, vec![(HotkeyAction::FocusCycle, HotkeyAction::MicMute)]);
        assert_eq!(hk.len(), HotkeyAction::ALL.len() - 1);
    }

    #[test]
    fn id_roundtrip() {
        for a in HotkeyAction::ALL {
            assert_eq!(HotkeyAction::from_id(a.id()), Some(a));
        }
    }
}
