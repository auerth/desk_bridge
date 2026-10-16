//! Setting the system default audio endpoints through the undocumented `IPolicyConfig`.
//!
//! This is the same COM interface that SoundSwitch, EarTrumpet, `nircmd setdefaultsounddevice`
//! and friends use. It is not part of the SDK and may change; callers must only use it behind an
//! explicit user opt-in ("set Desk Bridge as default device") and treat failure as non-fatal.
//!
//! GUIDs (Windows 7 - 11, the "Win7+" `IPolicyConfig` layout):
//! * interface `IPolicyConfig`: `f8679f50-850a-41cf-9c72-430f290290c8`
//! * coclass `CPolicyConfigClient`: `870af99c-171d-4f9e-af0d-e63df40c2bc9`
//!
//! Note: the task brief listed `870af99c-...` as the interface and `294935CE-...` as the CLSID.
//! In public sources `294935CE-F637-4E7C-A41B-AB255460B862` is the coclass of the older
//! *Vista* interface (`IPolicyConfigVista`, `568b9108-...`, which has no `ResetDeviceFormat` and so
//! a different vtable), and `870af99c-...` is the Win7+ coclass. We use the Win7+ pair.
//! [`policy_config_available`] checks that the object can be instantiated without changing
//! anything.

#![allow(non_snake_case)]

use super::wasapi::{ComGuard, enumerator, os_ctx, os_err};
use deskbridge_platform::{PlatformError, Result};
use std::ffi::c_void;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::{ERole, WAVEFORMATEX, eCommunications, eConsole, eMultimedia};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR};

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Vtable of the Win7+ `IPolicyConfig`. Only `SetDefaultEndpoint` is called; the preceding slots
/// are declared with the right arity so the vtable layout (slot index) matches.
#[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
    fn GetDeviceFormat(
        &self,
        device: PCWSTR,
        default: BOOL,
        format: *mut *mut WAVEFORMATEX,
    ) -> HRESULT;
    fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(
        &self,
        device: PCWSTR,
        endpoint_format: *mut WAVEFORMATEX,
        mix_format: *mut WAVEFORMATEX,
    ) -> HRESULT;
    fn GetProcessingPeriod(
        &self,
        device: PCWSTR,
        default: BOOL,
        default_period: *mut i64,
        min_period: *mut i64,
    ) -> HRESULT;
    fn SetProcessingPeriod(&self, device: PCWSTR, period: *mut i64) -> HRESULT;
    fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(
        &self,
        device: PCWSTR,
        fx_store: BOOL,
        key: *const c_void,
        value: *mut c_void,
    ) -> HRESULT;
    fn SetPropertyValue(
        &self,
        device: PCWSTR,
        fx_store: BOOL,
        key: *const c_void,
        value: *mut c_void,
    ) -> HRESULT;
    fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, device: PCWSTR, visible: BOOL) -> HRESULT;
}

/// Which default slot(s) to set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultRole {
    /// "Default device" for games, system sounds and most apps (`eConsole`).
    Console,
    /// Media playback (`eMultimedia`); Windows normally keeps this in sync with `Console`.
    Multimedia,
    /// "Default communication device" (`eCommunications`), used by Teams/Zoom/...
    Communications,
    /// All three roles.
    All,
}

impl DefaultRole {
    fn roles(self) -> &'static [ERole] {
        match self {
            Self::Console => &[eConsole],
            Self::Multimedia => &[eMultimedia],
            Self::Communications => &[eCommunications],
            Self::All => &[eConsole, eMultimedia, eCommunications],
        }
    }
}

fn policy_config() -> windows::core::Result<IPolicyConfig> {
    // SAFETY: COM is initialised by the caller's `ComGuard`.
    unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) }
}

/// True if the `IPolicyConfig` object can be created on this system (changes nothing).
pub fn policy_config_available() -> bool {
    let _com = ComGuard::init();
    policy_config().is_ok()
}

/// Makes `device_id` (an endpoint id from `AudioDevices::list`) the system default for `role`.
///
/// This changes a system setting; only call it on explicit user request. Not covered by
/// automated tests. Fails with `DeviceNotFound` for unknown ids.
pub fn set_default_device(device_id: &str, role: DefaultRole) -> Result<()> {
    let _com = ComGuard::init();
    let h = HSTRING::from(device_id);
    // Validate the id first so a typo yields DeviceNotFound instead of an opaque HRESULT.
    let en = enumerator().map_err(os_err)?;
    // SAFETY: `h` outlives the call.
    unsafe { en.GetDevice(PCWSTR(h.as_ptr())) }
        .map_err(|_| PlatformError::DeviceNotFound(device_id.to_string()))?;

    let policy = policy_config().map_err(|e| os_ctx("create IPolicyConfig", e))?;
    for &r in role.roles() {
        // SAFETY: `h` is a valid NUL-terminated id; `policy` is a live COM object.
        unsafe { policy.SetDefaultEndpoint(PCWSTR(h.as_ptr()), r) }
            .ok()
            .map_err(|e| os_ctx("SetDefaultEndpoint", e))?;
    }
    Ok(())
}

/// Enables or disables an endpoint, like "Disable" / "Enable" in the Sound control panel. A
/// disabled endpoint disappears from every application's device list.
///
/// This changes a system setting; only call it behind an explicit user opt-in.
pub fn set_endpoint_enabled(device_id: &str, enabled: bool) -> Result<()> {
    let _com = ComGuard::init();
    let h = HSTRING::from(device_id);
    let policy = policy_config().map_err(|e| os_ctx("create IPolicyConfig", e))?;
    // SAFETY: `h` is a valid NUL-terminated id; `policy` is a live COM object.
    unsafe { policy.SetEndpointVisibility(PCWSTR(h.as_ptr()), BOOL::from(enabled)) }
        .ok()
        .map_err(|e| os_ctx("SetEndpointVisibility", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_expansion() {
        assert_eq!(DefaultRole::Console.roles().len(), 1);
        assert_eq!(DefaultRole::All.roles().len(), 3);
        assert_eq!(DefaultRole::Communications.roles(), &[eCommunications]);
    }

    #[test]
    fn unknown_id_is_not_found() {
        // Does not touch any system setting: validation fails before IPolicyConfig is used.
        match set_default_device(
            "{0.0.0.00000000}.{00000000-0000-0000-0000-000000000000}",
            DefaultRole::All,
        ) {
            Err(PlatformError::DeviceNotFound(_)) | Err(PlatformError::Os { .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
