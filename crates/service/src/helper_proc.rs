//! Entry point of `deskbridge-service.exe --helper`.

use crate::helper::{HelperHandler, SystemDesktopFollower, run_worker};
use crate::pipe_security::{PipeAccess, build_sddl, is_valid_sid_string};
use crate::pipe_server::{self, AccessFn};
use crate::protocol::HELPER_PIPE;
use crate::secure_capture::SecureCaptureHost;
use deskbridge_platform_windows::input::WinInputInjector;
use std::sync::Arc;
use tokio::sync::{Notify, watch};
use windows::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// Command-line options of the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    /// SID of the interactive user allowed to open the pipe (besides SYSTEM).
    pub user_sid: Option<String>,
    /// Exit when this process (the service) exits, so a stopped service never leaves a SYSTEM
    /// helper behind.
    pub parent_pid: Option<u32>,
    /// Pipe name; only tests and `--console` experiments override the default.
    pub pipe_name: String,
}

impl Default for HelperArgs {
    fn default() -> Self {
        Self {
            user_sid: None,
            parent_pid: None,
            pipe_name: HELPER_PIPE.to_string(),
        }
    }
}

/// Parses everything after `--helper`. Unknown flags are errors.
pub fn parse_args(args: &[String]) -> Result<HelperArgs, String> {
    let mut out = HelperArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--user-sid" => {
                let v = it.next().ok_or("--user-sid needs a value")?;
                if !is_valid_sid_string(v) {
                    return Err(format!("invalid SID '{v}'"));
                }
                out.user_sid = Some(v.clone());
            }
            "--parent-pid" => {
                let v = it.next().ok_or("--parent-pid needs a value")?;
                out.parent_pid = Some(v.parse().map_err(|_| format!("invalid pid '{v}'"))?);
            }
            "--pipe" => out.pipe_name = it.next().ok_or("--pipe needs a value")?.clone(),
            other => return Err(format!("unknown helper argument '{other}'")),
        }
    }
    Ok(out)
}

/// Runs the helper until the parent exits. Returns the process exit code.
pub fn run(args: &HelperArgs) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("dbh-rt")
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("helper: tokio runtime: {e}");
            return 1;
        }
    };

    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("dbh-inject".into())
        .spawn(move || {
            // Created on the worker thread so the injector and the desktop attachment share it.
            let injector = WinInputInjector::new();
            let mut follower = SystemDesktopFollower::new();
            run_worker(&cmd_rx, &injector, &mut follower);
        });
    let Ok(worker) = worker else {
        tracing::error!("helper: cannot start the injection thread");
        return 1;
    };

    let (stop_tx, stop_rx) = watch::channel(false);
    if let Some(pid) = args.parent_pid {
        let stop_tx = stop_tx.clone();
        let _ = std::thread::Builder::new()
            .name("dbh-parent".into())
            .spawn(move || {
                wait_for_process(pid);
                tracing::info!("parent process {pid} is gone, helper exits");
                let _ = stop_tx.send(true);
            });
    }

    let user_sid = args.user_sid.clone();
    let access: AccessFn = Arc::new(move || PipeAccess::production(user_sid.clone()));
    // The shared textures carry a picture of the secure desktop: SYSTEM and the interactive user
    // only, like the pipe (without its integrity label, which does not apply to texture handles).
    let texture_sddl = build_sddl(&PipeAccess {
        user_sid: args.user_sid.clone(),
        require_high_integrity: false,
    });
    let capture = Arc::new(SecureCaptureHost::new(texture_sddl));
    let handler = Arc::new(HelperHandler::with_capture(cmd_tx, capture));
    tracing::info!(pipe = %args.pipe_name, user = ?args.user_sid, "helper serving");
    rt.block_on(pipe_server::serve(
        args.pipe_name.clone(),
        access,
        handler.clone(),
        stop_rx,
        Arc::new(Notify::new()),
    ));
    // Dropping the runtime cancels connection tasks (their guards queue a final ReleaseAll);
    // dropping the last sender ends the worker, whose injector releases everything on drop.
    drop(rt);
    drop(handler);
    let _ = worker.join();
    0
}

/// Blocks until process `pid` exits (or cannot be opened, which counts as gone).
fn wait_for_process(pid: u32) {
    // SAFETY: plain OpenProcess/Wait calls; the handle is closed after the wait.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return;
        };
        WaitForSingleObject(h, INFINITE);
        let _ = windows::Win32::Foundation::CloseHandle(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parses_helper_arguments() {
        let a = parse_args(&s(&[
            "--user-sid",
            "S-1-5-21-1-2-3-1000",
            "--parent-pid",
            "812",
        ]))
        .unwrap();
        assert_eq!(a.user_sid.as_deref(), Some("S-1-5-21-1-2-3-1000"));
        assert_eq!(a.parent_pid, Some(812));
        assert_eq!(a.pipe_name, HELPER_PIPE);
        assert_eq!(parse_args(&[]).unwrap(), HelperArgs::default());
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse_args(&s(&["--user-sid", "S-1-5-18)(A;;GA;;;WD"])).is_err());
        assert!(parse_args(&s(&["--user-sid"])).is_err());
        assert!(parse_args(&s(&["--parent-pid", "x"])).is_err());
        assert!(parse_args(&s(&["--bogus"])).is_err());
    }
}
