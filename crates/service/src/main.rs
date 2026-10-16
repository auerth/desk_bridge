//! `deskbridge-service.exe`: Windows service, secure-desktop helper and console test mode.

#[cfg(windows)]
fn main() {
    std::process::exit(app::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("deskbridge-service supports Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
mod app {
    use deskbridge_service::cli::{self, ConsoleOptions, Mode};
    use deskbridge_service::control::{DisabledSas, SasSender, SystemSas};
    use deskbridge_service::runtime::{Runtime, RuntimeOptions};
    use deskbridge_service::{helper_proc, logging, winsvc, winutil};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    /// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`: started from a console, not by the SCM.
    const NOT_UNDER_SCM: i32 = 1063;

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match cli::parse(&args) {
            Ok(Mode::Service) => service(),
            Ok(Mode::Helper(rest)) => helper(&rest),
            Ok(Mode::Console(opts)) => console(opts),
            Ok(Mode::Version) => {
                println!("deskbridge-service {}", env!("CARGO_PKG_VERSION"));
                0
            }
            Ok(Mode::Help) => {
                println!("{}", cli::USAGE);
                0
            }
            Err(e) => {
                eprintln!("{e}\n\n{}", cli::USAGE);
                2
            }
        }
    }

    fn service() -> i32 {
        match winsvc::run_dispatcher() {
            Ok(()) => 0,
            Err(e) if (e.code().0 & 0xFFFF) == NOT_UNDER_SCM => {
                eprintln!(
                    "This program is a Windows service and is started by the Service Control \
                     Manager.\n\n{}",
                    cli::USAGE
                );
                1
            }
            Err(e) => {
                eprintln!("service dispatcher failed: {e}");
                1
            }
        }
    }

    fn helper(rest: &[String]) -> i32 {
        let args = match helper_proc::parse_args(rest) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        };
        let _log = logging::init("service-helper", true, false);
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            "secure-desktop helper starting"
        );
        let r = std::panic::catch_unwind(|| helper_proc::run(&args));
        r.unwrap_or_else(|_| {
            tracing::error!("helper panicked");
            1
        })
    }

    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn on_ctrl(_kind: u32) -> BOOL {
        STOP.store(true, Ordering::SeqCst);
        BOOL(1)
    }

    fn console(opts: ConsoleOptions) -> i32 {
        let _log = logging::init("service-console", false, true);
        tracing::info!("console mode (Ctrl+C stops)");
        if winutil::current_user_sid().as_deref() != Some("S-1-5-18") {
            tracing::warn!(
                "not running as SYSTEM: starting the agent/helper in the user session will fail \
                 (WTSQueryUserToken needs SeTcbPrivilege). Use `psexec -s`, or --no-agent --no-helper."
            );
        }
        winsvc::warn_if_sas_not_configured();
        let sas: Arc<dyn SasSender> = if opts.no_sas {
            Arc::new(DisabledSas)
        } else {
            Arc::new(SystemSas)
        };
        let mut ro = RuntimeOptions::new(sas);
        ro.manage_agent = !opts.no_agent;
        ro.manage_helper = !opts.no_helper;
        let mut rt = match Runtime::start(ro) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("cannot start: {e}");
                return 1;
            }
        };
        // SAFETY: the handler is a plain `extern "system"` function touching only an atomic.
        unsafe {
            let _ = SetConsoleCtrlHandler(Some(on_ctrl), true);
        }
        while !STOP.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        tracing::info!("stopping");
        rt.stop();
        0
    }
}
