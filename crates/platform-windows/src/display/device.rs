//! SetupAPI access to the virtual display device node (`Root\MttVDD`): find it, read its state,
//! enable / disable it. Also the elevation check and a driver-store probe.

use std::mem::size_of;

use deskbridge_display::vdd::HARDWARE_IDS;
use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, DICS_DISABLE, DICS_ENABLE,
    DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, GUID_DEVCLASS_DISPLAY, HDEVINFO,
    SETUP_DI_GET_CLASS_DEVS_FLAGS, SETUP_DI_STATE_CHANGE, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA,
    SP_PROPCHANGE_PARAMS, SPDRP_DRIVER, SPDRP_HARDWAREID, SetupDiCallClassInstaller,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

const DN_STARTED: u32 = 0x0000_0008;
const CM_PROB_DISABLED: u32 = 22;
const CM_PROB_FAILED_INSTALL: u32 = 28;

/// State of the virtual display device node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VddNode {
    pub instance_id: String,
    pub hardware_id: String,
    /// `CM_Get_DevNode_Status` status bits.
    pub status: u32,
    /// `CM_PROB_*` (0 = none).
    pub problem: u32,
    /// A driver is bound (the `Driver` registry value exists).
    pub has_driver: bool,
}

impl VddNode {
    #[must_use]
    pub fn started(&self) -> bool {
        self.status & DN_STARTED != 0 && self.problem == 0
    }

    #[must_use]
    pub fn disabled(&self) -> bool {
        self.problem == CM_PROB_DISABLED
    }

    /// No problem other than "disabled" and a driver is bound.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.has_driver && (self.problem == 0 || self.problem == CM_PROB_DISABLED)
    }

    #[must_use]
    pub fn describe(&self) -> String {
        let state = if self.started() {
            "started".to_owned()
        } else if self.disabled() {
            "disabled".to_owned()
        } else if self.problem == CM_PROB_FAILED_INSTALL {
            "no driver installed (problem 28)".to_owned()
        } else if self.problem != 0 {
            format!("problem code {}", self.problem)
        } else {
            "stopped".to_owned()
        };
        format!(
            "{} [{}], driver bound: {}",
            self.instance_id, state, self.has_driver
        )
    }
}

/// Owns an `HDEVINFO` and destroys it on drop.
struct DevInfoList(HDEVINFO);

impl Drop for DevInfoList {
    fn drop(&mut self) {
        // SAFETY: the handle came from SetupDiGetClassDevsW and is destroyed exactly once.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn utf16_multi_sz(bytes: &[u8]) -> Vec<String> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    units
        .split(|u| *u == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn is_vdd_hwid(ids: &[String]) -> Option<String> {
    ids.iter()
        .find(|id| HARDWARE_IDS.iter().any(|h| h.eq_ignore_ascii_case(id)))
        .cloned()
}

/// Enumerates the display class and calls `f` for the Desk Bridge virtual display node, if any.
fn with_node<R>(f: impl FnOnce(HDEVINFO, &SP_DEVINFO_DATA, VddNode) -> R) -> Result<Option<R>> {
    // SAFETY: standard SetupAPI enumeration; all buffers are sized and initialised, the device
    // information set is owned by `DevInfoList` and outlives every use.
    unsafe {
        let set = SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
            PCWSTR::null(),
            HWND::default(),
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
        .map_err(|e| PlatformError::Os {
            code: i64::from(e.code().0),
            msg: format!("SetupDiGetClassDevsW: {e}"),
        })?;
        let list = DevInfoList(set);
        let mut idx = 0u32;
        loop {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInfo(list.0, idx, &mut data).is_err() {
                return Ok(None); // end of list
            }
            idx += 1;
            let mut buf = [0u8; 2048];
            let mut req = 0u32;
            if SetupDiGetDeviceRegistryPropertyW(
                list.0,
                &data,
                SPDRP_HARDWAREID,
                None,
                Some(&mut buf),
                Some(&mut req),
            )
            .is_err()
            {
                continue;
            }
            let ids = utf16_multi_sz(&buf[..(req as usize).min(buf.len())]);
            let Some(hwid) = is_vdd_hwid(&ids) else {
                continue;
            };
            let mut st = CM_DEVNODE_STATUS_FLAGS(0);
            let mut pr = CM_PROB(0);
            if CM_Get_DevNode_Status(&mut st, &mut pr, data.DevInst, 0) != CR_SUCCESS {
                // Phantom (not present) devnode: treat as absent.
                continue;
            }
            let mut idbuf = [0u16; 512];
            let instance_id =
                if SetupDiGetDeviceInstanceIdW(list.0, &data, Some(&mut idbuf), None).is_ok() {
                    super::ccd::wide(&idbuf)
                } else {
                    String::new()
                };
            let mut dbuf = [0u8; 512];
            let has_driver = SetupDiGetDeviceRegistryPropertyW(
                list.0,
                &data,
                SPDRP_DRIVER,
                None,
                Some(&mut dbuf),
                None,
            )
            .is_ok();
            let node = VddNode {
                instance_id,
                hardware_id: hwid,
                status: st.0,
                problem: pr.0,
                has_driver,
            };
            return Ok(Some(f(list.0, &data, node)));
        }
    }
}

/// Current state of the virtual display device node (`None`: not installed).
pub fn find_node() -> Result<Option<VddNode>> {
    with_node(|_, _, node| node)
}

/// Enables or disables the device node (needs elevation). Returns whether Windows asks for a
/// restart.
pub fn set_enabled(enable: bool) -> Result<bool> {
    let change: SETUP_DI_STATE_CHANGE = if enable { DICS_ENABLE } else { DICS_DISABLE };
    let r = with_node(|set, data, _node| -> Result<bool> {
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: change,
            Scope: DICS_FLAG_GLOBAL,
            HwProfile: 0,
        };
        let werr = |what: &str, e: windows::core::Error| PlatformError::Os {
            code: i64::from(e.code().0),
            msg: format!("{what}: {e}"),
        };
        // SAFETY: `params` starts with its SP_CLASSINSTALL_HEADER and the size passed is that of
        // the whole struct, as SetupDiSetClassInstallParams requires.
        unsafe {
            SetupDiSetClassInstallParamsW(
                set,
                Some(data),
                Some(std::ptr::from_ref(&params.ClassInstallHeader)),
                size_of::<SP_PROPCHANGE_PARAMS>() as u32,
            )
            .map_err(|e| werr("SetupDiSetClassInstallParamsW", e))?;
            let res = SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, Some(data));
            // Always clear the class install parameters again.
            let _ = SetupDiSetClassInstallParamsW(set, Some(data), None, 0);
            res.map_err(|e| werr("SetupDiCallClassInstaller(DIF_PROPERTYCHANGE)", e))?;
        }
        Ok(false)
    })?;
    match r {
        Some(inner) => inner,
        None => Err(PlatformError::DeviceNotFound(
            "virtual display device node (Root\\MttVDD)".into(),
        )),
    }
}

/// Is the process elevated (administrator token)?
#[must_use]
pub fn is_elevated() -> bool {
    // SAFETY: token handle is opened and closed here; the out buffer matches TOKEN_ELEVATION.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(std::ptr::from_mut(&mut elev).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// Is a package of the virtual display driver staged in the driver store?
/// `None` when the store cannot be listed.
#[must_use]
pub fn driver_store_has_package() -> Option<bool> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let dir = std::path::Path::new(&root).join(r"System32\DriverStore\FileRepository");
    let rd = std::fs::read_dir(dir).ok()?;
    Some(rd.flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("mttvdd.inf_")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_parsing() {
        let mut b = Vec::new();
        for s in ["Root\\MttVDD", "Other"] {
            b.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
            b.extend([0, 0]);
        }
        b.extend([0, 0]);
        let ids = utf16_multi_sz(&b);
        assert_eq!(ids, ["Root\\MttVDD", "Other"]);
        assert_eq!(is_vdd_hwid(&ids).as_deref(), Some("Root\\MttVDD"));
        assert_eq!(
            is_vdd_hwid(&["root\\mttvdd".to_owned()]).as_deref(),
            Some("root\\mttvdd")
        );
        assert!(is_vdd_hwid(&["PCI\\VEN_10DE".to_owned()]).is_none());
    }

    #[test]
    fn node_states() {
        let mut n = VddNode {
            instance_id: "ROOT\\DISPLAY\\0000".into(),
            hardware_id: "Root\\MttVDD".into(),
            status: DN_STARTED,
            problem: 0,
            has_driver: true,
        };
        assert!(n.started() && n.usable() && !n.disabled());
        n.status = 0;
        n.problem = CM_PROB_DISABLED;
        assert!(n.disabled() && n.usable() && !n.started());
        n.problem = 10;
        assert!(!n.usable());
        n.problem = 0;
        n.has_driver = false;
        assert!(!n.usable());
    }

    #[test]
    fn read_only_queries_work() {
        // Does not change anything; the node is usually absent on a dev machine.
        let node = find_node().expect("SetupAPI enumeration");
        eprintln!("vdd node: {node:?}, elevated: {}", is_elevated());
        let _ = driver_store_has_package();
    }
}
