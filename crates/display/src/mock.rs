//! In-memory [`DisplayManager`] for tests of the session logic (and of `core`): records every call,
//! simulates a tiny display world, and can inject failures/delays at each step.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use deskbridge_platform::display::{DisplayManager, Mode, Topology};
use deskbridge_platform::{PlatformError, Result};
use serde::{Deserialize, Serialize};

/// The calls of the [`DisplayManager`] trait (failure injection key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    VirtualAvailable,
    EnableVirtual,
    DisableVirtual,
    SaveTopology,
    RestoreTopology,
    UseAsOnlyDisplay,
    ConfigureModes,
}

/// A recorded call with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    VirtualAvailable,
    EnableVirtual {
        mode: Mode,
        hint: Option<String>,
    },
    DisableVirtual,
    SaveTopology,
    RestoreTopology(Topology),
    UseAsOnlyDisplay {
        screen_id: u32,
        disable_internal: bool,
    },
    ConfigureModes(Vec<Mode>),
}

impl Call {
    #[must_use]
    pub fn step(&self) -> Step {
        match self {
            Call::VirtualAvailable => Step::VirtualAvailable,
            Call::EnableVirtual { .. } => Step::EnableVirtual,
            Call::DisableVirtual => Step::DisableVirtual,
            Call::SaveTopology => Step::SaveTopology,
            Call::RestoreTopology(_) => Step::RestoreTopology,
            Call::UseAsOnlyDisplay { .. } => Step::UseAsOnlyDisplay,
            Call::ConfigureModes(_) => Step::ConfigureModes,
        }
    }
}

/// What the simulated machine looks like. This is also the payload of the mock's topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct World {
    /// The internal panel is part of the active topology.
    pub internal_active: bool,
    /// The virtual device node is enabled (a virtual monitor exists).
    pub virtual_enabled: bool,
    /// The virtual monitor is part of the active topology (and primary).
    pub virtual_in_use: bool,
}

impl Default for World {
    fn default() -> Self {
        Self {
            internal_active: true,
            virtual_enabled: false,
            virtual_in_use: false,
        }
    }
}

impl World {
    #[must_use]
    pub fn blackout(&self) -> bool {
        !self.internal_active && !self.virtual_in_use
    }
}

/// How an injected failure behaves.
#[derive(Debug, Clone, Copy)]
pub struct Failure {
    /// Fail this many calls (`u32::MAX` = always).
    pub times: u32,
    /// Apply the call's side effect before failing (a half-done operation).
    pub after_effect: bool,
}

impl Failure {
    #[must_use]
    pub const fn once() -> Self {
        Self {
            times: 1,
            after_effect: false,
        }
    }

    #[must_use]
    pub const fn always() -> Self {
        Self {
            times: u32::MAX,
            after_effect: false,
        }
    }

    #[must_use]
    pub const fn times(n: u32) -> Self {
        Self {
            times: n,
            after_effect: false,
        }
    }

    #[must_use]
    pub const fn with_effect(mut self) -> Self {
        self.after_effect = true;
        self
    }
}

struct Inner {
    calls: Vec<Call>,
    world: World,
    available: bool,
    failures: HashMap<Step, Failure>,
    delays: HashMap<Step, Duration>,
    /// Set once the simulated machine had no active display at any moment.
    ever_blackout: bool,
    virtual_id: u32,
    last_modes: Vec<Mode>,
}

pub struct MockDisplayManager {
    inner: Mutex<Inner>,
    /// Calls are serialised like the real implementation does, so that a call abandoned by a
    /// timeout finishes before the rollback's next call starts.
    serial: Mutex<()>,
}

impl Default for MockDisplayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDisplayManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                calls: Vec::new(),
                world: World::default(),
                available: true,
                failures: HashMap::new(),
                delays: HashMap::new(),
                ever_blackout: false,
                virtual_id: 7,
                last_modes: Vec::new(),
            }),
            serial: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn serial(&self) -> MutexGuard<'_, ()> {
        self.serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Number of recorded calls of `step`.
    #[must_use]
    pub fn count(&self, step: Step) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|c| c.step() == step)
            .count()
    }

    pub fn clear_calls(&self) {
        self.lock().calls.clear();
    }

    #[must_use]
    pub fn world(&self) -> World {
        self.lock().world.clone()
    }

    pub fn set_world(&self, w: World) {
        self.lock().world = w;
    }

    /// True if at any point no display was active (the one thing that must never happen).
    #[must_use]
    pub fn ever_blackout(&self) -> bool {
        self.lock().ever_blackout
    }

    #[must_use]
    pub fn last_modes(&self) -> Vec<Mode> {
        self.lock().last_modes.clone()
    }

    pub fn set_available(&self, v: bool) {
        self.lock().available = v;
    }

    /// Screen id returned by `enable_virtual`.
    #[must_use]
    pub fn virtual_screen_id(&self) -> u32 {
        self.lock().virtual_id
    }

    pub fn fail(&self, step: Step, f: Failure) {
        self.lock().failures.insert(step, f);
    }

    pub fn clear_failures(&self) {
        self.lock().failures.clear();
    }

    /// Makes every call of `step` block for `d` before it does anything.
    pub fn delay(&self, step: Step, d: Duration) {
        self.lock().delays.insert(step, d);
    }

    /// Records the call, applies delay and failure injection. Returns `(failed, apply_effect)`.
    fn enter(&self, call: Call) -> (bool, bool) {
        let step = call.step();
        let delay = {
            let mut g = self.lock();
            g.calls.push(call);
            g.delays.get(&step).copied()
        };
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
        let mut g = self.lock();
        match g.failures.get_mut(&step) {
            Some(f) if f.times > 0 => {
                if f.times != u32::MAX {
                    f.times -= 1;
                }
                (true, f.after_effect)
            }
            _ => (false, true),
        }
    }

    fn check_blackout(g: &mut Inner) {
        if g.world.blackout() {
            g.ever_blackout = true;
        }
    }

    fn injected(step: Step) -> PlatformError {
        PlatformError::other(format!("injected failure at {step:?}"))
    }
}

impl DisplayManager for MockDisplayManager {
    fn virtual_available(&self) -> bool {
        let _serial = self.serial();
        let (failed, _) = self.enter(Call::VirtualAvailable);
        !failed && self.lock().available
    }

    fn enable_virtual(&self, mode: Mode, hint: Option<&str>) -> Result<u32> {
        let _serial = self.serial();
        let (failed, effect) = self.enter(Call::EnableVirtual {
            mode,
            hint: hint.map(str::to_owned),
        });
        let mut g = self.lock();
        if effect {
            g.world.virtual_enabled = true;
        }
        if failed {
            return Err(Self::injected(Step::EnableVirtual));
        }
        Ok(g.virtual_id)
    }

    fn disable_virtual(&self) -> Result<()> {
        let _serial = self.serial();
        let (failed, effect) = self.enter(Call::DisableVirtual);
        let mut g = self.lock();
        if effect {
            // Like the real manager: when only the virtual display is active, the real ones
            // are switched on before the device goes away (never a blackout).
            if g.world.virtual_in_use && !g.world.internal_active {
                g.world.internal_active = true;
            }
            g.world.virtual_enabled = false;
            g.world.virtual_in_use = false;
            Self::check_blackout(&mut g);
        }
        if failed {
            return Err(Self::injected(Step::DisableVirtual));
        }
        Ok(())
    }

    fn save_topology(&self) -> Result<Topology> {
        let _serial = self.serial();
        let (failed, _) = self.enter(Call::SaveTopology);
        if failed {
            return Err(Self::injected(Step::SaveTopology));
        }
        let data = serde_json::to_string(&self.lock().world)
            .map_err(|e| PlatformError::other(e.to_string()))?;
        Ok(Topology { data })
    }

    fn restore_topology(&self, topology: &Topology) -> Result<()> {
        let _serial = self.serial();
        let (failed, effect) = self.enter(Call::RestoreTopology(topology.clone()));
        if failed && !effect {
            return Err(Self::injected(Step::RestoreTopology));
        }
        // Empty payload = "no saved topology": the platform falls back to extending all real displays.
        let saved: World = if topology.data.is_empty() {
            World::default()
        } else {
            serde_json::from_str(&topology.data)
                .map_err(|e| PlatformError::other(format!("mock topology: {e}")))?
        };
        let mut g = self.lock();
        // The virtual device node keeps its state; only the active paths come back.
        g.world.internal_active = saved.internal_active;
        g.world.virtual_in_use = saved.virtual_in_use && g.world.virtual_enabled;
        Self::check_blackout(&mut g);
        if failed {
            return Err(Self::injected(Step::RestoreTopology));
        }
        Ok(())
    }

    fn use_as_only_display(&self, screen_id: u32, disable_internal: bool) -> Result<()> {
        let _serial = self.serial();
        let (failed, effect) = self.enter(Call::UseAsOnlyDisplay {
            screen_id,
            disable_internal,
        });
        let mut g = self.lock();
        if screen_id != g.virtual_id || !g.world.virtual_enabled {
            return Err(PlatformError::DeviceNotFound(format!("screen {screen_id}")));
        }
        if effect {
            g.world.virtual_in_use = true;
            if disable_internal {
                g.world.internal_active = false;
            }
            Self::check_blackout(&mut g);
        }
        if failed {
            return Err(Self::injected(Step::UseAsOnlyDisplay));
        }
        Ok(())
    }

    fn configure_modes(&self, modes: &[Mode]) -> Result<()> {
        let _serial = self.serial();
        let (failed, _) = self.enter(Call::ConfigureModes(modes.to_vec()));
        if failed {
            return Err(Self::injected(Step::ConfigureModes));
        }
        self.lock().last_modes = modes.to_vec();
        Ok(())
    }
}
