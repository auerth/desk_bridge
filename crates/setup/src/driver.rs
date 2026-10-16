//! Driver package handling (plan 12.2): stage the package into the driver store, create the
//! root-enumerated device exactly once, bind the driver; and the reverse for uninstall.
//!
//! The pure parts (spec validation, catalogue, INF checks) are unit-tested. The SetupAPI calls
//! are in [`sys`] (Windows only) and are only reachable through `driver install|uninstall`,
//! `install` (driver catalogue) and `uninstall` (installed.json).

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::brand::DRIVER_CATALOGUE;
use crate::inf::{InfInfo, decode_inf, parse_guid, parse_inf};

/// A file copied into place before the device is created (e.g. the driver's settings XML).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCopy {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// One driver to install.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverSpec {
    pub inf: PathBuf,
    pub hwid: String,
    pub name: String,
    /// Config files to place before the device is created.
    pub files: Vec<FileCopy>,
    /// Publisher certificate to trust before installing (third-party signed user-mode drivers).
    pub trust_cert: Option<TrustCert>,
}

/// A pinned publisher certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustCert {
    pub file: PathBuf,
    /// SHA-1 thumbprint the file must have.
    pub sha1: String,
}

/// A validated spec together with what the INF says.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub spec: DriverSpec,
    /// Absolute INF path without `\\?\`.
    pub inf_abs: PathBuf,
    pub info: InfInfo,
}

/// Result of an install attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallOutcome {
    pub dry_run: bool,
    /// The device already existed (nothing was created).
    pub device_existed: bool,
    pub device_created: bool,
    pub reboot_required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub dry_run: bool,
    pub devices_removed: usize,
    pub packages_removed: usize,
    pub reboot_required: bool,
}

/// Read and validate: the INF must exist, parse, and actually declare `hwid`.
pub fn prepare(spec: &DriverSpec) -> Result<Prepared> {
    if spec.hwid.trim().is_empty() || spec.hwid.contains('\0') {
        bail!("empty or invalid hardware id");
    }
    let inf_abs = crate::paths::strip_verbatim(
        &std::fs::canonicalize(&spec.inf)
            .with_context(|| format!("INF not found: {}", spec.inf.display()))?,
    );
    let bytes =
        std::fs::read(&inf_abs).with_context(|| format!("reading {}", inf_abs.display()))?;
    let info = parse_inf(&decode_inf(&bytes));
    if !info.declares(&spec.hwid) {
        bail!(
            "{} does not declare hardware id {:?} (declared: {})",
            inf_abs.display(),
            spec.hwid,
            if info.hwids.is_empty() {
                "none".to_owned()
            } else {
                info.hwids.join(", ")
            }
        );
    }
    if info.class_guid.as_deref().and_then(parse_guid).is_none() && info.class.is_none() {
        bail!(
            "{} has neither ClassGuid nor Class in [Version]",
            inf_abs.display()
        );
    }
    Ok(Prepared {
        spec: spec.clone(),
        inf_abs,
        info,
    })
}

#[derive(Debug, Deserialize)]
struct CatalogueEntry {
    name: String,
    /// Relative to the install dir.
    inf: String,
    hwid: String,
    #[serde(default = "yes")]
    enabled: bool,
    /// Optional files to copy before device creation; `from` is relative to the install dir,
    /// `to` is an absolute destination.
    #[serde(default)]
    files: Vec<CatalogueFile>,
    #[serde(default)]
    trust_certificate: Option<CatalogueCert>,
}

#[derive(Debug, Deserialize)]
struct CatalogueCert {
    /// Relative to the install dir.
    file: String,
    sha1: String,
}

#[derive(Debug, Deserialize)]
struct CatalogueFile {
    from: String,
    to: String,
}

fn is_inside(rel: &Path) -> bool {
    !rel.is_absolute()
        && !rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
}

/// Copy the files of a spec (creating destination directories). Existing files are replaced,
/// except that identical content is left alone so user edits survive only if unchanged by us
/// (we always overwrite: the shipped config is authoritative).
pub fn copy_files(spec: &DriverSpec, dry_run: bool) -> Result<()> {
    for f in &spec.files {
        if dry_run {
            println!(
                "[dry-run]   copy {} -> {}",
                f.from.display(),
                f.to.display()
            );
            continue;
        }
        if let Some(parent) = f.to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(&f.from, &f.to)
            .with_context(|| format!("copying {} to {}", f.from.display(), f.to.display()))?;
    }
    Ok(())
}

fn yes() -> bool {
    true
}

/// Parse `drivers/drivers.json` (`[{"name":..,"inf":"drivers/display/x.inf","hwid":"Root\\X"}]`).
/// INF paths must be relative and stay inside `install_dir`.
pub fn parse_catalogue(json: &str, install_dir: &Path) -> Result<Vec<DriverSpec>> {
    let entries: Vec<CatalogueEntry> =
        serde_json::from_str(json).context("parsing drivers.json")?;
    let mut out = Vec::new();
    for e in entries.into_iter().filter(|e| e.enabled) {
        let rel = Path::new(&e.inf);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        {
            bail!(
                "driver catalogue entry {:?}: inf path must be relative and inside the install dir",
                e.name
            );
        }
        let mut files = Vec::new();
        for f in e.files {
            let from = Path::new(&f.from);
            if !is_inside(from) || !Path::new(&f.to).is_absolute() {
                bail!(
                    "driver catalogue entry {:?}: file `from` must be relative, `to` absolute",
                    e.name
                );
            }
            files.push(FileCopy {
                from: install_dir.join(from),
                to: PathBuf::from(f.to),
            });
        }
        let trust_cert = match e.trust_certificate {
            Some(c) => {
                if !is_inside(Path::new(&c.file)) {
                    bail!(
                        "driver catalogue entry {:?}: certificate path must be relative",
                        e.name
                    );
                }
                Some(TrustCert {
                    file: install_dir.join(&c.file),
                    sha1: c.sha1,
                })
            }
            None => None,
        };
        out.push(DriverSpec {
            inf: install_dir.join(rel),
            hwid: e.hwid,
            name: e.name,
            files,
            trust_cert,
        });
    }
    Ok(out)
}

/// Load the catalogue shipped with the installer; empty when there is none.
pub fn load_catalogue(install_dir: &Path) -> Result<Vec<DriverSpec>> {
    let p = install_dir.join(DRIVER_CATALOGUE);
    match std::fs::read_to_string(&p) {
        Ok(text) => parse_catalogue(&text, install_dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

#[cfg(windows)]
pub use sys::{device_present, install, uninstall};

#[cfg(windows)]
mod sys {
    use std::path::PathBuf;

    use anyhow::{Context, Result, bail};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_ALLCLASSES, DIINSTALLDRIVER_FLAGS,
        DIUNINSTALLDRIVER_FLAGS, DiInstallDriverW, DiUninstallDriverW, HDEVINFO, SP_DEVINFO_DATA,
        SPDRP_HARDWAREID, SetupDiCallClassInstaller, SetupDiClassGuidsFromNameW,
        SetupDiClassNameFromGuidW, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW,
        SetupDiSetDeviceRegistryPropertyW, UPDATEDRIVERFORPLUGANDPLAYDEVICES_FLAGS,
        UpdateDriverForPlugAndPlayDevicesW,
    };
    use windows::Win32::Devices::Properties::{DEVPKEY_Device_DriverInfPath, DEVPROPTYPE};
    use windows::Win32::Foundation::{BOOL, ERROR_NO_MORE_ITEMS, HWND};
    use windows::core::{GUID, HSTRING, PCWSTR};

    use super::{DriverSpec, InstallOutcome, Prepared, UninstallOutcome, prepare};
    use crate::inf::{multi_sz_bytes, parse_guid};

    /// RAII for a device information set.
    struct DevInfoSet(HDEVINFO);
    impl Drop for DevInfoSet {
        fn drop(&mut self) {
            let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    fn new_data() -> SP_DEVINFO_DATA {
        SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        }
    }

    fn utf16_to_strings(bytes: &[u8]) -> Vec<String> {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        units
            .split(|&u| u == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    fn hardware_ids(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Vec<String> {
        let mut needed = 0u32;
        // First call only reports the required size (fails with ERROR_INSUFFICIENT_BUFFER).
        let _ = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set,
                data,
                SPDRP_HARDWAREID,
                None,
                None,
                Some(&mut needed),
            )
        };
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set,
                data,
                SPDRP_HARDWAREID,
                None,
                Some(&mut buf),
                None,
            )
        };
        if ok.is_err() {
            Vec::new()
        } else {
            utf16_to_strings(&buf)
        }
    }

    /// Calls `f` for every device (present or not, any class) whose hardware IDs contain `hwid`.
    fn for_each_device(
        hwid: &str,
        mut f: impl FnMut(HDEVINFO, &mut SP_DEVINFO_DATA) -> Result<()>,
    ) -> Result<usize> {
        let set = DevInfoSet(
            unsafe {
                SetupDiGetClassDevsW(None, PCWSTR::null(), HWND::default(), DIGCF_ALLCLASSES)
            }
            .context("SetupDiGetClassDevsW")?,
        );
        let mut count = 0usize;
        let mut index = 0u32;
        loop {
            let mut data = new_data();
            match unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) } {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
                Err(e) => return Err(e).context("SetupDiEnumDeviceInfo"),
            }
            index += 1;
            if hardware_ids(set.0, &data)
                .iter()
                .any(|h| crate::inf::hwid_eq(h, hwid))
            {
                count += 1;
                f(set.0, &mut data)?;
            }
        }
        Ok(count)
    }

    /// Read-only: does a (possibly non-present) device with this hardware ID exist?
    pub fn device_present(hwid: &str) -> Result<bool> {
        Ok(for_each_device(hwid, |_, _| Ok(()))? > 0)
    }

    fn class_guid_and_name(p: &Prepared) -> Result<(GUID, String)> {
        let guid = if let Some((a, b, c, d)) = p.info.class_guid.as_deref().and_then(parse_guid) {
            GUID {
                data1: a,
                data2: b,
                data3: c,
                data4: d,
            }
        } else {
            let name = p.info.class.clone().context("INF has no class")?;
            let mut list = [GUID::zeroed(); 1];
            let mut needed = 0u32;
            unsafe {
                SetupDiClassGuidsFromNameW(&HSTRING::from(name.as_str()), &mut list, &mut needed)
            }
            .context("SetupDiClassGuidsFromNameW")?;
            if needed == 0 {
                bail!("unknown device class {name:?}");
            }
            list[0]
        };
        let name = match &p.info.class {
            Some(n) => n.clone(),
            None => {
                let mut buf = [0u16; 128];
                unsafe { SetupDiClassNameFromGuidW(&guid, &mut buf, None) }
                    .context("SetupDiClassNameFromGuidW")?;
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                String::from_utf16_lossy(&buf[..len])
            }
        };
        Ok((guid, name))
    }

    fn create_device(p: &Prepared) -> Result<()> {
        let (guid, class_name) = class_guid_and_name(p)?;
        let set = DevInfoSet(
            unsafe { SetupDiCreateDeviceInfoList(Some(&guid), HWND::default()) }
                .context("SetupDiCreateDeviceInfoList")?,
        );
        let mut data = new_data();
        unsafe {
            SetupDiCreateDeviceInfoW(
                set.0,
                &HSTRING::from(class_name.as_str()),
                &guid,
                &HSTRING::from(p.spec.name.as_str()),
                HWND::default(),
                DICD_GENERATE_ID,
                Some(&mut data),
            )
        }
        .context("SetupDiCreateDeviceInfoW")?;
        unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                set.0,
                &mut data,
                SPDRP_HARDWAREID,
                Some(&multi_sz_bytes(&p.spec.hwid)),
            )
        }
        .context("SetupDiSetDeviceRegistryPropertyW(HARDWAREID)")?;
        unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, Some(&data)) }
            .context("SetupDiCallClassInstaller(DIF_REGISTERDEVICE)")?;
        Ok(())
    }

    /// Stage the package, create the device once, bind the driver.
    ///
    /// Idempotent: a second call finds the device and only re-runs the (version-gated) driver
    /// update. `dry_run` validates the INF, queries the device state read-only and changes nothing.
    pub fn install(spec: &DriverSpec, dry_run: bool) -> Result<InstallOutcome> {
        let p = prepare(spec)?;
        let existed = device_present(&spec.hwid)?;
        let mut out = InstallOutcome {
            dry_run,
            device_existed: existed,
            ..Default::default()
        };
        if dry_run {
            return Ok(out);
        }

        let inf = HSTRING::from(p.inf_abs.as_os_str());
        let mut reboot = BOOL(0);
        match unsafe {
            DiInstallDriverW(
                HWND::default(),
                &inf,
                DIINSTALLDRIVER_FLAGS(0),
                Some(&mut reboot),
            )
        } {
            Ok(()) => out.reboot_required |= reboot.as_bool(),
            // The same (or a newer) package is installed already: every reinstall reported this
            // as a failed setup step.
            Err(e) if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() => {
                tracing::info!(
                    "driver package {} is already installed",
                    p.inf_abs.display()
                );
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("DiInstallDriverW({})", p.inf_abs.display()));
            }
        }

        if !existed {
            create_device(&p)?;
            out.device_created = true;
        }

        let mut reboot = BOOL(0);
        match unsafe {
            UpdateDriverForPlugAndPlayDevicesW(
                HWND::default(),
                &HSTRING::from(spec.hwid.as_str()),
                &inf,
                UPDATEDRIVERFORPLUGANDPLAYDEVICES_FLAGS(0),
                Some(&mut reboot),
            )
        } {
            Ok(()) => out.reboot_required |= reboot.as_bool(),
            // Installed driver is already as good or better: nothing to update.
            Err(e) if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() => {
                tracing::info!("driver for {} is already up to date", spec.hwid);
            }
            Err(e) => return Err(e).context("UpdateDriverForPlugAndPlayDevicesW"),
        }
        Ok(out)
    }

    fn driver_inf_name(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<String> {
        let mut ty = DEVPROPTYPE::default();
        let mut buf = vec![0u8; 520];
        let mut needed = 0u32;
        unsafe {
            SetupDiGetDevicePropertyW(
                set,
                data,
                &DEVPKEY_Device_DriverInfPath,
                &mut ty,
                Some(&mut buf),
                Some(&mut needed),
                0,
            )
        }
        .ok()?;
        utf16_to_strings(&buf[..(needed as usize).min(buf.len())])
            .into_iter()
            .next()
    }

    /// Remove all devices with this hardware ID, then the driver packages they used.
    pub fn uninstall(hwid: &str, dry_run: bool) -> Result<UninstallOutcome> {
        let mut out = UninstallOutcome {
            dry_run,
            ..Default::default()
        };
        let mut infs: Vec<String> = Vec::new();
        let found = for_each_device(hwid, |set, data| {
            if let Some(inf) = driver_inf_name(set, data)
                && !infs.iter().any(|i| i.eq_ignore_ascii_case(&inf))
            {
                infs.push(inf);
            }
            if !dry_run {
                unsafe { SetupDiCallClassInstaller(DIF_REMOVE, set, Some(&*data)) }
                    .context("SetupDiCallClassInstaller(DIF_REMOVE)")?;
            }
            Ok(())
        })?;
        out.devices_removed = found;

        let windir = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        for inf in infs {
            // Only OEM-published packages (oemNN.inf) are ours to delete; never touch inbox INFs.
            let lower = inf.to_ascii_lowercase();
            if !(lower.starts_with("oem") && lower.ends_with(".inf")) {
                tracing::warn!("not removing non-OEM package {inf}");
                continue;
            }
            if dry_run {
                out.packages_removed += 1;
                continue;
            }
            let path = windir.join("INF").join(&inf);
            let mut reboot = BOOL(0);
            match unsafe {
                DiUninstallDriverW(
                    HWND::default(),
                    &HSTRING::from(path.as_os_str()),
                    DIUNINSTALLDRIVER_FLAGS(0),
                    Some(&mut reboot),
                )
            } {
                Ok(()) => {
                    out.packages_removed += 1;
                    out.reboot_required |= reboot.as_bool();
                }
                Err(e) => tracing::warn!("DiUninstallDriverW({}): {e}", path.display()),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INF: &str = "[Version]\nClass=Display\nClassGuid={4d36e968-e325-11ce-bfc1-08002be10318}\n\
[Manufacturer]\n%m%=Std,NTamd64\n[Std.NTamd64]\n%d%=Inst,Root\\TestVdd\n";

    fn write_inf(dir: &Path) -> PathBuf {
        let p = dir.join("t.inf");
        std::fs::write(&p, INF).unwrap();
        p
    }

    #[test]
    fn prepare_accepts_declared_hwid() {
        let d = tempfile::tempdir().unwrap();
        let spec = DriverSpec {
            inf: write_inf(d.path()),
            hwid: r"root\testvdd".into(),
            name: "T".into(),
            ..Default::default()
        };
        let p = prepare(&spec).unwrap();
        assert!(p.inf_abs.is_absolute());
        assert!(!p.inf_abs.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(p.info.class.as_deref(), Some("Display"));
    }

    #[test]
    fn prepare_rejects_undeclared_hwid_missing_inf_and_empty_id() {
        let d = tempfile::tempdir().unwrap();
        let inf = write_inf(d.path());
        let bad = DriverSpec {
            inf: inf.clone(),
            hwid: r"Root\Other".into(),
            name: "T".into(),
            ..Default::default()
        };
        assert!(
            prepare(&bad)
                .unwrap_err()
                .to_string()
                .contains("does not declare")
        );
        let missing = DriverSpec {
            inf: d.path().join("nope.inf"),
            hwid: "x".into(),
            name: "T".into(),
            ..Default::default()
        };
        assert!(prepare(&missing).is_err());
        let empty = DriverSpec {
            inf,
            hwid: "  ".into(),
            name: "T".into(),
            ..Default::default()
        };
        assert!(prepare(&empty).is_err());
    }

    #[test]
    fn catalogue_parsing_and_path_safety() {
        let dir = Path::new(r"C:\Program Files\softwelop\Desk Bridge");
        let specs = parse_catalogue(
            r#"[{"name":"Display","inf":"drivers/display/vdd.inf","hwid":"Root\\Vdd"},
                {"name":"Off","inf":"drivers/x.inf","hwid":"Root\\X","enabled":false}]"#,
            dir,
        )
        .unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].inf, dir.join("drivers/display/vdd.inf"));
        for bad in [
            r#"[{"name":"a","inf":"../evil.inf","hwid":"x"}]"#,
            r#"[{"name":"a","inf":"C:\\evil.inf","hwid":"x"}]"#,
        ] {
            assert!(parse_catalogue(bad, dir).is_err(), "{bad}");
        }
        assert!(parse_catalogue("not json", dir).is_err());
    }

    #[test]
    fn catalogue_files_are_validated_and_copied() {
        let d = tempfile::tempdir().unwrap();
        let dst = d.path().join("out").join("cfg.xml");
        std::fs::create_dir_all(d.path().join("drivers")).unwrap();
        std::fs::write(d.path().join("drivers").join("cfg.xml"), "x").unwrap();
        let json = format!(
            r#"[{{"name":"A","inf":"drivers/a.inf","hwid":"Root\\A","files":[{{"from":"drivers/cfg.xml","to":{}}}]}}]"#,
            serde_json::to_string(&dst.display().to_string()).unwrap()
        );
        let specs = parse_catalogue(&json, d.path()).unwrap();
        assert_eq!(specs[0].files.len(), 1);
        copy_files(&specs[0], false).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "x");
        let bad =
            r#"[{"name":"A","inf":"a.inf","hwid":"x","files":[{"from":"../x","to":"C:\\y"}]}]"#;
        assert!(parse_catalogue(bad, d.path()).is_err());
        let bad2 = r#"[{"name":"A","inf":"a.inf","hwid":"x","files":[{"from":"x","to":"rel"}]}]"#;
        assert!(parse_catalogue(bad2, d.path()).is_err());
    }

    #[test]
    fn missing_catalogue_is_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(load_catalogue(d.path()).unwrap().is_empty());
    }
}
