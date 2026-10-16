//! Windows [`DisplayManager`]: topology save/restore (CCD), the virtual display driver
//! (SetupAPI device node + `vdd_settings.xml` + control pipe) and focus-mode display switching.
//!
//! Design rules ("safe by design"):
//! * every public operation is serialised by one lock, so an operation abandoned by a caller's
//!   timeout finishes before the next one starts;
//! * nothing changes the system before all prerequisites (elevation, device present, settings
//!   renderable) are checked;
//! * a failed operation rolls back to the state it started from (topology + device state);
//! * the display configuration is never applied without a validation first, and a result with
//!   zero active displays is treated as failure;
//! * the platform-neutral decisions live in `deskbridge-display` and are unit-tested there.
//!
//! The screen id returned by [`WinDisplayManager::enable_virtual`] is the id used by
//! [`crate::screen::WinScreenProvider`] (its process-wide id table is shared, the virtual monitor
//! is located by its desktop rectangle).

mod ccd;
mod device;
mod dxgi;
mod vddfile;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deskbridge_display::topology::{
    CurrentTarget, SavedTopology, is_indirect_technology, is_internal_technology,
};
use deskbridge_display::util::run_with_timeout;
use deskbridge_display::vdd::{
    self, AdapterInfo, CMD_RELOAD, DEFAULT_SETTINGS_TEMPLATE, choose_render_adapter, cmd_setgpu,
};
use deskbridge_platform::display::{DisplayManager, Mode, Topology};
use deskbridge_platform::screen::ScreenProvider;
use deskbridge_platform::{PlatformError, Result};
use parking_lot::Mutex;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CDS_TEST, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
    ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
};
use windows::core::PCWSTR;

pub use device::VddNode;

use crate::screen::WinScreenProvider;

/// Timing knobs; the defaults follow the plan (5 s for the monitor to appear).
#[derive(Debug, Clone)]
pub struct DisplayTuning {
    /// How long to wait for the new display target after enabling the device.
    pub settle_timeout: Duration,
    /// Upper bound for one SetupAPI enable/disable call.
    pub device_op_timeout: Duration,
    /// How long to wait until a display configuration change is visible.
    pub verify_timeout: Duration,
    pub poll_interval: Duration,
    /// Send `RELOAD_DRIVER` / `SETGPU` to the driver pipe when the driver runs.
    pub use_pipe: bool,
}

impl Default for DisplayTuning {
    fn default() -> Self {
        Self {
            settle_timeout: Duration::from_secs(5),
            device_op_timeout: Duration::from_secs(25),
            verify_timeout: Duration::from_secs(3),
            poll_interval: Duration::from_millis(100),
            use_pipe: true,
        }
    }
}

pub struct WinDisplayManager {
    settings_dir: PathBuf,
    tuning: DisplayTuning,
    /// Serialises every operation.
    op: Mutex<()>,
}

impl Default for WinDisplayManager {
    fn default() -> Self {
        Self::new()
    }
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Heuristic: the Desk Bridge virtual display (EDID id "MTT…", indirect output).
fn looks_like_vdd(t: &CurrentTarget) -> bool {
    is_indirect_technology(t.output_technology)
        && (t.identity.device_path.to_ascii_uppercase().contains("MTT")
            || t.identity
                .friendly_name
                .to_ascii_uppercase()
                .contains("MTT"))
}

fn device_paths(targets: &[CurrentTarget]) -> HashSet<String> {
    targets
        .iter()
        .filter(|t| t.available)
        .map(|t| lower(&t.identity.device_path))
        .collect()
}

impl WinDisplayManager {
    #[must_use]
    pub fn new() -> Self {
        Self::with_settings_dir(vddfile::default_settings_dir())
    }

    #[must_use]
    pub fn with_settings_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            settings_dir: dir.into(),
            tuning: DisplayTuning::default(),
            op: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn with_tuning(mut self, t: DisplayTuning) -> Self {
        self.tuning = t;
        self
    }

    #[must_use]
    pub fn settings_dir(&self) -> &Path {
        &self.settings_dir
    }

    // -- helpers -----------------------------------------------------------------------------

    fn require_elevated() -> Result<()> {
        if device::is_elevated() {
            Ok(())
        } else {
            Err(PlatformError::other(
                "administrator rights are required to change the virtual display device",
            ))
        }
    }

    fn device_op(&self, enable: bool) -> Result<()> {
        let label = if enable {
            "enable-device"
        } else {
            "disable-device"
        };
        run_with_timeout(label, self.tuning.device_op_timeout, move || {
            device::set_enabled(enable).map(|restart| {
                if restart {
                    tracing::warn!(
                        "Windows asks for a restart after changing the virtual display device"
                    );
                }
            })
        })
    }

    fn node(&self) -> Result<Option<VddNode>> {
        device::find_node()
    }

    fn wait_until<T>(&self, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(v) = f() {
                return Some(v);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(self.tuning.poll_interval);
        }
    }

    /// Render adapter by hint / automatic choice.
    fn choose_adapter(&self, hint: Option<&str>) -> Option<AdapterInfo> {
        match dxgi::enumerate_adapters() {
            Ok(list) => {
                let chosen = choose_render_adapter(&list, hint).cloned();
                match &chosen {
                    Some(a) => {
                        tracing::info!(adapter = %a.name, vram_mb = a.dedicated_vram >> 20, "render adapter")
                    }
                    None => {
                        tracing::warn!("no usable render adapter found; leaving the driver default")
                    }
                }
                chosen
            }
            Err(e) => {
                tracing::warn!(error = %e, "adapter enumeration failed; leaving the driver default");
                None
            }
        }
    }

    fn base_settings(&self) -> String {
        vddfile::read_settings(&self.settings_dir)
            .filter(|s| vdd::parse_count(s).is_some() || !vdd::parse_modes(s).is_empty())
            .unwrap_or_else(|| DEFAULT_SETTINGS_TEMPLATE.to_owned())
    }

    fn reload_driver_if_running(&self, node: Option<&VddNode>) {
        if self.tuning.use_pipe
            && node.is_some_and(VddNode::started)
            && let Err(e) = vddfile::send_pipe_command(CMD_RELOAD)
        {
            tracing::warn!(error = %e, "RELOAD_DRIVER failed; modes apply at the next device start");
        }
    }

    /// Finds the active path of the screen with this id (by desktop rectangle).
    fn locate_screen(&self, screen_id: u32, topo: &SavedTopology) -> Result<usize> {
        let screens = WinScreenProvider::new().screens();
        let s = screens
            .iter()
            .find(|s| s.id == screen_id)
            .ok_or_else(|| PlatformError::DeviceNotFound(format!("screen {screen_id}")))?;
        let hits: Vec<usize> = (0..topo.paths.len())
            .filter(|&i| {
                topo.source_position(i) == Some((s.x, s.y))
                    && topo.source_size(i) == Some((s.w, s.h))
            })
            .collect();
        match hits.as_slice() {
            [] => Err(PlatformError::DeviceNotFound(format!(
                "no active display path for screen {screen_id}"
            ))),
            [one] => Ok(*one),
            many => Ok(many
                .iter()
                .copied()
                .find(|&i| topo.paths[i].is_indirect())
                .unwrap_or(many[0])),
        }
    }

    /// Screen id of the monitor behind a GDI device name (matched by desktop rectangle).
    fn screen_id_for_gdi(&self, gdi: &str, friendly: &str) -> Result<u32> {
        let (x, y, w, h, _) = current_geometry(gdi)
            .ok_or_else(|| PlatformError::other(format!("cannot read the mode of {gdi}")))?;
        let found = self.wait_until(Duration::from_secs(3), || {
            let screens = WinScreenProvider::new().screens();
            let hits: Vec<_> = screens
                .iter()
                .filter(|s| s.x == x && s.y == y && s.w == w && s.h == h)
                .collect();
            hits.iter()
                .find(|s| !friendly.is_empty() && s.name == friendly)
                .or(hits.first())
                .map(|s| s.id)
        });
        found.ok_or_else(|| {
            PlatformError::DeviceNotFound(format!("{gdi} does not show up as a screen"))
        })
    }

    /// Switches `gdi` to `mode`, or to the closest published refresh rate of the same size.
    fn set_exact_mode(&self, gdi: &str, mode: Mode) -> Result<()> {
        if let Some((_, _, w, h, hz)) = current_geometry(gdi)
            && (w, h, hz) == (mode.width, mode.height, mode.hz)
        {
            return Ok(());
        }
        let wname: Vec<u16> = gdi.encode_utf16().chain(std::iter::once(0)).collect();
        let name = PCWSTR(wname.as_ptr());
        // Candidates offered by the driver with the requested size, best rate match first.
        let mut offered: Vec<u32> = Vec::new();
        let mut i = 0u32;
        loop {
            let mut dm = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            // SAFETY: NUL-terminated name, initialised DEVMODEW with dmSize.
            if !unsafe { EnumDisplaySettingsW(name, ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }
                .as_bool()
            {
                break;
            }
            i += 1;
            if dm.dmPelsWidth == mode.width && dm.dmPelsHeight == mode.height {
                offered.push(dm.dmDisplayFrequency);
            }
        }
        if offered.is_empty() {
            return Err(PlatformError::other(format!(
                "the driver does not offer {}x{} on {gdi} (is it in vdd_settings.xml?)",
                mode.width, mode.height
            )));
        }
        offered.sort_by_key(|hz| hz.abs_diff(mode.hz));
        offered.dedup();
        let mut last = None;
        for hz in offered {
            let dm = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                dmPelsWidth: mode.width,
                dmPelsHeight: mode.height,
                dmDisplayFrequency: hz,
                dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
                ..Default::default()
            };
            // SAFETY: valid NUL-terminated name and initialised DEVMODEW.
            let test = unsafe {
                ChangeDisplaySettingsExW(name, Some(&dm), HWND::default(), CDS_TEST, None)
            };
            if test != DISP_CHANGE_SUCCESSFUL {
                last = Some(test.0);
                continue;
            }
            // SAFETY: as above; the mode was just validated with CDS_TEST.
            let rc = unsafe {
                ChangeDisplaySettingsExW(name, Some(&dm), HWND::default(), CDS_UPDATEREGISTRY, None)
            };
            if rc == DISP_CHANGE_SUCCESSFUL {
                if hz != mode.hz {
                    tracing::warn!(
                        wanted = mode.hz,
                        got = hz,
                        "exact refresh rate not offered; using the closest"
                    );
                }
                return Ok(());
            }
            last = Some(rc.0);
        }
        Err(PlatformError::Os {
            code: i64::from(last.unwrap_or(-1)),
            msg: format!(
                "ChangeDisplaySettingsEx({gdi}, {}x{}@{})",
                mode.width, mode.height, mode.hz
            ),
        })
    }

    /// Everything of `enable_virtual` after the checks; the caller rolls back on error.
    fn enable_inner(&self, mode: Mode, hint: Option<&str>, node: &VddNode) -> Result<u32> {
        // (a) settings: requested mode + fallbacks, one monitor, render GPU by friendly name.
        let adapter = self.choose_adapter(hint);
        let base = self.base_settings();
        let existing = vdd::parse_modes(&base);
        let wanted = vdd::modes_for_enable(&existing, mode);
        let gpu = adapter.as_ref().map(|a| a.name.as_str());
        let xml = vdd::render_settings(&base, &wanted, gpu, 1)
            .map_err(|e| PlatformError::other(format!("vdd_settings.xml: {e}")))?;

        let settings_unchanged = vddfile::read_settings(&self.settings_dir).is_some_and(|cur| {
            vdd::parse_modes(&cur) == vdd::parse_modes(&xml)
                && vdd::parse_count(&cur) == Some(1)
                && gpu.is_none_or(|g| vdd::parse_gpu(&cur).as_deref() == Some(g))
        });

        // Fast path: the monitor already exists with a suitable configuration.
        let mut running = None;
        if node.started() && settings_unchanged {
            running = ccd::current_targets()?
                .into_iter()
                .find(|t| t.available && looks_like_vdd(t));
        }

        let target = if let Some(t) = running {
            tracing::info!("virtual display already running; only switching the mode");
            t
        } else {
            // (b) restart the device node with the new settings.
            if node.started() {
                tracing::info!("restarting the virtual display device to apply new settings");
                self.device_op(false)?;
                std::thread::sleep(Duration::from_millis(300));
            }
            vddfile::write_settings(&self.settings_dir, &xml)?;
            let before = device_paths(&ccd::current_targets()?);
            self.device_op(true)?;
            // (c) wait for the new display target.
            let found = self.wait_until(self.tuning.settle_timeout, || {
                let now = ccd::current_targets().ok()?;
                let mut fresh: Vec<CurrentTarget> = now
                    .into_iter()
                    .filter(|t| t.available && !before.contains(&lower(&t.identity.device_path)))
                    .collect();
                fresh.sort_by_key(|t| !is_indirect_technology(t.output_technology));
                fresh.into_iter().next()
            });
            found.ok_or_else(|| {
                PlatformError::other(format!(
                    "the virtual display did not appear within {} ms",
                    self.tuning.settle_timeout.as_millis()
                ))
            })?
        };
        let key = (target.adapter, target.id);

        // The new monitor must be part of the active configuration.
        let path_of = |topo: &SavedTopology| topo.paths.iter().position(|p| p.target_key() == key);
        let mut active = ccd::query_active_saved()?;
        if path_of(&active).is_none() {
            tracing::info!("virtual display is not active yet; adding it to the desktop");
            let wait_active = || {
                self.wait_until(self.tuning.verify_timeout, || {
                    let t = ccd::query_active_saved().ok()?;
                    path_of(&t).map(|_| t)
                })
            };
            let mut got = match ccd::activate_target(target.adapter, target.id) {
                Ok(()) => wait_active(),
                Err(e) => {
                    tracing::warn!(error = %e, "activating the virtual monitor path failed");
                    None
                }
            };
            if got.is_none() {
                tracing::info!("falling back to extending the desktop onto every monitor");
                ccd::extend_all()?;
                got = wait_active();
            }
            active = got.ok_or_else(|| {
                PlatformError::other("the virtual display could not be activated")
            })?;
        }
        let idx = path_of(&active)
            .ok_or_else(|| PlatformError::other("virtual display path vanished"))?;
        let p = &active.paths[idx];
        let gdi = ccd::source_gdi_name(ccd::luid_from(p.source.adapter), p.source.id).ok_or_else(
            || PlatformError::other("cannot resolve the GDI name of the virtual display"),
        )?;

        // (d) exact mode, then the screen id.
        self.set_exact_mode(&gdi, mode)?;
        let id = self.screen_id_for_gdi(&gdi, &target.identity.friendly_name)?;
        tracing::info!(screen = id, %gdi, ?mode, "virtual display enabled");
        Ok(id)
    }

    /// Restores `before` (best effort, never fails the caller's error path) and stops the device.
    fn rollback_enable(&self, before: Option<&SavedTopology>) {
        tracing::warn!("rolling back the virtual display activation");
        if let Err(e) = ccd::restore(before) {
            tracing::error!(error = %e, "rollback: topology restore failed");
        }
        if self.node().ok().flatten().is_some_and(|n| n.started())
            && let Err(e) = self.device_op(false)
        {
            tracing::error!(error = %e, "rollback: disabling the device failed");
        }
        if !ccd::has_active_display() {
            let _ = ccd::extend_all();
        }
    }

    // -- diagnostics -------------------------------------------------------------------------

    /// Read-only snapshot of everything this manager looks at.
    #[must_use]
    pub fn report(&self) -> SystemReport {
        let adapters = dxgi::enumerate_adapters().unwrap_or_default();
        let chosen_adapter = choose_render_adapter(&adapters, None).map(|a| a.name.clone());
        let targets = ccd::current_targets();
        let active = ccd::query_active_saved();
        let validate = match &active {
            Ok(t) => ccd::validate_saved(t)
                .map_err(|rc| format!("SetDisplayConfig(VALIDATE) returned {rc}")),
            Err(e) => Err(e.to_string()),
        };
        let json_roundtrip = match &active {
            Ok(t) => t
                .to_json()
                .map_err(|e| e.to_string())
                .and_then(|j| SavedTopology::from_json(&j).map_err(|e| e.to_string()))
                .map(|back| &back == t),
            Err(e) => Err(e.to_string()),
        };
        let match_check = match (&active, &targets) {
            (Ok(t), Ok(cur)) => {
                Some(deskbridge_display::topology::match_targets(t, cur).is_identity())
            }
            _ => None,
        };
        let settings = vddfile::read_settings(&self.settings_dir);
        SystemReport {
            elevated: device::is_elevated(),
            adapters,
            chosen_adapter,
            targets: targets.map_err(|e| e.to_string()),
            active,
            driver_node: self.node().map_err(|e| e.to_string()),
            driver_store: device::driver_store_has_package(),
            virtual_available: self.virtual_available(),
            settings_dir: self.settings_dir.clone(),
            settings_modes: settings.as_deref().map(vdd::parse_modes),
            settings_gpu: settings.as_deref().and_then(vdd::parse_gpu),
            validate_active: validate,
            json_roundtrip,
            self_match_identity: match_check,
        }
    }
}

/// Current desktop rectangle and refresh rate of a GDI display: `(x, y, w, h, hz)`.
fn current_geometry(gdi: &str) -> Option<(i32, i32, u32, u32, u32)> {
    let wname: Vec<u16> = gdi.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dm = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: NUL-terminated name and initialised DEVMODEW.
    if !unsafe { EnumDisplaySettingsW(PCWSTR(wname.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }
        .as_bool()
        || dm.dmPelsWidth == 0
    {
        return None;
    }
    // SAFETY: the display variant of the DEVMODE union is the active one for displays.
    let pos = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
    Some((
        pos.x,
        pos.y,
        dm.dmPelsWidth,
        dm.dmPelsHeight,
        dm.dmDisplayFrequency,
    ))
}

impl DisplayManager for WinDisplayManager {
    fn virtual_available(&self) -> bool {
        match self.node() {
            Ok(Some(n)) => {
                let ok = n.usable();
                if !ok {
                    tracing::warn!(node = %n.describe(), "virtual display device is not usable");
                }
                if device::driver_store_has_package() == Some(false) {
                    tracing::warn!(
                        "device node exists but no mttvdd package was found in the driver store"
                    );
                }
                ok
            }
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(error = %e, "cannot enumerate display devices");
                false
            }
        }
    }

    fn enable_virtual(&self, mode: Mode, render_adapter_hint: Option<&str>) -> Result<u32> {
        let _g = self.op.lock();
        Self::require_elevated()?;
        let node = self.node()?.ok_or_else(|| {
            PlatformError::DeviceNotFound(
                "virtual display driver (Root\\MttVDD) is not installed".into(),
            )
        })?;
        if !node.usable() {
            return Err(PlatformError::other(format!(
                "virtual display device is in an error state: {}",
                node.describe()
            )));
        }
        // Rollback target: the configuration as it is right now.
        let before = ccd::query_active_saved().ok();
        match self.enable_inner(mode, render_adapter_hint, &node) {
            Ok(id) => Ok(id),
            Err(e) => {
                tracing::error!(error = %e, "enabling the virtual display failed");
                self.rollback_enable(before.as_ref());
                Err(e)
            }
        }
    }

    fn disable_virtual(&self) -> Result<()> {
        let _g = self.op.lock();
        let Some(node) = self.node()? else {
            return Ok(());
        };
        if !node.started() {
            return Ok(());
        }
        Self::require_elevated()?;
        // Never remove the last real display: if only virtual displays are active, bring the
        // real ones back first.
        let active = ccd::query_active_saved()?;
        if !active.paths.is_empty() && active.paths.iter().all(|p| p.is_indirect()) {
            tracing::warn!(
                "only virtual displays are active; activating the real ones before disabling"
            );
            let _ = ccd::extend_all();
            let after = ccd::query_active_saved()?;
            let real_active = after.paths.iter().any(|p| !p.is_indirect());
            let real_available = ccd::current_targets()?
                .iter()
                .any(|t| t.available && !is_indirect_technology(t.output_technology));
            if !real_active && real_available {
                return Err(PlatformError::other(
                    "refusing to disable the virtual display: no real display could be activated",
                ));
            }
        }
        self.device_op(false)?;
        let gone = self.wait_until(self.tuning.settle_timeout, || {
            self.node()
                .ok()
                .flatten()
                .is_none_or(|n| !n.started())
                .then_some(())
        });
        if gone.is_none() {
            tracing::warn!("device still reports started after disabling");
        }
        if !ccd::has_active_display() {
            tracing::error!("no active display after disabling the virtual display; extending");
            ccd::extend_all()?;
        }
        tracing::info!("virtual display disabled");
        Ok(())
    }

    fn save_topology(&self) -> Result<Topology> {
        let _g = self.op.lock();
        let t = ccd::query_active_saved()?;
        let data = t
            .to_json()
            .map_err(|e| PlatformError::other(e.to_string()))?;
        tracing::info!(
            paths = t.paths.len(),
            modes = t.modes.len(),
            aware = t.virtual_aware,
            "topology saved"
        );
        Ok(Topology { data })
    }

    fn restore_topology(&self, topology: &Topology) -> Result<()> {
        let _g = self.op.lock();
        let saved = if topology.data.trim().is_empty() {
            None
        } else {
            match SavedTopology::from_json(&topology.data) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::error!(error = %e, "saved topology is unreadable; using system fallbacks");
                    None
                }
            }
        };
        let method = ccd::restore(saved.as_ref())?;
        tracing::info!(method, "topology restored");
        Ok(())
    }

    fn use_as_only_display(&self, screen_id: u32, disable_internal: bool) -> Result<()> {
        let _g = self.op.lock();
        let before = ccd::query_active_saved()?;
        let idx = self.locate_screen(screen_id, &before)?;
        let key = before.paths[idx].target_key();

        let mut want = before.clone();
        if !want.make_primary(idx) {
            return Err(PlatformError::other(
                "the selected screen has no source mode",
            ));
        }
        if disable_internal && !before.paths[idx].is_internal() {
            let dropped =
                want.retain_paths(|_, p| !is_internal_technology(p.target.output_technology));
            if dropped.paths.iter().all(|p| p.target_key() != key) {
                return Err(PlatformError::other(
                    "guard: the virtual display would vanish",
                ));
            }
            want = dropped;
        }
        // Guard rails before touching anything.
        let new_idx = want
            .paths
            .iter()
            .position(|p| p.target_key() == key)
            .ok_or_else(|| PlatformError::other("guard: target path missing"))?;
        if want.paths.is_empty() || want.primary_path() != Some(new_idx) {
            return Err(PlatformError::other(
                "guard: resulting configuration has no primary display",
            ));
        }
        want.validate()
            .map_err(|e| PlatformError::other(e.to_string()))?;
        ccd::validate_saved(&want).map_err(|rc| {
            PlatformError::other(format!(
                "Windows rejects the focus configuration (rc {rc}); nothing changed"
            ))
        })?;

        let rollback = |why: &str| {
            tracing::error!(
                why,
                "focus display switch failed; restoring the previous configuration"
            );
            if let Err(e) = ccd::restore(Some(&before)) {
                tracing::error!(error = %e, "rollback of the display switch failed");
            }
        };
        if let Err(rc) = ccd::apply_saved(&want, ccd::focus_apply_flags(), true) {
            rollback("SetDisplayConfig failed");
            return Err(PlatformError::other(format!(
                "SetDisplayConfig failed (rc {rc}); previous configuration restored"
            )));
        }
        // The virtual target must be active, primary, and something must be on screen.
        let ok = self.wait_until(self.tuning.verify_timeout, || {
            let now = ccd::query_active_saved().ok()?;
            let i = now.paths.iter().position(|p| p.target_key() == key)?;
            (now.primary_path() == Some(i)).then_some(())
        });
        if ok.is_none() {
            rollback("virtual display not primary after the switch");
            return Err(PlatformError::other(
                "the virtual display did not become the primary display; previous configuration restored",
            ));
        }
        tracing::info!(
            screen = screen_id,
            disable_internal,
            "virtual display is now the primary display"
        );
        Ok(())
    }

    fn configure_modes(&self, modes: &[Mode]) -> Result<()> {
        let _g = self.op.lock();
        let clean = vdd::clean_modes(modes);
        if clean.is_empty() {
            return Err(PlatformError::other("no valid mode to configure"));
        }
        let base = self.base_settings();
        let xml = vdd::render_settings(&base, &clean, None, 1)
            .map_err(|e| PlatformError::other(format!("vdd_settings.xml: {e}")))?;
        vddfile::write_settings(&self.settings_dir, &xml)?;
        tracing::info!(count = clean.len(), "driver modes written");
        let node = self.node().ok().flatten();
        self.reload_driver_if_running(node.as_ref());
        Ok(())
    }
}

/// `SETGPU` through the driver pipe (when the driver runs), kept for callers that want to switch
/// the render GPU without restarting the device.
pub fn set_render_gpu_via_pipe(name: &str) -> Result<()> {
    vddfile::send_pipe_command(&cmd_setgpu(name))
}

/// Everything `display_probe list` prints. Read-only.
#[derive(Debug)]
pub struct SystemReport {
    pub elevated: bool,
    pub adapters: Vec<AdapterInfo>,
    pub chosen_adapter: Option<String>,
    pub targets: std::result::Result<Vec<CurrentTarget>, String>,
    pub active: Result<SavedTopology>,
    pub driver_node: std::result::Result<Option<VddNode>, String>,
    pub driver_store: Option<bool>,
    pub virtual_available: bool,
    pub settings_dir: PathBuf,
    pub settings_modes: Option<Vec<Mode>>,
    pub settings_gpu: Option<String>,
    /// `SDC_VALIDATE` of the active configuration (no change is made).
    pub validate_active: std::result::Result<(), String>,
    pub json_roundtrip: std::result::Result<bool, String>,
    pub self_match_identity: Option<bool>,
}

impl SystemReport {
    /// Human readable multi-line summary.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "elevated: {}", self.elevated);
        let _ = writeln!(s, "DXGI adapters:");
        for a in &self.adapters {
            let _ = writeln!(
                s,
                "  - {} (vendor {:#06x}, {} MiB VRAM{}{})",
                a.name,
                a.vendor_id,
                a.dedicated_vram >> 20,
                if a.software { ", software" } else { "" },
                if a.has_hw_encoder() {
                    ", HW encoder"
                } else {
                    ""
                },
            );
        }
        let _ = writeln!(
            s,
            "render adapter that would be chosen: {}",
            self.chosen_adapter.as_deref().unwrap_or("none")
        );
        match &self.active {
            Ok(t) => {
                let _ = writeln!(
                    s,
                    "active paths: {} (virtual-mode-aware: {})",
                    t.paths.len(),
                    t.virtual_aware
                );
                for (i, p) in t.paths.iter().enumerate() {
                    let pos = t
                        .source_position(i)
                        .map_or_else(String::new, |(x, y)| format!(" at ({x},{y})"));
                    let size = t
                        .source_size(i)
                        .map_or_else(String::new, |(w, h)| format!(" {w}x{h}"));
                    let kind = if p.is_internal() {
                        "internal"
                    } else if p.is_indirect() {
                        "indirect"
                    } else {
                        "external"
                    };
                    let _ = writeln!(
                        s,
                        "  - {}{}{} [{}, tech {}]{}",
                        p.target.identity.friendly_name,
                        size,
                        pos,
                        kind,
                        p.target.output_technology,
                        if t.primary_path() == Some(i) {
                            " PRIMARY"
                        } else {
                            ""
                        },
                    );
                }
            }
            Err(e) => {
                let _ = writeln!(s, "active paths: error: {e}");
            }
        }
        match &self.targets {
            Ok(v) => {
                let avail = v.iter().filter(|t| t.available).count();
                let _ = writeln!(s, "display targets: {} known, {} available", v.len(), avail);
            }
            Err(e) => {
                let _ = writeln!(s, "display targets: error: {e}");
            }
        }
        match &self.driver_node {
            Ok(Some(n)) => {
                let _ = writeln!(s, "virtual display device: {}", n.describe());
            }
            Ok(None) => {
                let _ = writeln!(
                    s,
                    "virtual display device: not installed (Root\\MttVDD not found)"
                );
            }
            Err(e) => {
                let _ = writeln!(s, "virtual display device: error: {e}");
            }
        }
        let _ = writeln!(
            s,
            "driver store package (mttvdd.inf_*): {}",
            match self.driver_store {
                Some(true) => "present",
                Some(false) => "absent",
                None => "unknown",
            }
        );
        let _ = writeln!(s, "virtual_available(): {}", self.virtual_available);
        let _ = writeln!(
            s,
            "settings: {} ({}{})",
            self.settings_dir.display(),
            match &self.settings_modes {
                Some(m) => format!("{} modes", m.len()),
                None => "file absent".to_owned(),
            },
            self.settings_gpu
                .as_ref()
                .map_or_else(String::new, |g| format!(", gpu \"{g}\"")),
        );
        let _ = writeln!(
            s,
            "self-check: topology JSON round trip: {:?}",
            self.json_roundtrip
        );
        let _ = writeln!(
            s,
            "self-check: saved topology matches the live one: {:?}",
            self.self_match_identity
        );
        let _ = writeln!(
            s,
            "self-check: SetDisplayConfig(SDC_VALIDATE) of the active config: {:?}",
            self.validate_active
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(dir: &Path) -> WinDisplayManager {
        WinDisplayManager::with_settings_dir(dir).with_tuning(DisplayTuning {
            use_pipe: false,
            ..DisplayTuning::default()
        })
    }

    #[test]
    fn configure_modes_writes_the_driver_config() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(d.path());
        let modes = [
            Mode {
                width: 5120,
                height: 1440,
                hz: 120,
            },
            Mode {
                width: 2560,
                height: 1440,
                hz: 120,
            },
        ];
        m.configure_modes(&modes).unwrap();
        let xml = vddfile::read_settings(d.path()).unwrap();
        assert_eq!(vdd::parse_modes(&xml), modes);
        assert_eq!(vdd::parse_count(&xml), Some(1));
        // a second call replaces, not appends
        m.configure_modes(&modes[..1]).unwrap();
        assert_eq!(
            vdd::parse_modes(&vddfile::read_settings(d.path()).unwrap()),
            modes[..1]
        );
        assert!(m.configure_modes(&[]).is_err());
        assert!(
            m.configure_modes(&[Mode {
                width: 0,
                height: 0,
                hz: 0
            }])
            .is_err()
        );
    }

    #[test]
    fn garbage_settings_fall_back_to_the_template() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(vddfile::settings_file(d.path()), "garbage").unwrap();
        let m = manager(d.path());
        m.configure_modes(&[Mode {
            width: 1920,
            height: 1080,
            hz: 60,
        }])
        .unwrap();
        let xml = vddfile::read_settings(d.path()).unwrap();
        assert!(xml.contains("<HardwareCursor>"), "template options present");
    }

    #[test]
    fn restore_of_garbage_is_handled_before_touching_the_system() {
        // Only the parsing side is exercised: an unreadable payload must not panic.
        assert!(SavedTopology::from_json("{garbage").is_err());
    }

    #[test]
    fn vdd_target_heuristic() {
        use deskbridge_display::topology::{Luid, TargetIdentity};
        let mk = |tech, path: &str| CurrentTarget {
            adapter: Luid::default(),
            id: 1,
            output_technology: tech,
            available: true,
            identity: TargetIdentity {
                device_path: path.into(),
                ..TargetIdentity::default()
            },
        };
        assert!(looks_like_vdd(&mk(16, r"\\?\DISPLAY#MTT1337#5&abc#{guid}")));
        assert!(!looks_like_vdd(&mk(
            10,
            r"\\?\DISPLAY#MTT1337#5&abc#{guid}"
        )));
        assert!(!looks_like_vdd(&mk(
            16,
            r"\\?\DISPLAY#DEL4321#5&abc#{guid}"
        )));
    }

    #[test]
    fn read_only_report_on_this_machine() {
        let m = WinDisplayManager::new();
        let r = m.report();
        let text = r.render();
        eprintln!("{text}");
        assert!(text.contains("active paths"));
        // The live configuration converts to the model and survives a JSON round trip.
        if let Ok(t) = &r.active {
            assert!(t.validate().is_ok());
            assert_eq!(r.json_roundtrip, Ok(true));
        }
    }

    #[test]
    fn save_topology_is_read_only_and_valid_json() {
        let m = WinDisplayManager::new();
        if let Ok(t) = m.save_topology() {
            let parsed = SavedTopology::from_json(&t.data).unwrap();
            assert!(!parsed.paths.is_empty());
        }
    }
}
