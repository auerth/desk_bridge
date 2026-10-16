//! Platform-neutral model of a Windows display configuration (CCD paths + modes), the logic to
//! re-match a saved configuration to the monitors present now (adapter LUIDs change on reboot),
//! and the edits focus mode needs (drop the internal panel, make a display primary).
//!
//! The Windows layer converts `DISPLAYCONFIG_*` structs from and to this model; everything here is
//! pure and tested with synthetic data.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;
/// "no mode" in the legacy (non virtual-mode-aware) index encoding.
pub const IDX_INVALID: u32 = 0xFFFF_FFFF;
/// "no mode" in the 16-bit halves of the virtual-mode-aware index encoding.
pub const IDX16_INVALID: u32 = 0xFFFF;

pub const INFO_TYPE_SOURCE: i32 = 1;
pub const INFO_TYPE_TARGET: i32 = 2;
pub const INFO_TYPE_DESKTOP_IMAGE: i32 = 3;

// DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY values
pub const OUTPUT_LVDS: i32 = 6;
pub const OUTPUT_DISPLAYPORT_EMBEDDED: i32 = 11;
pub const OUTPUT_UDI_EMBEDDED: i32 = 13;
pub const OUTPUT_INDIRECT_WIRED: i32 = 16;
pub const OUTPUT_INDIRECT_VIRTUAL: i32 = 17;
/// 0x80000000 as the signed enum value.
pub const OUTPUT_INTERNAL: i32 = i32::MIN;

/// Is this an internal laptop panel (eDP / LVDS / UDI embedded / "internal")?
#[must_use]
pub fn is_internal_technology(t: i32) -> bool {
    matches!(
        t,
        OUTPUT_INTERNAL | OUTPUT_LVDS | OUTPUT_DISPLAYPORT_EMBEDDED | OUTPUT_UDI_EMBEDDED
    )
}

/// Indirect display (IddCx virtual displays, Miracast-like).
#[must_use]
pub fn is_indirect_technology(t: i32) -> bool {
    matches!(t, OUTPUT_INDIRECT_WIRED | OUTPUT_INDIRECT_VIRTUAL)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Luid {
    pub high: i32,
    pub low: u32,
}

/// What identifies a monitor across reboots (the LUID and target id are not stable).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TargetIdentity {
    /// `DISPLAYCONFIG_ADAPTER_NAME.adapterDevicePath` (PCI path of the GPU).
    pub adapter_path: String,
    /// `DISPLAYCONFIG_TARGET_DEVICE_NAME.monitorDevicePath`.
    pub device_path: String,
    pub friendly_name: String,
    pub edid_manufacture_id: u16,
    pub edid_product_code: u16,
    pub connector_instance: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSource {
    pub adapter: Luid,
    pub id: u32,
    /// Index into `SavedTopology::modes` (source mode).
    pub mode_idx: Option<u32>,
    /// Virtual-mode-aware only.
    pub clone_group: Option<u32>,
    pub status_flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTarget {
    pub adapter: Luid,
    pub id: u32,
    /// Index of the target mode (virtual-mode-aware: `targetModeInfoIdx`).
    pub mode_idx: Option<u32>,
    /// Index of the desktop-image mode (virtual-mode-aware only).
    pub desktop_idx: Option<u32>,
    pub output_technology: i32,
    pub rotation: i32,
    pub scaling: i32,
    pub refresh_num: u32,
    pub refresh_den: u32,
    pub scanline_ordering: i32,
    pub target_available: bool,
    pub status_flags: u32,
    pub identity: TargetIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPath {
    pub source: SavedSource,
    pub target: SavedTarget,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModeBody {
    Source {
        width: u32,
        height: u32,
        pixel_format: i32,
        x: i32,
        y: i32,
    },
    Target {
        pixel_rate: u64,
        h_sync: (u32, u32),
        v_sync: (u32, u32),
        active: (u32, u32),
        total: (u32, u32),
        /// Raw union of `DISPLAYCONFIG_VIDEO_SIGNAL_INFO` (video standard / additional info).
        signal_flags: u32,
        scanline_ordering: i32,
    },
    DesktopImage {
        path_source_size: (i32, i32),
        region: (i32, i32, i32, i32),
        clip: (i32, i32, i32, i32),
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedMode {
    pub info_type: i32,
    pub id: u32,
    pub adapter: Luid,
    pub body: ModeBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTopology {
    pub version: u32,
    /// Queried (and to be applied) with the virtual-mode-aware flags.
    pub virtual_aware: bool,
    pub paths: Vec<SavedPath>,
    pub modes: Vec<SavedMode>,
}

#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("topology JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported topology format version {0}")]
    Version(u32),
    #[error("invalid topology: {0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------------------------
// Mode index encoding

/// Raw `modeInfoIdx` union value of a path source.
#[must_use]
pub fn pack_source_idx(aware: bool, clone_group: Option<u32>, mode: Option<u32>) -> u32 {
    if aware {
        clone_group.unwrap_or(IDX16_INVALID) & 0xFFFF
            | (mode.unwrap_or(IDX16_INVALID) & 0xFFFF) << 16
    } else {
        mode.unwrap_or(IDX_INVALID)
    }
}

/// Inverse of [`pack_source_idx`]: `(clone_group, source_mode)`.
#[must_use]
pub fn unpack_source_idx(aware: bool, raw: u32) -> (Option<u32>, Option<u32>) {
    if aware {
        let (c, m) = (raw & 0xFFFF, raw >> 16);
        (
            (c != IDX16_INVALID).then_some(c),
            (m != IDX16_INVALID).then_some(m),
        )
    } else {
        (None, (raw != IDX_INVALID).then_some(raw))
    }
}

/// Raw `modeInfoIdx` union value of a path target.
#[must_use]
pub fn pack_target_idx(aware: bool, desktop: Option<u32>, mode: Option<u32>) -> u32 {
    if aware {
        desktop.unwrap_or(IDX16_INVALID) & 0xFFFF | (mode.unwrap_or(IDX16_INVALID) & 0xFFFF) << 16
    } else {
        mode.unwrap_or(IDX_INVALID)
    }
}

/// Inverse of [`pack_target_idx`]: `(desktop_image_mode, target_mode)`.
#[must_use]
pub fn unpack_target_idx(aware: bool, raw: u32) -> (Option<u32>, Option<u32>) {
    if aware {
        let (d, m) = (raw & 0xFFFF, raw >> 16);
        (
            (d != IDX16_INVALID).then_some(d),
            (m != IDX16_INVALID).then_some(m),
        )
    } else {
        (None, (raw != IDX_INVALID).then_some(raw))
    }
}

// ---------------------------------------------------------------------------------------------
// Model operations

impl SavedPath {
    /// Mode indices this path refers to.
    fn mode_refs(&self) -> impl Iterator<Item = u32> {
        [
            self.source.mode_idx,
            self.target.mode_idx,
            self.target.desktop_idx,
        ]
        .into_iter()
        .flatten()
    }

    #[must_use]
    pub fn is_internal(&self) -> bool {
        is_internal_technology(self.target.output_technology)
    }

    #[must_use]
    pub fn is_indirect(&self) -> bool {
        is_indirect_technology(self.target.output_technology)
    }

    #[must_use]
    pub fn target_key(&self) -> (Luid, u32) {
        (self.target.adapter, self.target.id)
    }
}

impl SavedTopology {
    pub fn to_json(&self) -> Result<String, TopologyError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parses and validates a topology saved by [`Self::to_json`].
    pub fn from_json(s: &str) -> Result<Self, TopologyError> {
        let t: Self = serde_json::from_str(s)?;
        if t.version != FORMAT_VERSION {
            return Err(TopologyError::Version(t.version));
        }
        t.validate()?;
        Ok(t)
    }

    /// Structural checks: at least one path, all mode indices in range and of the right type.
    pub fn validate(&self) -> Result<(), TopologyError> {
        if self.paths.is_empty() {
            return Err(TopologyError::Invalid("no paths".into()));
        }
        let kind = |i: u32| self.modes.get(i as usize).map(|m| m.info_type);
        for (n, p) in self.paths.iter().enumerate() {
            let checks = [
                (p.source.mode_idx, INFO_TYPE_SOURCE, "source"),
                (p.target.mode_idx, INFO_TYPE_TARGET, "target"),
                (p.target.desktop_idx, INFO_TYPE_DESKTOP_IMAGE, "desktop"),
            ];
            for (idx, want, what) in checks {
                if let Some(i) = idx
                    && kind(i) != Some(want)
                {
                    return Err(TopologyError::Invalid(format!(
                        "path {n}: {what} mode index {i} out of range or of wrong type"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Keeps the paths for which `keep` is true, drops modes no remaining path references and
    /// re-numbers all indices.
    #[must_use]
    pub fn retain_paths(&self, keep: impl Fn(usize, &SavedPath) -> bool) -> SavedTopology {
        let paths: Vec<&SavedPath> = self
            .paths
            .iter()
            .enumerate()
            .filter(|(i, p)| keep(*i, p))
            .map(|(_, p)| p)
            .collect();
        let used: HashSet<u32> = paths.iter().flat_map(|p| p.mode_refs()).collect();
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut modes = Vec::new();
        for (i, m) in self.modes.iter().enumerate() {
            if used.contains(&(i as u32)) {
                remap.insert(i as u32, modes.len() as u32);
                modes.push(m.clone());
            }
        }
        let fix = |o: Option<u32>| o.and_then(|i| remap.get(&i).copied());
        let paths = paths
            .into_iter()
            .map(|p| {
                let mut p = p.clone();
                p.source.mode_idx = fix(p.source.mode_idx);
                p.target.mode_idx = fix(p.target.mode_idx);
                p.target.desktop_idx = fix(p.target.desktop_idx);
                p
            })
            .collect();
        SavedTopology {
            version: self.version,
            virtual_aware: self.virtual_aware,
            paths,
            modes,
        }
    }

    /// Top-left position of the path's source in the virtual desktop.
    #[must_use]
    pub fn source_position(&self, path: usize) -> Option<(i32, i32)> {
        let i = self.paths.get(path)?.source.mode_idx?;
        match &self.modes.get(i as usize)?.body {
            ModeBody::Source { x, y, .. } => Some((*x, *y)),
            _ => None,
        }
    }

    /// Size of the path's source mode.
    #[must_use]
    pub fn source_size(&self, path: usize) -> Option<(u32, u32)> {
        let i = self.paths.get(path)?.source.mode_idx?;
        match &self.modes.get(i as usize)?.body {
            ModeBody::Source { width, height, .. } => Some((*width, *height)),
            _ => None,
        }
    }

    /// The primary display is the one whose source sits at (0, 0).
    #[must_use]
    pub fn primary_path(&self) -> Option<usize> {
        (0..self.paths.len()).find(|&i| self.source_position(i) == Some((0, 0)))
    }

    /// Makes `path` primary by shifting every source so that its source ends up at (0, 0).
    /// Returns false when the path has no source mode (inactive).
    pub fn make_primary(&mut self, path: usize) -> bool {
        let Some((dx, dy)) = self.source_position(path) else {
            return false;
        };
        for m in &mut self.modes {
            if let ModeBody::Source { x, y, .. } = &mut m.body {
                *x -= dx;
                *y -= dy;
            }
        }
        true
    }

    /// Indices of paths whose target is an internal panel.
    #[must_use]
    pub fn internal_paths(&self) -> Vec<usize> {
        (0..self.paths.len())
            .filter(|&i| self.paths[i].is_internal())
            .collect()
    }

    /// Same configuration without the internal panel(s). `None` if that would leave no path.
    #[must_use]
    pub fn without_internal(&self) -> Option<SavedTopology> {
        let t = self.retain_paths(|_, p| !p.is_internal());
        (!t.paths.is_empty()).then_some(t)
    }
}

// ---------------------------------------------------------------------------------------------
// Re-matching after reboot / hot-plug

/// A target as enumerated now (from `QDC_ALL_PATHS`), with the same identity data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentTarget {
    pub adapter: Luid,
    pub id: u32,
    pub output_technology: i32,
    pub available: bool,
    pub identity: TargetIdentity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchOutcome {
    /// Saved adapter LUID -> LUID now.
    pub luid_map: HashMap<Luid, Luid>,
    /// Saved (adapter, target id) -> (adapter, target id) now.
    pub target_map: HashMap<(Luid, u32), (Luid, u32)>,
    /// Saved path indices whose monitor is not present any more.
    pub unmatched: Vec<usize>,
}

impl MatchOutcome {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unmatched.is_empty()
    }

    /// True when nothing needs to be rewritten (all LUIDs and ids unchanged).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.unmatched.is_empty()
            && self.luid_map.iter().all(|(a, b)| a == b)
            && self.target_map.iter().all(|(a, b)| a == b)
    }
}

fn eq_ci(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// How well a saved target matches a current one (0 = not at all).
fn score(s: &SavedTarget, c: &CurrentTarget) -> u32 {
    let (si, ci) = (&s.identity, &c.identity);
    if eq_ci(&si.device_path, &ci.device_path) {
        return 100;
    }
    if eq_ci(&si.adapter_path, &ci.adapter_path) && s.id == c.id {
        return 80;
    }
    let edid = si.edid_manufacture_id != 0
        && si.edid_manufacture_id == ci.edid_manufacture_id
        && si.edid_product_code == ci.edid_product_code;
    if edid && si.connector_instance == ci.connector_instance && s.id == c.id {
        return 60;
    }
    if edid && s.id == c.id {
        return 50;
    }
    // Same adapter LUID and target id (no reboot between save and restore) and same technology.
    if s.adapter == c.adapter && s.id == c.id && s.output_technology == c.output_technology {
        return 30;
    }
    0
}

/// Matches the targets of `saved` to `current` (greedy by score, each current target once).
#[must_use]
pub fn match_targets(saved: &SavedTopology, current: &[CurrentTarget]) -> MatchOutcome {
    // Distinct saved targets (a clone group lists a target once per path anyway).
    let mut cands: Vec<(u32, usize, usize)> = Vec::new();
    for (pi, p) in saved.paths.iter().enumerate() {
        for (ci, c) in current.iter().enumerate() {
            if !c.available {
                continue;
            }
            let sc = score(&p.target, c);
            if sc > 0 {
                cands.push((sc, pi, ci));
            }
        }
    }
    // Highest score first; ties by path order for determinism.
    cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut used_paths = HashSet::new();
    let mut used_cur = HashSet::new();
    let mut out = MatchOutcome::default();
    for (_, pi, ci) in cands {
        if used_paths.contains(&pi) || used_cur.contains(&ci) {
            continue;
        }
        used_paths.insert(pi);
        used_cur.insert(ci);
        let p = &saved.paths[pi];
        let c = &current[ci];
        out.target_map.insert(p.target_key(), (c.adapter, c.id));
        // First match wins for the adapter mapping; conflicting later ones are logged only.
        match out.luid_map.get(&p.target.adapter) {
            None => {
                out.luid_map.insert(p.target.adapter, c.adapter);
            }
            Some(prev) if *prev != c.adapter => {
                tracing::warn!(saved = ?p.target.adapter, "targets of one saved adapter map to different adapters now");
            }
            _ => {}
        }
    }
    out.unmatched = (0..saved.paths.len())
        .filter(|i| !used_paths.contains(i))
        .collect();
    out
}

/// Rewrites `saved` for the monitors present now: drops unmatched paths and replaces adapter
/// LUIDs / target ids. Source ids are per adapter and stay as saved.
#[must_use]
pub fn remap_topology(saved: &SavedTopology, m: &MatchOutcome) -> SavedTopology {
    let drop: HashSet<usize> = m.unmatched.iter().copied().collect();
    let mut t = saved.retain_paths(|i, _| !drop.contains(&i));
    let luid = |l: Luid| m.luid_map.get(&l).copied().unwrap_or(l);
    for p in &mut t.paths {
        if let Some((a, id)) = m.target_map.get(&p.target_key()) {
            p.target.adapter = *a;
            p.target.id = *id;
        }
        p.source.adapter = luid(p.source.adapter);
    }
    for md in &mut t.modes {
        md.adapter = luid(md.adapter);
    }
    t
}

// ---------------------------------------------------------------------------------------------
// Tests with synthetic data

#[cfg(test)]
pub(crate) mod testdata {
    use super::*;

    pub fn luid(low: u32) -> Luid {
        Luid { high: 0, low }
    }

    pub fn src_mode(adapter: Luid, id: u32, w: u32, h: u32, x: i32, y: i32) -> SavedMode {
        SavedMode {
            info_type: INFO_TYPE_SOURCE,
            id,
            adapter,
            body: ModeBody::Source {
                width: w,
                height: h,
                pixel_format: 4,
                x,
                y,
            },
        }
    }

    pub fn tgt_mode(adapter: Luid, id: u32, w: u32, h: u32) -> SavedMode {
        SavedMode {
            info_type: INFO_TYPE_TARGET,
            id,
            adapter,
            body: ModeBody::Target {
                pixel_rate: u64::from(w) * u64::from(h) * 60,
                h_sync: (1, 1),
                v_sync: (60, 1),
                active: (w, h),
                total: (w, h),
                signal_flags: 0,
                scanline_ordering: 1,
            },
        }
    }

    pub fn ident(adapter_path: &str, dev: &str, edid: (u16, u16), conn: u32) -> TargetIdentity {
        TargetIdentity {
            adapter_path: adapter_path.into(),
            device_path: dev.into(),
            friendly_name: dev.into(),
            edid_manufacture_id: edid.0,
            edid_product_code: edid.1,
            connector_instance: conn,
        }
    }

    pub fn path(
        adapter: Luid,
        src_id: u32,
        tgt_id: u32,
        tech: i32,
        src_mode: u32,
        tgt_mode: u32,
        identity: TargetIdentity,
    ) -> SavedPath {
        SavedPath {
            source: SavedSource {
                adapter,
                id: src_id,
                mode_idx: Some(src_mode),
                clone_group: None,
                status_flags: 1,
            },
            target: SavedTarget {
                adapter,
                id: tgt_id,
                mode_idx: Some(tgt_mode),
                desktop_idx: None,
                output_technology: tech,
                rotation: 1,
                scaling: 1,
                refresh_num: 60,
                refresh_den: 1,
                scanline_ordering: 1,
                target_available: true,
                status_flags: 1,
                identity,
            },
            flags: 1,
        }
    }

    /// Laptop panel (primary, 1920x1200) plus an external 5120x1440 monitor on its right.
    pub fn laptop_with_external() -> SavedTopology {
        let a = luid(0x1111);
        SavedTopology {
            version: FORMAT_VERSION,
            virtual_aware: false,
            paths: vec![
                path(
                    a,
                    0,
                    4352,
                    OUTPUT_INTERNAL,
                    0,
                    1,
                    ident("pci#a", "\\\\?\\DISPLAY#LGD05A8#1", (0x1E6D, 0x05A8), 0),
                ),
                path(
                    a,
                    1,
                    4353,
                    10,
                    2,
                    3,
                    ident("pci#a", "\\\\?\\DISPLAY#SAM7001#2", (0x4C2D, 0x7001), 1),
                ),
            ],
            modes: vec![
                src_mode(a, 0, 1920, 1200, 0, 0),
                tgt_mode(a, 4352, 1920, 1200),
                src_mode(a, 1, 5120, 1440, 1920, 0),
                tgt_mode(a, 4353, 5120, 1440),
            ],
        }
    }

    pub fn current_of(t: &SavedTopology) -> Vec<CurrentTarget> {
        t.paths
            .iter()
            .map(|p| CurrentTarget {
                adapter: p.target.adapter,
                id: p.target.id,
                output_technology: p.target.output_technology,
                available: true,
                identity: p.target.identity.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testdata::*;
    use super::*;

    #[test]
    fn json_roundtrip_and_validation() {
        let t = laptop_with_external();
        let s = t.to_json().unwrap();
        assert_eq!(SavedTopology::from_json(&s).unwrap(), t);

        let mut bad = t.clone();
        bad.paths[0].source.mode_idx = Some(99);
        assert!(bad.validate().is_err());
        let mut wrong_type = t.clone();
        wrong_type.paths[0].source.mode_idx = Some(1); // a target mode
        assert!(wrong_type.validate().is_err());
        let mut v = t.clone();
        v.version = 7;
        assert!(matches!(
            SavedTopology::from_json(&v.to_json().unwrap()),
            Err(TopologyError::Version(7))
        ));
        assert!(SavedTopology::from_json("{").is_err());
        let mut empty = t;
        empty.paths.clear();
        assert!(empty.validate().is_err());
    }

    #[test]
    fn index_packing() {
        for aware in [false, true] {
            for (c, m) in [
                (None, None),
                (Some(0), Some(3)),
                (Some(2), None),
                (None, Some(7)),
            ] {
                let raw = pack_source_idx(aware, c, m);
                let (c2, m2) = unpack_source_idx(aware, raw);
                assert_eq!(m2, m, "aware={aware}");
                if aware {
                    assert_eq!(c2, c);
                }
                let raw = pack_target_idx(aware, c, m);
                let (d2, t2) = unpack_target_idx(aware, raw);
                assert_eq!(t2, m);
                if aware {
                    assert_eq!(d2, c);
                }
            }
        }
        // layout: low 16 bits first member, high 16 bits second member
        assert_eq!(pack_source_idx(true, Some(1), Some(2)), 0x0002_0001);
        assert_eq!(pack_target_idx(true, Some(5), Some(6)), 0x0006_0005);
        assert_eq!(pack_source_idx(false, None, None), IDX_INVALID);
    }

    #[test]
    fn dropping_the_internal_panel_compacts_modes() {
        let t = laptop_with_external();
        assert_eq!(t.internal_paths(), [0]);
        let w = t.without_internal().unwrap();
        assert_eq!(w.paths.len(), 1);
        assert_eq!(w.modes.len(), 2);
        assert_eq!(w.paths[0].source.mode_idx, Some(0));
        assert_eq!(w.paths[0].target.mode_idx, Some(1));
        assert!(w.validate().is_ok());
        assert_eq!(w.source_size(0), Some((5120, 1440)));
    }

    #[test]
    fn cannot_drop_the_only_display() {
        let t = laptop_with_external().retain_paths(|i, _| i == 0);
        assert!(t.without_internal().is_none());
    }

    #[test]
    fn shared_modes_survive_dropping_one_path() {
        // clone group: two paths share one source mode
        let mut t = laptop_with_external();
        t.paths[1].source.mode_idx = Some(0);
        t.paths[1].source.id = 0;
        let kept = t.retain_paths(|i, _| i == 1);
        assert_eq!(kept.paths[0].source.mode_idx, Some(0));
        assert!(kept.validate().is_ok());
    }

    #[test]
    fn make_primary_shifts_all_sources() {
        let mut t = laptop_with_external();
        assert_eq!(t.primary_path(), Some(0));
        assert!(t.make_primary(1));
        assert_eq!(t.primary_path(), Some(1));
        assert_eq!(t.source_position(0), Some((-1920, 0)));
        assert_eq!(t.source_position(1), Some((0, 0)));
        // inactive path cannot be made primary
        t.paths[0].source.mode_idx = None;
        assert!(!t.make_primary(0));
    }

    #[test]
    fn unchanged_system_matches_as_identity() {
        let t = laptop_with_external();
        let m = match_targets(&t, &current_of(&t));
        assert!(m.is_complete() && m.is_identity());
        assert_eq!(remap_topology(&t, &m), t);
    }

    #[test]
    fn matches_after_reboot_with_new_luids_and_ids() {
        let t = laptop_with_external();
        let new_a = luid(0x2222);
        let mut cur = current_of(&t);
        // adapter LUID changed; the target ids of the external monitor changed, too
        for c in &mut cur {
            c.adapter = new_a;
        }
        cur[1].id = 4999;
        cur[1].identity.connector_instance = 9;
        cur.reverse(); // enumeration order must not matter
        let m = match_targets(&t, &cur);
        assert!(m.is_complete(), "{m:?}");
        assert!(!m.is_identity());
        let r = remap_topology(&t, &m);
        assert_eq!(r.paths[0].target.adapter, new_a);
        assert_eq!(r.paths[0].source.adapter, new_a);
        assert_eq!(r.paths[1].target.id, 4999);
        assert!(r.modes.iter().all(|m| m.adapter == new_a));
        assert!(r.validate().is_ok());
    }

    #[test]
    fn falls_back_to_edid_when_device_path_changed() {
        let t = laptop_with_external();
        let mut cur = current_of(&t);
        for c in &mut cur {
            c.identity.device_path = format!("{}-x", c.identity.device_path);
            c.identity.adapter_path = "other".into();
        }
        let m = match_targets(&t, &cur);
        assert!(m.is_complete(), "{m:?}");
    }

    #[test]
    fn unplugged_monitor_is_dropped_not_fatal() {
        let t = laptop_with_external();
        let mut cur = current_of(&t);
        cur.remove(1); // the external monitor is gone
        let m = match_targets(&t, &cur);
        assert_eq!(m.unmatched, [1]);
        let r = remap_topology(&t, &m);
        assert_eq!(r.paths.len(), 1);
        assert!(r.paths[0].is_internal());
        assert!(r.validate().is_ok());
        assert_eq!(r.modes.len(), 2);
    }

    #[test]
    fn unavailable_targets_do_not_match() {
        let t = laptop_with_external();
        let mut cur = current_of(&t);
        cur[1].available = false;
        let m = match_targets(&t, &cur);
        assert_eq!(m.unmatched, [1]);
    }

    #[test]
    fn two_identical_monitors_are_not_mixed_up() {
        // two same-model monitors: distinct device paths keep them apart
        let a = luid(1);
        let id = |dev: &str| ident("pci#a", dev, (0x4C2D, 0x7001), 0);
        let t = SavedTopology {
            version: FORMAT_VERSION,
            virtual_aware: false,
            paths: vec![
                path(a, 0, 10, 10, 0, 1, id("DISPLAY#SAM7001#L")),
                path(a, 1, 11, 10, 2, 3, id("DISPLAY#SAM7001#R")),
            ],
            modes: vec![
                src_mode(a, 0, 2560, 1440, 0, 0),
                tgt_mode(a, 10, 2560, 1440),
                src_mode(a, 1, 2560, 1440, 2560, 0),
                tgt_mode(a, 11, 2560, 1440),
            ],
        };
        let mut cur = current_of(&t);
        cur[0].id = 21; // ids swapped by the OS
        cur[1].id = 20;
        let m = match_targets(&t, &cur);
        assert_eq!(m.target_map[&(a, 10)], (a, 21));
        assert_eq!(m.target_map[&(a, 11)], (a, 20));
    }

    #[test]
    fn technology_classes() {
        assert!(is_internal_technology(OUTPUT_INTERNAL));
        assert!(is_internal_technology(OUTPUT_DISPLAYPORT_EMBEDDED));
        assert!(!is_internal_technology(10));
        assert!(is_indirect_technology(OUTPUT_INDIRECT_WIRED));
        assert!(!is_indirect_technology(5));
    }
}
