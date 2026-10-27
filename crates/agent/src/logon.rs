//! Control at the Windows logon screen (ADR 0008).
//!
//! Nobody is signed in after a reboot, so the user's agent (and its `%APPDATA%` profile) does not
//! exist yet. The service then starts `deskbridge-agent --logon-screen` as SYSTEM on
//! `winsta0\Winlogon`; that agent needs the same device identity and the same paired partner,
//! otherwise the partner's certificate pinning would reject it.
//!
//! * The **user's agent** keeps a copy of `identity.bin`, `peers.toml` and `config.toml` in
//!   `%ProgramData%\softwelop\DeskBridge\logon\config` ([`sync_task`]). The identity is
//!   re-encrypted with machine-scope DPAPI (SYSTEM has no access to the user's DPAPI key); the
//!   directory is created with a protected DACL for SYSTEM and Administrators only, owner
//!   Administrators. With `general.logon_screen = false` the copy is deleted, which is also what
//!   tells the service not to start the logon agent.
//! * The **logon agent** only uses the copy when every part of it is owned by SYSTEM or
//!   Administrators ([`verify_trusted`]): a standard user who pre-created the directory (or
//!   planted a `peers.toml`) cannot make a SYSTEM process accept their device.

use deskbridge_core::config::Config;
use deskbridge_core::paths::AppPaths;
use deskbridge_net::identity::Identity;
use deskbridge_platform::secret::SecretProtector;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// `%ProgramData%\softwelop\DeskBridge`
#[must_use]
pub fn program_data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
        .join("softwelop")
        .join("DeskBridge")
}

/// Root of the logon-screen profile: `%ProgramData%\softwelop\DeskBridge\logon`.
#[must_use]
pub fn root() -> PathBuf {
    program_data_dir().join("logon")
}

/// Paths of the logon-screen agent (`<root>\config`, `<root>\data`).
#[must_use]
pub fn paths() -> AppPaths {
    AppPaths::under(&root())
}

/// The logon agent logs next to the service (`%ProgramData%\...\logs\logon-agent.log.*`), where
/// the partner's "Protokolle" view and the diagnostics already look.
#[must_use]
pub fn logs_dir() -> PathBuf {
    program_data_dir().join("logs")
}

/// What [`sync_profile`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synced {
    /// The copy was already current.
    Unchanged,
    Written,
    /// `general.logon_screen` is off or this device is the input source: the copy was removed
    /// (or never existed).
    Disabled,
}

/// Seam for the Windows security calls, so the copy logic is testable with a temp dir.
pub trait DirSecurity {
    /// Creates `dir` readable by SYSTEM and administrators only (or checks an existing one) and
    /// fails when an existing directory belongs to someone else.
    fn secure_dir(&self, dir: &Path) -> io::Result<()>;
    /// Makes a file we just wrote owned by Administrators (best effort).
    fn own_file(&self, file: &Path);
}

/// Brings the logon copy in line with the user's profile.
pub fn sync_profile(
    user: &AppPaths,
    logon_root: &Path,
    user_secrets: &dyn SecretProtector,
    machine_secrets: &dyn SecretProtector,
    security: &dyn DirSecurity,
) -> io::Result<Synced> {
    let config = Config::load(&user.config_file()).map_err(io::Error::other)?;
    // Only a target device is controlled from the partner; the input source (PC) would only
    // sit at its logon screen with nothing to receive.
    if !config.general.logon_screen || config.device.input_source {
        return match std::fs::remove_dir_all(logon_root) {
            Ok(()) => Ok(Synced::Disabled),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Synced::Disabled),
            Err(e) => Err(e),
        };
    }
    if !user.identity_file().exists() {
        // The engine creates the identity on its first start; nothing to copy before that.
        return Ok(Synced::Unchanged);
    }
    security.secure_dir(logon_root)?;
    let logon = AppPaths::under(logon_root);
    std::fs::create_dir_all(&logon.config_dir)?;

    let mut wrote = false;
    // identity: re-encrypt for the machine scope, rewrite only when it changed
    let id =
        Identity::load_or_create(&user.identity_file(), user_secrets).map_err(io::Error::other)?;
    let current = load_identity(&logon.identity_file(), machine_secrets);
    if current.is_none_or(|c| c.device_id != id.device_id || c.fingerprint != id.fingerprint) {
        id.save(&logon.identity_file(), machine_secrets)
            .map_err(io::Error::other)?;
        security.own_file(&logon.identity_file());
        wrote = true;
    }
    // peers and config: byte copies
    for (from, to) in [
        (user.peers_file(), logon.peers_file()),
        (user.config_file(), logon.config_file()),
    ] {
        let new = match std::fs::read(&from) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        if std::fs::read(&to).ok().as_deref() != Some(new.as_slice()) {
            let tmp = to.with_extension("tmp");
            std::fs::write(&tmp, &new)?;
            std::fs::rename(&tmp, &to)?;
            security.own_file(&to);
            wrote = true;
        }
    }
    Ok(if wrote {
        Synced::Written
    } else {
        Synced::Unchanged
    })
}

fn load_identity(path: &Path, secrets: &dyn SecretProtector) -> Option<Identity> {
    // `load_or_create` would create one: only load an existing file.
    path.exists()
        .then(|| Identity::load_or_create(path, secrets).ok())
        .flatten()
}

/// Is a logon profile there (what the service checks before it starts the logon agent)?
#[must_use]
pub fn profile_present(logon_root: &Path) -> bool {
    AppPaths::under(logon_root).identity_file().is_file()
}

/// Checks that the logon profile can be trusted by a SYSTEM process: the root, its config
/// directory and the three files exist and are owned by SYSTEM or Administrators.
pub fn verify_trusted(
    logon: &AppPaths,
    owner_ok: &dyn Fn(&Path) -> io::Result<bool>,
) -> Result<(), String> {
    let root = logon
        .config_dir
        .parent()
        .map_or_else(|| logon.config_dir.clone(), Path::to_path_buf);
    let parts = [
        root,
        logon.config_dir.clone(),
        logon.identity_file(),
        logon.peers_file(),
        logon.config_file(),
    ];
    for p in &parts {
        match owner_ok(p) {
            Ok(true) => {}
            Ok(false) => {
                return Err(format!(
                    "{} is not owned by SYSTEM or Administrators",
                    p.display()
                ));
            }
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    Ok(())
}

/// Signature of the inputs: the copy is only refreshed when one of them changed.
fn inputs_stamp(user: &AppPaths) -> Vec<Option<(SystemTime, u64)>> {
    [user.config_file(), user.peers_file(), user.identity_file()]
        .iter()
        .map(|p| {
            std::fs::metadata(p)
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())))
        })
        .collect()
}

/// How often the user's agent looks for changes of its profile.
const SYNC_EVERY: Duration = Duration::from_secs(20);

/// Runs in the user's agent: keeps the logon copy current (pairing, layout, settings).
#[cfg(windows)]
pub async fn sync_task(user: AppPaths) {
    use deskbridge_platform_windows::secret::DpapiProtector;
    let mut last_stamp = None;
    let mut last_error: Option<String> = None;
    loop {
        let stamp = inputs_stamp(&user);
        if last_stamp.as_ref() != Some(&stamp) {
            let u = user.clone();
            let r = tokio::task::spawn_blocking(move || {
                sync_profile(
                    &u,
                    &root(),
                    &DpapiProtector::new(),
                    &DpapiProtector::machine(),
                    &win::WinDirSecurity,
                )
            })
            .await
            .unwrap_or_else(|e| Err(io::Error::other(e.to_string())));
            match r {
                Ok(s) => {
                    if s != Synced::Unchanged {
                        tracing::info!(result = ?s, dir = %root().display(), "logon-screen profile");
                    }
                    last_error = None;
                    last_stamp = Some(stamp);
                }
                Err(e) => {
                    // Typically: the agent runs without administrator rights.
                    let msg = e.to_string();
                    if last_error.as_deref() != Some(msg.as_str()) {
                        tracing::warn!("logon-screen profile not updated: {msg}");
                    }
                    last_error = Some(msg);
                }
            }
        }
        tokio::time::sleep(SYNC_EVERY).await;
    }
}

/// Resolves once somebody signed in at the console. The service ends the logon agent at the
/// sign-in; this is the fallback when it cannot (service stopped or crashed): the user's agent
/// needs the port.
#[cfg(windows)]
pub async fn wait_for_sign_in() {
    use deskbridge_service::session;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(user) = session::active_console_session().and_then(session::session_user) {
            tracing::info!(%user, "signed in, the logon-screen agent ends");
            return;
        }
    }
}

#[cfg(windows)]
pub mod win {
    //! Owner checks and the protected directory.

    use super::DirSecurity;
    use std::ffi::c_void;
    use std::io;
    use std::path::Path;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows::Win32::Security::{
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    };
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows::core::{HSTRING, PWSTR};

    pub const SYSTEM_SID: &str = "S-1-5-18";
    pub const ADMINS_SID: &str = "S-1-5-32-544";
    /// Owner Administrators; SYSTEM and Administrators full access, nothing inherited from
    /// `%ProgramData%` (where every user may create files).
    const ROOT_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

    /// SID string of the owner of `path`.
    pub fn owner_sid(path: &Path) -> io::Result<String> {
        let name = HSTRING::from(path.as_os_str());
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid out pointers; the descriptor (which `owner` points into) is freed below
        // after the SID was converted to a string.
        unsafe {
            let r = GetNamedSecurityInfoW(
                &name,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                None,
                None,
                &mut sd,
            );
            if r.is_err() {
                return Err(io::Error::from_raw_os_error(r.0 as i32));
            }
            let mut s = PWSTR::null();
            let conv = ConvertSidToStringSidW(owner, &mut s);
            let out = conv.ok().and_then(|()| s.to_string().ok());
            if !s.is_null() {
                let _ = LocalFree(HLOCAL(s.0.cast()));
            }
            let _ = LocalFree(HLOCAL(sd.0));
            out.ok_or_else(|| io::Error::other("owner SID not readable"))
        }
    }

    /// Owned by SYSTEM or Administrators?
    pub fn owner_is_trusted(path: &Path) -> io::Result<bool> {
        let sid = owner_sid(path)?;
        Ok(sid == SYSTEM_SID || sid == ADMINS_SID)
    }

    fn create_protected_dir(dir: &Path) -> io::Result<()> {
        // SAFETY: the descriptor stays alive until CreateDirectoryW returned and is freed once.
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(ROOT_SDDL),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            let r = CreateDirectoryW(&HSTRING::from(dir.as_os_str()), Some(&raw const sa));
            let _ = LocalFree(HLOCAL(sd.0));
            r.map_err(|e| io::Error::other(format!("{}: {e}", dir.display())))
        }
    }

    pub struct WinDirSecurity;

    impl DirSecurity for WinDirSecurity {
        fn secure_dir(&self, dir: &Path) -> io::Result<()> {
            if dir.exists() {
                if owner_is_trusted(dir)? {
                    return Ok(());
                }
                // Somebody else made it (before the installer did): never trust, start over.
                tracing::warn!(dir = %dir.display(), "logon profile directory has a foreign owner, recreating it");
                std::fs::remove_dir_all(dir)?;
            }
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            create_protected_dir(dir)
        }

        fn own_file(&self, file: &Path) {
            let name = HSTRING::from(file.as_os_str());
            let mut admins = PSID::default();
            // SAFETY: the SID from ConvertStringSidToSidW is freed with LocalFree after use.
            unsafe {
                if ConvertStringSidToSidW(&HSTRING::from(ADMINS_SID), &mut admins).is_err() {
                    return;
                }
                let r = SetNamedSecurityInfoW(
                    &name,
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION,
                    admins,
                    PSID::default(),
                    None,
                    None,
                );
                if r.is_err() {
                    tracing::debug!(file = %file.display(), code = r.0, "cannot set owner");
                }
                let _ = LocalFree(HLOCAL(admins.0.cast::<c_void>()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::secret::PlainProtector;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recorder {
        dirs: RefCell<Vec<PathBuf>>,
        files: RefCell<Vec<PathBuf>>,
    }

    impl DirSecurity for Recorder {
        fn secure_dir(&self, dir: &Path) -> io::Result<()> {
            std::fs::create_dir_all(dir)?;
            self.dirs.borrow_mut().push(dir.to_path_buf());
            Ok(())
        }
        fn own_file(&self, file: &Path) {
            self.files.borrow_mut().push(file.to_path_buf());
        }
    }

    /// "Machine" protector for tests: distinguishable from the user one.
    struct Xor;
    impl SecretProtector for Xor {
        fn protect(&self, plain: &[u8]) -> deskbridge_platform::Result<Vec<u8>> {
            Ok(plain.iter().map(|b| b ^ 0x5a).collect())
        }
        fn unprotect(&self, c: &[u8]) -> deskbridge_platform::Result<Vec<u8>> {
            self.protect(c)
        }
    }

    fn user_profile(dir: &Path) -> (AppPaths, Identity) {
        let user = AppPaths::under(&dir.join("user"));
        user.ensure().unwrap();
        let id = Identity::load_or_create(&user.identity_file(), &PlainProtector).unwrap();
        std::fs::write(user.peers_file(), "[[peer]]\nid = \"x\"\n").unwrap();
        let mut c = Config::default();
        c.device.input_source = false; // the laptop
        c.save(&user.config_file()).unwrap();
        (user, id)
    }

    #[test]
    fn copies_identity_peers_and_config_once() {
        let dir = tempfile::tempdir().unwrap();
        let (user, id) = user_profile(dir.path());
        let root = dir.path().join("logon");
        let sec = Recorder::default();

        let r = sync_profile(&user, &root, &PlainProtector, &Xor, &sec).unwrap();
        assert_eq!(r, Synced::Written);
        assert!(profile_present(&root));
        let logon = AppPaths::under(&root);
        let copy = Identity::load_or_create(&logon.identity_file(), &Xor).unwrap();
        assert_eq!(copy.device_id, id.device_id);
        assert_eq!(copy.fingerprint, id.fingerprint);
        assert!(
            Identity::load_or_create(&logon.identity_file(), &PlainProtector).is_err(),
            "the copy is encrypted for the machine scope, not stored like the user's"
        );
        assert_eq!(
            std::fs::read(logon.peers_file()).unwrap(),
            std::fs::read(user.peers_file()).unwrap()
        );
        assert_eq!(sec.dirs.borrow().as_slice(), std::slice::from_ref(&root));
        assert_eq!(sec.files.borrow().len(), 3);

        // nothing changed: nothing rewritten
        assert_eq!(
            sync_profile(&user, &root, &PlainProtector, &Xor, &sec).unwrap(),
            Synced::Unchanged
        );
        assert_eq!(sec.files.borrow().len(), 3);

        // a new pairing is copied
        std::fs::write(user.peers_file(), "[[peer]]\nid = \"y\"\n").unwrap();
        assert_eq!(
            sync_profile(&user, &root, &PlainProtector, &Xor, &sec).unwrap(),
            Synced::Written
        );
        assert_eq!(
            std::fs::read_to_string(logon.peers_file()).unwrap(),
            "[[peer]]\nid = \"y\"\n"
        );
    }

    #[test]
    fn switching_the_setting_off_removes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (user, _) = user_profile(dir.path());
        let root = dir.path().join("logon");
        let sec = Recorder::default();
        sync_profile(&user, &root, &PlainProtector, &Xor, &sec).unwrap();
        assert!(profile_present(&root));

        let mut c = Config::load(&user.config_file()).unwrap();
        c.general.logon_screen = false;
        c.save(&user.config_file()).unwrap();
        assert_eq!(
            sync_profile(&user, &root, &PlainProtector, &Xor, &sec).unwrap(),
            Synced::Disabled
        );
        assert!(!root.exists());
        assert!(!profile_present(&root));
        // and stays a no-op
        assert_eq!(
            sync_profile(&user, &root, &PlainProtector, &Xor, &sec).unwrap(),
            Synced::Disabled
        );
    }

    #[test]
    fn the_input_source_keeps_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (user, _) = user_profile(dir.path());
        let root = dir.path().join("logon");
        sync_profile(&user, &root, &PlainProtector, &Xor, &Recorder::default()).unwrap();
        assert!(profile_present(&root));
        let mut c = Config::load(&user.config_file()).unwrap();
        c.device.input_source = true;
        c.save(&user.config_file()).unwrap();
        assert_eq!(
            sync_profile(&user, &root, &PlainProtector, &Xor, &Recorder::default()).unwrap(),
            Synced::Disabled
        );
        assert!(!root.exists());
    }

    #[test]
    fn nothing_is_copied_before_the_identity_exists() {
        let dir = tempfile::tempdir().unwrap();
        let user = AppPaths::under(&dir.path().join("user"));
        user.ensure().unwrap();
        let mut c = Config::default();
        c.device.input_source = false;
        c.save(&user.config_file()).unwrap();
        let root = dir.path().join("logon");
        let r = sync_profile(&user, &root, &PlainProtector, &Xor, &Recorder::default()).unwrap();
        assert_eq!(r, Synced::Unchanged);
        assert!(!root.exists());
        assert!(
            !user.identity_file().exists(),
            "never creates an identity itself"
        );
    }

    #[test]
    fn untrusted_parts_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let logon = AppPaths::under(&dir.path().join("logon"));
        let peers = logon.peers_file();
        assert!(verify_trusted(&logon, &|_| Ok(true)).is_ok());
        let err = verify_trusted(&logon, &|p| Ok(p != peers)).unwrap_err();
        assert!(err.contains("peers.toml"), "{err}");
        assert!(verify_trusted(&logon, &|_| Err(io::Error::other("missing"))).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn owner_of_a_temp_file_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let sid = win::owner_sid(dir.path()).unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
    }
}
