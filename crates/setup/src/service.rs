//! Raw Service Control Manager calls for `deskbridge-service.exe` (built in M6).
//!
//! Every function tolerates a missing service/executable so `install`/`uninstall` can call them
//! unconditionally.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
    SC_MANAGER_CREATE_SERVICE, SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL, SERVICE_QUERY_STATUS,
    SERVICE_STATUS, SERVICE_STOP, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS, StartServiceW,
};
use windows::Win32::System::Services::{SERVICE_ALL_ACCESS, SERVICE_START};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::brand::{SERVICE_DISPLAY_NAME, SERVICE_NAME};

/// RAII wrapper so handles are closed on every path.
struct Handle(SC_HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn scm(access: u32) -> Result<Handle> {
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) }
        .map(Handle)
        .context("OpenSCManagerW")
}

fn open(scm: &Handle, access: u32) -> windows::core::Result<Handle> {
    unsafe { OpenServiceW(scm.0, &HSTRING::from(SERVICE_NAME), access) }.map(Handle)
}

fn is_missing(e: &windows::core::Error) -> bool {
    e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult()
}

/// Read-only: `Some(running)` if the service is registered.
pub fn query() -> Result<Option<bool>> {
    let scm = scm(SC_MANAGER_CONNECT)?;
    match open(&scm, SERVICE_QUERY_STATUS) {
        Ok(svc) => {
            let mut st = SERVICE_STATUS::default();
            unsafe { QueryServiceStatus(svc.0, &mut st) }.context("QueryServiceStatus")?;
            Ok(Some(st.dwCurrentState != SERVICE_STOPPED))
        }
        Err(e) if is_missing(&e) => Ok(None),
        Err(e) => Err(e).context("OpenServiceW"),
    }
}

/// Register the service. Returns `Ok(false)` (and logs a warning) if the exe is absent.
pub fn install(exe: &Path) -> Result<bool> {
    if !exe.is_file() {
        tracing::warn!("{} not found; skipping service registration", exe.display());
        return Ok(false);
    }
    let scm = scm(SC_MANAGER_CREATE_SERVICE | SC_MANAGER_CONNECT)?;
    if open(&scm, SERVICE_QUERY_STATUS).is_ok() {
        tracing::info!("service {SERVICE_NAME} already registered");
        return Ok(true);
    }
    let bin = HSTRING::from(format!("\"{}\"", exe.display()));
    let svc = unsafe {
        CreateServiceW(
            scm.0,
            &HSTRING::from(SERVICE_NAME),
            &HSTRING::from(SERVICE_DISPLAY_NAME),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            &bin,
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    }
    .map(Handle)
    .context("CreateServiceW")?;
    let mut desc = HSTRING::from(
        "Supervises the Desk Bridge agent and handles the secure desktop (UAC, lock screen).",
    )
    .as_wide()
    .to_vec();
    desc.push(0);
    let info = SERVICE_DESCRIPTIONW {
        lpDescription: PWSTR(desc.as_mut_ptr()),
    };
    // Description is cosmetic; ignore failures.
    let _ = unsafe {
        ChangeServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some((&raw const info).cast()),
        )
    };
    Ok(true)
}

/// Start the service (already running counts as success). `Ok(false)` if it is not registered.
pub fn start() -> Result<bool> {
    let scm = scm(SC_MANAGER_CONNECT)?;
    match open(&scm, SERVICE_START) {
        Ok(svc) => match unsafe { StartServiceW(svc.0, None) } {
            Ok(()) => Ok(true),
            Err(e) if e.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => Ok(true),
            Err(e) => Err(e).context("StartServiceW"),
        },
        Err(e) if is_missing(&e) => Ok(false),
        Err(e) => Err(e).context("OpenServiceW"),
    }
}

/// Stop the service and wait up to 15 s. `Ok(false)` if it is not registered.
pub fn stop() -> Result<bool> {
    let scm = scm(SC_MANAGER_CONNECT)?;
    match open(&scm, SERVICE_STOP | SERVICE_QUERY_STATUS) {
        Ok(svc) => {
            let mut st = SERVICE_STATUS::default();
            // Failure here usually means "not running"; verify through the status below.
            let _ = unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) };
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                unsafe { QueryServiceStatus(svc.0, &mut st) }.context("QueryServiceStatus")?;
                if st.dwCurrentState == SERVICE_STOPPED {
                    return Ok(true);
                }
                if Instant::now() > deadline {
                    anyhow::bail!("service did not stop within 15 s");
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        Err(e) if is_missing(&e) => Ok(false),
        Err(e) => Err(e).context("OpenServiceW"),
    }
}

/// Stop (best effort) and delete the service. Missing service is not an error.
pub fn uninstall() -> Result<bool> {
    let scm = scm(SC_MANAGER_CONNECT)?;
    if matches!(open(&scm, SERVICE_QUERY_STATUS), Err(ref e) if is_missing(e)) {
        return Ok(false);
    }
    if let Err(e) = stop() {
        tracing::warn!("stopping service before delete: {e:#}");
    }
    let svc = open(&scm, SERVICE_ALL_ACCESS).context("OpenServiceW(ALL_ACCESS)")?;
    unsafe { DeleteService(svc.0) }.context("DeleteService")?;
    Ok(true)
}
