use crate::Result;

/// Privileged system services that need the SYSTEM service on Windows (plan 6.1 / M6).
pub trait SystemServices: Send + Sync {
    /// Is the helper service reachable (secure desktop input, Ctrl+Alt+Del)?
    fn available(&self) -> bool;
    /// Generates the secure attention sequence (Ctrl+Alt+Del) on this machine.
    fn send_sas(&self) -> Result<()>;
    /// Names of the connected networks Windows classifies as **Public** (the firewall rule never
    /// opens that profile, so peers cannot reach this device). `None` = unknown.
    fn public_networks(&self) -> Option<Vec<String>> {
        None
    }
    /// Switches the connected Public networks to Private (a system setting: only after the user
    /// confirmed it in the UI). Needs elevation or the service.
    fn make_network_private(&self) -> Result<()> {
        Err(crate::PlatformError::Unsupported(
            "changing the network profile is not supported here",
        ))
    }
    /// Starts a Desk Bridge installer unattended (a partner pushed it and asked for it to be
    /// installed). Returns once it was started; the installer then stops this process.
    fn launch_installer(&self, path: &std::path::Path) -> Result<()> {
        let _ = path;
        Err(crate::PlatformError::Unsupported(
            "installing updates is not supported here",
        ))
    }
}

/// No service available (tests, unelevated dev runs).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSystemServices;

impl SystemServices for NullSystemServices {
    fn available(&self) -> bool {
        false
    }

    fn send_sas(&self) -> Result<()> {
        Err(crate::PlatformError::Unsupported(
            "the Desk Bridge service is not running",
        ))
    }
}
