use crate::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

/// Opaque saved display topology (serialised to `topology.json` for crash recovery).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Topology {
    /// Platform specific payload (Windows: JSON of paths/modes).
    pub data: String,
}

pub trait DisplayManager: Send + Sync {
    /// Is the virtual display driver installed and usable?
    fn virtual_available(&self) -> bool;
    /// Enables the virtual monitor with the given mode. Returns the new screen id.
    fn enable_virtual(&self, mode: Mode, render_adapter_hint: Option<&str>) -> Result<u32>;
    fn disable_virtual(&self) -> Result<()>;
    fn save_topology(&self) -> Result<Topology>;
    fn restore_topology(&self, topology: &Topology) -> Result<()>;
    /// Makes `screen_id` primary and optionally switches the internal panel off.
    fn use_as_only_display(&self, screen_id: u32, disable_internal: bool) -> Result<()>;
    /// Modes the virtual driver should expose (written to the driver configuration).
    fn configure_modes(&self, modes: &[Mode]) -> Result<()>;
    /// Sets the Windows display scale of `screen_id` to the step closest to `percent`.
    /// Returns the scale now in effect.
    fn set_scale(&self, _screen_id: u32, percent: u32) -> Result<u32> {
        Ok(percent)
    }
}
