//! Connecting and Configuring Displays (CCD) access: query the display configuration, convert it
//! to and from the platform-neutral model of `deskbridge-display`, and apply configurations with
//! guard rails and fallbacks.
//!
//! Nothing here changes the system unless an `apply*` / `restore*` function is called.

use std::collections::HashMap;
use std::mem::size_of;

use deskbridge_display::topology::{
    CurrentTarget, FORMAT_VERSION, INFO_TYPE_DESKTOP_IMAGE, INFO_TYPE_SOURCE, INFO_TYPE_TARGET,
    Luid, ModeBody, SavedMode, SavedPath, SavedSource, SavedTarget, SavedTopology, TargetIdentity,
    match_targets, pack_source_idx, pack_target_idx, remap_topology, unpack_source_idx,
    unpack_target_idx,
};
use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_2DREGION, DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DESKTOP_IMAGE_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_0,
    DISPLAYCONFIG_MODE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_PATH_SOURCE_INFO, DISPLAYCONFIG_PATH_SOURCE_INFO_0,
    DISPLAYCONFIG_PATH_TARGET_INFO, DISPLAYCONFIG_PATH_TARGET_INFO_0, DISPLAYCONFIG_PIXELFORMAT,
    DISPLAYCONFIG_RATIONAL, DISPLAYCONFIG_ROTATION, DISPLAYCONFIG_SCALING,
    DISPLAYCONFIG_SCANLINE_ORDERING, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_SOURCE_MODE,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DISPLAYCONFIG_TARGET_MODE,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DISPLAYCONFIG_VIDEO_SIGNAL_INFO,
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO_0, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QDC_VIRTUAL_MODE_AWARE, QUERY_DISPLAY_CONFIG_FLAGS,
    QueryDisplayConfig, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_NO_OPTIMIZATION, SDC_SAVE_TO_DATABASE,
    SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_INTERNAL, SDC_USE_DATABASE_CURRENT,
    SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE, SDC_VIRTUAL_MODE_AWARE,
    SET_DISPLAY_CONFIG_FLAGS, SetDisplayConfig,
};
use windows::Win32::Foundation::{
    BOOL, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, LUID, POINTL, RECTL,
};

pub(super) fn luid_to(l: LUID) -> Luid {
    Luid {
        high: l.HighPart,
        low: l.LowPart,
    }
}

pub(super) fn luid_from(l: Luid) -> LUID {
    LUID {
        LowPart: l.low,
        HighPart: l.high,
    }
}

pub(super) fn wide(w: &[u16]) -> String {
    let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

pub(super) fn os_err(code: i64, what: &str) -> PlatformError {
    PlatformError::Os {
        code,
        msg: what.to_owned(),
    }
}

/// Raw CCD data as returned by the OS.
pub(super) struct Raw {
    pub aware: bool,
    pub paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    pub modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

type PathsAndModes = (Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>);

fn query_flags(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<PathsAndModes> {
    // The number of paths can change between the size query and the data query: retry.
    for _ in 0..5 {
        let (mut np, mut nm) = (0u32, 0u32);
        // SAFETY: valid out pointers.
        let rc = unsafe { GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) };
        if rc != ERROR_SUCCESS {
            return Err(os_err(i64::from(rc.0), "GetDisplayConfigBufferSizes"));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        // SAFETY: the buffers have exactly `np` / `nm` elements; a `None` topology id is valid
        // for the ALL_PATHS / ONLY_ACTIVE_PATHS queries.
        let rc = unsafe {
            QueryDisplayConfig(
                flags,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            )
        };
        if rc == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if rc != ERROR_SUCCESS {
            return Err(os_err(i64::from(rc.0), "QueryDisplayConfig"));
        }
        paths.truncate(np as usize);
        modes.truncate(nm as usize);
        return Ok((paths, modes));
    }
    Err(PlatformError::other(
        "QueryDisplayConfig: display configuration keeps changing",
    ))
}

/// Queries the active (or all) paths, virtual-mode-aware when the OS supports it.
pub(super) fn query(all_paths: bool, prefer_aware: bool) -> Result<Raw> {
    let base = if all_paths {
        QDC_ALL_PATHS
    } else {
        QDC_ONLY_ACTIVE_PATHS
    };
    if prefer_aware {
        match query_flags(base | QDC_VIRTUAL_MODE_AWARE) {
            Ok((paths, modes)) => {
                return Ok(Raw {
                    aware: true,
                    paths,
                    modes,
                });
            }
            Err(PlatformError::Os { code, .. }) if code == i64::from(ERROR_INVALID_PARAMETER.0) => {
                tracing::debug!(
                    "virtual-mode-aware CCD query not supported; using the legacy form"
                );
            }
            Err(e) => return Err(e),
        }
    }
    let (paths, modes) = query_flags(base)?;
    Ok(Raw {
        aware: false,
        paths,
        modes,
    })
}

// ---------------------------------------------------------------------------------------------
// Names

fn device_header(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: adapter,
        id,
    }
}

fn target_name(adapter: LUID, id: u32) -> Option<DISPLAYCONFIG_TARGET_DEVICE_NAME> {
    let mut t = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
            adapter,
            id,
        ),
        ..Default::default()
    };
    // SAFETY: header size/type describe the struct we pass.
    (unsafe { DisplayConfigGetDeviceInfo(&mut t.header) } == 0).then_some(t)
}

pub(super) fn adapter_path(adapter: LUID) -> String {
    let mut a = DISPLAYCONFIG_ADAPTER_NAME {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
            size_of::<DISPLAYCONFIG_ADAPTER_NAME>(),
            adapter,
            0,
        ),
        ..Default::default()
    };
    // SAFETY: header size/type describe the struct we pass.
    if unsafe { DisplayConfigGetDeviceInfo(&mut a.header) } == 0 {
        wide(&a.adapterDevicePath)
    } else {
        String::new()
    }
}

/// GDI device name (`\\.\DISPLAY3`) of a path source.
pub(super) fn source_gdi_name(adapter: LUID, source_id: u32) -> Option<String> {
    let mut s = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
            adapter,
            source_id,
        ),
        ..Default::default()
    };
    // SAFETY: header size/type describe the struct we pass.
    (unsafe { DisplayConfigGetDeviceInfo(&mut s.header) } == 0).then(|| wide(&s.viewGdiDeviceName))
}

struct NameCache {
    targets: HashMap<(Luid, u32), TargetIdentity>,
    adapters: HashMap<Luid, String>,
}

impl NameCache {
    fn new() -> Self {
        Self {
            targets: HashMap::new(),
            adapters: HashMap::new(),
        }
    }

    fn identity(&mut self, adapter: LUID, id: u32) -> TargetIdentity {
        let key = (luid_to(adapter), id);
        if let Some(i) = self.targets.get(&key) {
            return i.clone();
        }
        let ap = self
            .adapters
            .entry(key.0)
            .or_insert_with(|| adapter_path(adapter))
            .clone();
        let ident = match target_name(adapter, id) {
            Some(t) => TargetIdentity {
                adapter_path: ap,
                device_path: wide(&t.monitorDevicePath),
                friendly_name: wide(&t.monitorFriendlyDeviceName),
                edid_manufacture_id: t.edidManufactureId,
                edid_product_code: t.edidProductCodeId,
                connector_instance: t.connectorInstance,
            },
            None => TargetIdentity {
                adapter_path: ap,
                ..TargetIdentity::default()
            },
        };
        self.targets.insert(key, ident.clone());
        ident
    }
}

// ---------------------------------------------------------------------------------------------
// OS structs <-> model

pub(super) fn to_saved(raw: &Raw) -> Result<SavedTopology> {
    let mut names = NameCache::new();
    let mut paths = Vec::with_capacity(raw.paths.len());
    for p in &raw.paths {
        // SAFETY: both union members are `u32`-sized; reading the `u32` view is always valid.
        let (s_raw, t_raw) = unsafe {
            (
                p.sourceInfo.Anonymous.modeInfoIdx,
                p.targetInfo.Anonymous.modeInfoIdx,
            )
        };
        let (clone_group, source_mode) = unpack_source_idx(raw.aware, s_raw);
        let (desktop_idx, target_mode) = unpack_target_idx(raw.aware, t_raw);
        paths.push(SavedPath {
            source: SavedSource {
                adapter: luid_to(p.sourceInfo.adapterId),
                id: p.sourceInfo.id,
                mode_idx: source_mode,
                clone_group,
                status_flags: p.sourceInfo.statusFlags,
            },
            target: SavedTarget {
                adapter: luid_to(p.targetInfo.adapterId),
                id: p.targetInfo.id,
                mode_idx: target_mode,
                desktop_idx,
                output_technology: p.targetInfo.outputTechnology.0,
                rotation: p.targetInfo.rotation.0,
                scaling: p.targetInfo.scaling.0,
                refresh_num: p.targetInfo.refreshRate.Numerator,
                refresh_den: p.targetInfo.refreshRate.Denominator,
                scanline_ordering: p.targetInfo.scanLineOrdering.0,
                target_available: p.targetInfo.targetAvailable.as_bool(),
                status_flags: p.targetInfo.statusFlags,
                identity: names.identity(p.targetInfo.adapterId, p.targetInfo.id),
            },
            flags: p.flags,
        });
    }
    let mut modes = Vec::with_capacity(raw.modes.len());
    for m in &raw.modes {
        let body = match m.infoType {
            DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE => {
                // SAFETY: `infoType` says the `sourceMode` member is the active one.
                let s = unsafe { m.Anonymous.sourceMode };
                ModeBody::Source {
                    width: s.width,
                    height: s.height,
                    pixel_format: s.pixelFormat.0,
                    x: s.position.x,
                    y: s.position.y,
                }
            }
            DISPLAYCONFIG_MODE_INFO_TYPE_TARGET => {
                // SAFETY: `infoType` says the `targetMode` member is the active one.
                let t = unsafe { m.Anonymous.targetMode.targetVideoSignalInfo };
                ModeBody::Target {
                    pixel_rate: t.pixelRate,
                    h_sync: (t.hSyncFreq.Numerator, t.hSyncFreq.Denominator),
                    v_sync: (t.vSyncFreq.Numerator, t.vSyncFreq.Denominator),
                    active: (t.activeSize.cx, t.activeSize.cy),
                    total: (t.totalSize.cx, t.totalSize.cy),
                    // SAFETY: both union members are `u32`-sized.
                    signal_flags: unsafe { t.Anonymous.videoStandard },
                    scanline_ordering: t.scanLineOrdering.0,
                }
            }
            DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE => {
                // SAFETY: `infoType` says the `desktopImageInfo` member is the active one.
                let d = unsafe { m.Anonymous.desktopImageInfo };
                ModeBody::DesktopImage {
                    path_source_size: (d.PathSourceSize.x, d.PathSourceSize.y),
                    region: (
                        d.DesktopImageRegion.left,
                        d.DesktopImageRegion.top,
                        d.DesktopImageRegion.right,
                        d.DesktopImageRegion.bottom,
                    ),
                    clip: (
                        d.DesktopImageClip.left,
                        d.DesktopImageClip.top,
                        d.DesktopImageClip.right,
                        d.DesktopImageClip.bottom,
                    ),
                }
            }
            other => {
                return Err(PlatformError::other(format!(
                    "unknown display mode info type {}",
                    other.0
                )));
            }
        };
        modes.push(SavedMode {
            info_type: m.infoType.0,
            id: m.id,
            adapter: luid_to(m.adapterId),
            body,
        });
    }
    let t = SavedTopology {
        version: FORMAT_VERSION,
        virtual_aware: raw.aware,
        paths,
        modes,
    };
    t.validate()
        .map_err(|e| PlatformError::other(e.to_string()))?;
    Ok(t)
}

pub(super) fn from_saved(t: &SavedTopology) -> Result<Raw> {
    let aware = t.virtual_aware;
    let paths = t
        .paths
        .iter()
        .map(|p| DISPLAYCONFIG_PATH_INFO {
            sourceInfo: DISPLAYCONFIG_PATH_SOURCE_INFO {
                adapterId: luid_from(p.source.adapter),
                id: p.source.id,
                Anonymous: DISPLAYCONFIG_PATH_SOURCE_INFO_0 {
                    modeInfoIdx: pack_source_idx(aware, p.source.clone_group, p.source.mode_idx),
                },
                statusFlags: p.source.status_flags,
            },
            targetInfo: DISPLAYCONFIG_PATH_TARGET_INFO {
                adapterId: luid_from(p.target.adapter),
                id: p.target.id,
                Anonymous: DISPLAYCONFIG_PATH_TARGET_INFO_0 {
                    modeInfoIdx: pack_target_idx(aware, p.target.desktop_idx, p.target.mode_idx),
                },
                outputTechnology: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY(p.target.output_technology),
                rotation: DISPLAYCONFIG_ROTATION(p.target.rotation),
                scaling: DISPLAYCONFIG_SCALING(p.target.scaling),
                refreshRate: DISPLAYCONFIG_RATIONAL {
                    Numerator: p.target.refresh_num,
                    Denominator: p.target.refresh_den,
                },
                scanLineOrdering: DISPLAYCONFIG_SCANLINE_ORDERING(p.target.scanline_ordering),
                targetAvailable: BOOL::from(p.target.target_available),
                statusFlags: p.target.status_flags,
            },
            flags: p.flags,
        })
        .collect();
    let mut modes = Vec::with_capacity(t.modes.len());
    for m in &t.modes {
        let (want, union) = match &m.body {
            ModeBody::Source {
                width,
                height,
                pixel_format,
                x,
                y,
            } => (
                INFO_TYPE_SOURCE,
                DISPLAYCONFIG_MODE_INFO_0 {
                    sourceMode: DISPLAYCONFIG_SOURCE_MODE {
                        width: *width,
                        height: *height,
                        pixelFormat: DISPLAYCONFIG_PIXELFORMAT(*pixel_format),
                        position: POINTL { x: *x, y: *y },
                    },
                },
            ),
            ModeBody::Target {
                pixel_rate,
                h_sync,
                v_sync,
                active,
                total,
                signal_flags,
                scanline_ordering,
            } => (
                INFO_TYPE_TARGET,
                DISPLAYCONFIG_MODE_INFO_0 {
                    targetMode: DISPLAYCONFIG_TARGET_MODE {
                        targetVideoSignalInfo: DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
                            pixelRate: *pixel_rate,
                            hSyncFreq: DISPLAYCONFIG_RATIONAL {
                                Numerator: h_sync.0,
                                Denominator: h_sync.1,
                            },
                            vSyncFreq: DISPLAYCONFIG_RATIONAL {
                                Numerator: v_sync.0,
                                Denominator: v_sync.1,
                            },
                            activeSize: DISPLAYCONFIG_2DREGION {
                                cx: active.0,
                                cy: active.1,
                            },
                            totalSize: DISPLAYCONFIG_2DREGION {
                                cx: total.0,
                                cy: total.1,
                            },
                            Anonymous: DISPLAYCONFIG_VIDEO_SIGNAL_INFO_0 {
                                videoStandard: *signal_flags,
                            },
                            scanLineOrdering: DISPLAYCONFIG_SCANLINE_ORDERING(*scanline_ordering),
                        },
                    },
                },
            ),
            ModeBody::DesktopImage {
                path_source_size,
                region,
                clip,
            } => (
                INFO_TYPE_DESKTOP_IMAGE,
                DISPLAYCONFIG_MODE_INFO_0 {
                    desktopImageInfo: DISPLAYCONFIG_DESKTOP_IMAGE_INFO {
                        PathSourceSize: POINTL {
                            x: path_source_size.0,
                            y: path_source_size.1,
                        },
                        DesktopImageRegion: RECTL {
                            left: region.0,
                            top: region.1,
                            right: region.2,
                            bottom: region.3,
                        },
                        DesktopImageClip: RECTL {
                            left: clip.0,
                            top: clip.1,
                            right: clip.2,
                            bottom: clip.3,
                        },
                    },
                },
            ),
        };
        if m.info_type != want {
            return Err(PlatformError::other(
                "saved mode: info type does not match its body",
            ));
        }
        modes.push(DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE(m.info_type),
            id: m.id,
            adapterId: luid_from(m.adapter),
            Anonymous: union,
        });
    }
    Ok(Raw {
        aware,
        paths,
        modes,
    })
}

// ---------------------------------------------------------------------------------------------
// High level queries

/// The active configuration as model.
pub(super) fn query_active_saved() -> Result<SavedTopology> {
    to_saved(&query(false, true)?)
}

/// Every connectable target (deduplicated), with identity.
pub(super) fn current_targets() -> Result<Vec<CurrentTarget>> {
    let raw = query(true, false)?;
    let mut names = NameCache::new();
    let mut seen: HashMap<(Luid, u32), usize> = HashMap::new();
    let mut out: Vec<CurrentTarget> = Vec::new();
    for p in &raw.paths {
        let key = (luid_to(p.targetInfo.adapterId), p.targetInfo.id);
        let available = p.targetInfo.targetAvailable.as_bool();
        if let Some(&i) = seen.get(&key) {
            out[i].available |= available;
            continue;
        }
        seen.insert(key, out.len());
        out.push(CurrentTarget {
            adapter: key.0,
            id: key.1,
            output_technology: p.targetInfo.outputTechnology.0,
            available,
            identity: names.identity(p.targetInfo.adapterId, p.targetInfo.id),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Applying

/// `SetDisplayConfig` with a model topology. A non-zero return code is the error.
pub(super) fn apply_saved(
    t: &SavedTopology,
    flags: SET_DISPLAY_CONFIG_FLAGS,
    with_modes: bool,
) -> std::result::Result<(), i32> {
    let raw = from_saved(t).map_err(|_| -1)?;
    let mut flags = flags | SDC_USE_SUPPLIED_DISPLAY_CONFIG;
    if raw.aware {
        flags |= SDC_VIRTUAL_MODE_AWARE;
    }
    // SAFETY: the slices are valid for the call; the OS copies the data.
    let rc = unsafe {
        SetDisplayConfig(
            Some(&raw.paths),
            if with_modes { Some(&raw.modes) } else { None },
            flags,
        )
    };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

/// Validates `t` without applying it (`SDC_VALIDATE`; never `SDC_APPLY`).
pub(super) fn validate_saved(t: &SavedTopology) -> std::result::Result<(), i32> {
    apply_saved(t, SDC_VALIDATE | SDC_ALLOW_CHANGES, true)
}

/// Flags used to switch to the focus configuration. Not saved to the CCD database, so a crash
/// plus reboot falls back to the persisted original layout by itself.
pub(super) fn focus_apply_flags() -> SET_DISPLAY_CONFIG_FLAGS {
    SDC_APPLY | SDC_ALLOW_CHANGES | SDC_NO_OPTIMIZATION
}

fn set_topology(flags: SET_DISPLAY_CONFIG_FLAGS) -> std::result::Result<(), i32> {
    // SAFETY: topology flags need no arrays.
    let rc = unsafe { SetDisplayConfig(None, None, SDC_APPLY | flags) };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

/// At least one active path exists.
pub(super) fn has_active_display() -> bool {
    query(false, false).is_ok_and(|r| !r.paths.is_empty())
}

/// Restores a saved configuration, trying progressively less exact methods. Returns the method
/// used. `saved = None` goes straight to the system fallbacks (database / extend).
///
/// Guard rail: success means "the OS accepted it **and** at least one display is active".
pub(super) fn restore(saved: Option<&SavedTopology>) -> Result<&'static str> {
    if let Some(saved) = saved {
        match current_targets() {
            Ok(current) => {
                let outcome = match_targets(saved, &current);
                if outcome.unmatched.len() == saved.paths.len() {
                    tracing::warn!("none of the saved monitors is present any more");
                } else {
                    if !outcome.is_complete() {
                        tracing::warn!(
                            missing = outcome.unmatched.len(),
                            "some saved monitors are gone; restoring the rest"
                        );
                    }
                    let remapped = remap_topology(saved, &outcome);
                    let attempts: [(&str, SET_DISPLAY_CONFIG_FLAGS, bool); 3] = [
                        (
                            "supplied configuration",
                            SDC_APPLY | SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES,
                            true,
                        ),
                        (
                            "supplied configuration (not saved to database)",
                            SDC_APPLY | SDC_ALLOW_CHANGES,
                            true,
                        ),
                        (
                            "supplied paths without modes",
                            SDC_APPLY | SDC_ALLOW_CHANGES,
                            false,
                        ),
                    ];
                    for (name, flags, with_modes) in attempts {
                        match apply_saved(&remapped, flags, with_modes) {
                            Ok(()) if has_active_display() => return Ok(name),
                            Ok(()) => {
                                tracing::error!(method = name, "applied, but no display is active");
                            }
                            Err(rc) => tracing::warn!(
                                method = name,
                                rc,
                                "SetDisplayConfig rejected the saved topology"
                            ),
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot enumerate display targets for matching"),
        }
    }
    // System fallbacks: persisted layout, then internal-only / extend everything.
    let internal_only =
        saved.is_some_and(|s| !s.paths.is_empty() && s.paths.iter().all(|p| p.is_internal()));
    let mut chain: Vec<(&'static str, SET_DISPLAY_CONFIG_FLAGS)> = vec![
        ("database", SDC_USE_DATABASE_CURRENT),
        (
            "database (allow changes)",
            SDC_USE_DATABASE_CURRENT | SDC_ALLOW_CHANGES,
        ),
    ];
    if internal_only {
        chain.push(("internal only", SDC_TOPOLOGY_INTERNAL));
        chain.push(("extend", SDC_TOPOLOGY_EXTEND));
    } else {
        chain.push(("extend", SDC_TOPOLOGY_EXTEND));
        chain.push(("internal only", SDC_TOPOLOGY_INTERNAL));
    }
    for (name, flags) in chain {
        match set_topology(flags) {
            Ok(()) if has_active_display() => return Ok(name),
            Ok(()) => tracing::error!(method = name, "applied, but no display is active"),
            Err(rc) => tracing::warn!(method = name, rc, "SetDisplayConfig fallback rejected"),
        }
    }
    Err(PlatformError::other(
        "could not restore the display topology by any method",
    ))
}

/// `DISPLAYCONFIG_PATH_ACTIVE`.
const PATH_ACTIVE: u32 = 0x1;
/// `DISPLAYCONFIG_PATH_MODE_IDX_INVALID` (legacy, not virtual-mode-aware layout).
const MODE_IDX_INVALID: u32 = 0xffff_ffff;

/// Adds the path of one target to the active configuration and keeps every active path as it
/// is. Unlike `SDC_TOPOLOGY_EXTEND` this does not depend on what Windows remembered for this set
/// of monitors: once a monitor was stored as "not part of the desktop" (an earlier rollback or
/// focus session), EXTEND leaves it off and the virtual monitor never came up.
pub(super) fn activate_target(adapter: Luid, target_id: u32) -> Result<()> {
    let raw = query(true, false)?;
    let is_active = |p: &DISPLAYCONFIG_PATH_INFO| p.flags & PATH_ACTIVE != 0;
    let target_of =
        |p: &DISPLAYCONFIG_PATH_INFO| (luid_to(p.targetInfo.adapterId), p.targetInfo.id);
    if raw
        .paths
        .iter()
        .any(|p| is_active(p) && target_of(p) == (adapter, target_id))
    {
        return Ok(());
    }
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> =
        raw.paths.iter().copied().filter(is_active).collect();
    let used_sources: Vec<(Luid, u32)> = paths
        .iter()
        .map(|p| (luid_to(p.sourceInfo.adapterId), p.sourceInfo.id))
        .collect();
    // Every (source, target) combination is listed; take one whose source is still free.
    let mut new = *raw
        .paths
        .iter()
        .filter(|p| !is_active(p) && target_of(p) == (adapter, target_id))
        .filter(|p| p.targetInfo.targetAvailable.as_bool())
        .find(|p| !used_sources.contains(&(luid_to(p.sourceInfo.adapterId), p.sourceInfo.id)))
        .ok_or_else(|| PlatformError::other("no free display source for the virtual monitor"))?;
    new.flags |= PATH_ACTIVE;
    new.sourceInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
    new.targetInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
    paths.push(new);
    let mut last = 0;
    for flags in [
        SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
        SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE,
    ] {
        // SAFETY: the slices are valid for the call; the OS copies the data. Mode indices of the
        // kept paths refer to `raw.modes`, the new path has none (the OS picks the modes).
        let rc = unsafe { SetDisplayConfig(Some(&paths), Some(&raw.modes), flags) };
        if rc == 0 {
            return Ok(());
        }
        last = rc;
        tracing::warn!(
            rc,
            "SetDisplayConfig rejected the activation of the virtual monitor"
        );
    }
    Err(os_err(
        i64::from(last),
        "SetDisplayConfig(activate virtual monitor)",
    ))
}

/// Activates every connected target (`SDC_TOPOLOGY_EXTEND`).
pub(super) fn extend_all() -> Result<()> {
    // SDC_ALLOW_CHANGES must not be combined with a topology flag (ERROR_INVALID_PARAMETER, 87;
    // verified with SDC_VALIDATE in tests/ccd_flags.rs).
    set_topology(SDC_TOPOLOGY_EXTEND)
        .map_err(|rc| os_err(i64::from(rc), "SetDisplayConfig(TOPOLOGY_EXTEND)"))
}
