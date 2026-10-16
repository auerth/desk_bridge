use crate::Result;

/// Privileged system services that need the SYSTEM service on Windows (plan 6.1 / M6).
pub trait SystemServices: Send + Sync {
    /// Is the helper service reachable (secure desktop input, Ctrl+Alt+Del)?
    fn available(&self) -> bool;
    /// Generates the secure attention sequence (Ctrl+Alt+Del) on this machine.
    fn send_sas(&self) -> Result<()>;
}

/// No service available (tests, unelevated dev runs).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSystemServices;

impl SystemServices for NullSystemServices {
    fn available(&self) -> bool {
        false
    }

    fn send_sas(&self) -> Result<()> {
        Err(crate::PlatformError::Unsupported("the Desk Bridge service is not running"))
    }
}
