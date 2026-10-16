//! Raw Service Control Manager glue (no extra crates): dispatcher, control handler, status.
//!
//! Every callback is wrapped in `catch_unwind`; the control handler only pushes into a channel so
//! it returns immediately, as the SCM requires.

use crate::control::{SasSender, SystemSas, policy_allows_services, read_sas_policy};
use crate::logging;
use crate::runtime::{Runtime, RuntimeOptions};
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use windows::Win32::System::RemoteDesktop::WTSSESSION_NOTIFICATION;
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows::core::PWSTR;

/// Must match `crates/setup/src/brand.rs` (checked by a test).
pub const SERVICE_NAME: &str = "DeskBridgeService";
pub const SERVICE_DISPLAY_NAME: &str = "Desk Bridge Service";

/// `ERROR_CALL_NOT_IMPLEMENTED`
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;

enum SvcEvent {
    Stop,
    Session(u32, u32),
}

static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);
static EVENTS: Mutex<Option<Sender<SvcEvent>>> = Mutex::new(None);

fn send_event(ev: SvcEvent) {
    if let Ok(g) = EVENTS.lock() {
        if let Some(tx) = g.as_ref() {
            let _ = tx.send(ev);
        }
    }
}

fn set_status(state: SERVICE_STATUS_CURRENT_STATE, accepted: u32, exit_code: u32, wait_hint_ms: u32) {
    let h = STATUS_HANDLE.load(Ordering::Acquire);
    if h == 0 {
        return;
    }
    static CHECKPOINT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let pending = state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING;
    let st = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: exit_code,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: if pending { CHECKPOINT.fetch_add(1, Ordering::Relaxed) + 1 } else { 0 },
        dwWaitHint: wait_hint_ms,
    };
    // SAFETY: valid status handle obtained from RegisterServiceCtrlHandlerExW; `st` is initialised.
    unsafe {
        let _ = SetServiceStatus(SERVICE_STATUS_HANDLE(h as *mut c_void), &st);
    }
}

unsafe extern "system" fn ctrl_handler(
    control: u32,
    event_type: u32,
    event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    let r = std::panic::catch_unwind(|| match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            send_event(SvcEvent::Stop);
            0
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            let session = if event_data.is_null() {
                0
            } else {
                // SAFETY: for SESSIONCHANGE the SCM passes a WTSSESSION_NOTIFICATION.
                unsafe { (*event_data.cast::<WTSSESSION_NOTIFICATION>()).dwSessionId }
            };
            send_event(SvcEvent::Session(event_type, session));
            0
        }
        SERVICE_CONTROL_INTERROGATE => 0,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    });
    r.unwrap_or(ERROR_CALL_NOT_IMPLEMENTED)
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    if std::panic::catch_unwind(service_main_impl).is_err() {
        set_status(SERVICE_STOPPED, 0, 1, 0);
    }
}

fn service_main_impl() {
    let (tx, rx) = channel();
    if let Ok(mut g) = EVENTS.lock() {
        *g = Some(tx);
    }
    let name = crate::winutil::wide(SERVICE_NAME);
    // SAFETY: `name` is NUL-terminated; the handler is a valid `extern "system"` function.
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(windows::core::PCWSTR(name.as_ptr()), Some(ctrl_handler), None)
    };
    let Ok(handle) = handle else { return };
    STATUS_HANDLE.store(handle.0 as isize, Ordering::Release);
    set_status(SERVICE_START_PENDING, 0, 0, 10_000);

    let _log = logging::init("service", true, false);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Desk Bridge service starting");
    warn_if_sas_not_configured();

    let sas: Arc<dyn SasSender> = Arc::new(SystemSas);
    let mut runtime = match Runtime::start(RuntimeOptions::new(sas)) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("cannot start: {e}");
            set_status(SERVICE_STOPPED, 0, 1, 0);
            return;
        }
    };
    set_status(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE,
        0,
        0,
    );
    tracing::info!("service running");
    loop {
        match rx.recv() {
            Ok(SvcEvent::Session(ev, sid)) => runtime.session_change(ev, sid),
            Ok(SvcEvent::Stop) | Err(_) => break,
        }
    }
    tracing::info!("service stopping");
    set_status(SERVICE_STOP_PENDING, 0, 0, 10_000);
    runtime.stop();
    tracing::info!("service stopped");
    set_status(SERVICE_STOPPED, 0, 0, 0);
}

/// Logs how to enable Ctrl+Alt+Del when the policy is missing.
pub fn warn_if_sas_not_configured() {
    let v = read_sas_policy();
    if !policy_allows_services(v) {
        tracing::warn!(
            policy = ?v,
            "SoftwareSASGeneration is not 1 or 3: Ctrl+Alt+Del (SendSAS) requests will be refused. \
             Enable it with: reg add HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\System \
             /v SoftwareSASGeneration /t REG_DWORD /d 1 /f"
        );
    }
}

/// Connects to the SCM and blocks until the service stops. Fails with
/// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` (1063) when started from a console.
pub fn run_dispatcher() -> windows::core::Result<()> {
    let mut name = crate::winutil::wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(name.as_mut_ptr()), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR::null(), lpServiceProc: None },
    ];
    // SAFETY: the table is NULL-terminated and outlives the call, which blocks until the service ends.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_setup_brand() {
        let setup = include_str!("../../setup/src/brand.rs");
        for (name, value) in [("SERVICE_NAME", SERVICE_NAME), ("SERVICE_DISPLAY_NAME", SERVICE_DISPLAY_NAME)] {
            let line = setup
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("pub const {name}:")))
                .unwrap_or_else(|| panic!("{name} missing in setup brand.rs"));
            assert!(line.contains(&format!("\"{value}\"")), "{name} drifted: {line}");
        }
    }
}
