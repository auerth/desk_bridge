//! `deskbridge-agent`: runs the engine, serves the UI pipe.
//!
//! * `deskbridge-agent` – run the agent (single instance per user session)
//! * `deskbridge-agent send <paths…>` – ask the running agent to send files/folders ("Senden an")
//! * `deskbridge-agent --stop` – ask the running agent to quit
//! * `deskbridge-agent --after-pid <pid>` – run the agent once process `pid` has exited (used by
//!   "Agent neu starten": the old instance starts its successor right before it exits)
//! * `deskbridge-agent --logon-screen` – the restricted agent at the Windows logon screen; only
//!   the service starts it, as SYSTEM, while nobody is signed in (ADR 0008)
//!
//! Environment overrides (used by `scripts/dev-two-instances.ps1` and tests):
//! `DESKBRIDGE_DATA_ROOT`, `DESKBRIDGE_PORT`, `DESKBRIDGE_PIPE`.

#![cfg_attr(not(windows), allow(unused))]
// GUI subsystem: the agent is a background process and must never show a console window
// (the scheduled task, the service supervisor and the UI all start it). The command line modes
// (`--version`, `send`, `--stop`) attach to the parent console below so output still works.
#![cfg_attr(windows, windows_subsystem = "windows")]
#![allow(non_upper_case_globals)]

/// Exported (see build.rs): selects the discrete GPU on Optimus / PowerXpress laptops.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub static NvOptimusEnablement: u32 = 1;
#[cfg(windows)]
#[unsafe(no_mangle)]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

#[cfg(windows)]
mod logon;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod secure;
#[cfg(windows)]
mod winplatform;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        attach_parent_console();
    }
    let code = match args.first().map(String::as_str) {
        Some("send") => pipe::client_main(pipe::ClientCommand::Send(args[1..].to_vec())),
        Some("--stop" | "stop") => pipe::client_main(pipe::ClientCommand::Stop),
        Some("--version" | "-V") => {
            println!("deskbridge-agent {}", deskbridge_core::brand::VERSION);
            0
        }
        Some(deskbridge_core::brand::LOGON_SCREEN_ARG) => run_logon_agent(),
        Some("--after-pid") => {
            if let Some(pid) = args.get(1).and_then(|p| p.parse::<u32>().ok()) {
                wait_for_exit(pid, std::time::Duration::from_secs(15));
            }
            run_agent()
        }
        _ => run_agent(),
    };
    std::process::exit(code);
}

/// Lets the command line modes print into the terminal they were started from.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: plain Win32 call; failure (no parent console) is fine.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Blocks until process `pid` has exited (or `timeout` elapsed / the process is already gone).
#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
            let _ = WaitForSingleObject(h, ms);
            let _ = CloseHandle(h);
        }
    }
}

/// Starts the successor of this agent (`--after-pid`): it waits until this process has exited
/// and released the single-instance mutex, the UDP port and the pipe. When the service
/// supervises the agent it sees a clean exit (no restart of its own) and adopts the successor.
///
/// `elevated`: start the successor as administrator through the UAC prompt ("Als Administrator
/// neu starten" on the elevation warning). A declined prompt falls back to a plain successor so
/// the agent never stays gone.
#[cfg(windows)]
fn spawn_successor(elevated: bool) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("restart: cannot locate own executable: {e}");
            return;
        }
    };
    if elevated && !deskbridge_platform_windows::input::is_elevated() {
        match spawn_successor_elevated(&exe, std::process::id()) {
            Ok(pid) => {
                tracing::info!(pid, "restart: elevated successor started");
                return;
            }
            Err(e) => tracing::warn!(
                "restart: elevated start failed ({e}); starting the successor with the current rights"
            ),
        }
    } else if elevated {
        tracing::info!("restart: already elevated, plain restart");
    }
    match std::process::Command::new(&exe)
        .arg("--after-pid")
        .arg(std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => tracing::info!(pid = child.id(), "restart: successor started"),
        Err(e) => tracing::error!("restart: cannot start successor: {e}"),
    }
}

/// `ShellExecuteEx("runas")` on our own executable with `--after-pid`. Windows shows the UAC
/// prompt and the call returns once it is answered: `Ok(pid)` of the elevated successor, `Err`
/// when the prompt was declined (`ERROR_CANCELLED`) or the start failed.
#[cfg(windows)]
fn spawn_successor_elevated(exe: &std::path::Path, after_pid: u32) -> Result<u32, String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::GetProcessId;
    use windows::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR};

    let verb = HSTRING::from("runas");
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(format!("--after-pid {after_pid}"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: the strings outlive the call; the returned process handle is closed below.
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| e.to_string())?;
        if info.hProcess.is_invalid() {
            return Ok(0);
        }
        let pid = GetProcessId(info.hProcess);
        let _ = CloseHandle(info.hProcess);
        Ok(pid)
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("deskbridge-agent currently supports Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
fn run_agent() -> i32 {
    use deskbridge_core::{brand, config::Config, logging, paths::AppPaths};

    let custom_root = std::env::var("DESKBRIDGE_DATA_ROOT")
        .ok()
        .filter(|r| !r.is_empty());
    let paths = match &custom_root {
        Some(root) => AppPaths::under(std::path::Path::new(root)),
        None => AppPaths::system(),
    };
    if let Err(e) = paths.ensure() {
        eprintln!("cannot create data directories: {e}");
        return 1;
    }
    let pipe_name = pipe::pipe_name();

    let Some(_instance) = pipe::SingleInstance::acquire(&pipe_name) else {
        eprintln!("{} agent is already running", brand::PRODUCT_NAME);
        return 0;
    };

    let mut config = match Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config unreadable ({e}), starting with defaults");
            Config::default()
        }
    };
    if let Ok(p) = std::env::var("DESKBRIDGE_PORT")
        && let Ok(p) = p.parse::<u16>()
    {
        config.network.port = p;
    }
    let _log = logging::init(&paths.logs_dir(), "agent", &config.general.log_level);
    logging::install_panic_hook(&paths.logs_dir());
    deskbridge_platform_windows::input::init_dpi_awareness();
    deskbridge_platform_windows::input::restore_system_cursors();
    if !config.focus.lid_do_nothing {
        let _ = deskbridge_platform_windows::power::restore_lid_action_if_backup_exists();
    }
    tracing::info!(version = brand::VERSION, "agent starting");
    if !config.device.input_source {
        // Target device: a crashed predecessor may have left injected keys pressed.
        secure::release_stuck_keys();
    }
    // Dev instances (`DESKBRIDGE_DATA_ROOT`) never touch the machine-wide logon profile.
    let logon_sync = custom_root.is_none().then(|| paths.clone());
    run_engine(EngineRun {
        paths,
        config,
        platform: winplatform::build(),
        ui_pipe: Some(pipe_name),
        logon_screen: false,
        logon_sync,
    })
}

/// The agent at the logon screen (ADR 0008): started by the service as SYSTEM on
/// `winsta0\Winlogon` while nobody is signed in, ended by it at the sign-in. Runs on the copy of
/// identity, peers and config the user's agent keeps in `%ProgramData%`, as an input target only.
/// Exit code 0 (no profile, untrusted profile, switched off, already running) tells the service
/// not to try again until the console session changes.
#[cfg(windows)]
fn run_logon_agent() -> i32 {
    use deskbridge_core::{brand, config::Config, logging};

    let paths = logon::paths();
    let logs = logon::logs_dir();
    let _ = std::fs::create_dir_all(&logs);
    let _log = logging::init(&logs, "logon-agent", "info");
    logging::install_panic_hook(&logs);
    tracing::info!(version = brand::VERSION, "logon-screen agent starting");

    if !logon::profile_present(&logon::root()) {
        tracing::info!("no logon-screen profile (the user's agent has not written one yet)");
        return 0;
    }
    if let Err(e) = logon::verify_trusted(&paths, &logon::win::owner_is_trusted) {
        tracing::error!("logon-screen profile rejected: {e}");
        return 0;
    }
    let Some(_instance) = pipe::SingleInstance::acquire_name(brand::LOGON_AGENT_INSTANCE_ID) else {
        tracing::info!("logon-screen agent already running");
        return 0;
    };
    let config = match Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("logon-screen config unreadable: {e}");
            return 0;
        }
    };
    if !config.general.logon_screen {
        tracing::info!("control at the logon screen is switched off");
        return 0;
    }
    deskbridge_platform_windows::input::init_dpi_awareness();
    secure::release_stuck_keys();
    run_engine(EngineRun {
        paths,
        config,
        platform: winplatform::build_logon(),
        ui_pipe: None,
        logon_screen: true,
        logon_sync: None,
    })
}

#[cfg(windows)]
struct EngineRun {
    paths: deskbridge_core::paths::AppPaths,
    config: deskbridge_core::config::Config,
    platform: deskbridge_core::platform::Platform,
    /// Serve the UI on this pipe (`None` at the logon screen: there is no UI).
    ui_pipe: Option<String>,
    logon_screen: bool,
    /// Keep the logon-screen copy of this profile current.
    logon_sync: Option<deskbridge_core::paths::AppPaths>,
}

/// Runs the engine until it is asked to stop.
#[cfg(windows)]
fn run_engine(run: EngineRun) -> i32 {
    use deskbridge_core::engine;

    let EngineRun {
        paths,
        config,
        platform,
        ui_pipe,
        logon_screen,
        logon_sync,
    } = run;
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("db-rt")
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("runtime: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let bind_ip: std::net::IpAddr = config
            .network
            .bind_address
            .trim()
            .parse()
            .unwrap_or(std::net::IpAddr::from([0, 0, 0, 0]));
        let bind = std::net::SocketAddr::new(bind_ip, config.network.port);
        let handle = match engine::start(engine::EngineParams {
            paths,
            config,
            platform,
            bind,
            enable_discovery: true,
            identity: None,
            logon_screen,
        })
        .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("engine failed to start: {e:#}");
                eprintln!("engine failed to start: {e:#}");
                return 1;
            }
        };
        if !logon_screen && !deskbridge_platform_windows::input::is_elevated() {
            handle.warn(
                "elevation",
                "Desk Bridge läuft ohne Administratorrechte: Eingaben in Programme mit erhöhten Rechten (z. B. Task-Manager) sind nicht möglich.",
            );
        }
        let pipe_task = ui_pipe.map(|name| tokio::spawn(pipe::serve(name, handle.clone())));
        let sync_task = logon_sync.map(|p| tokio::spawn(logon::sync_task(p)));
        let signed_in = async {
            if logon_screen {
                logon::wait_for_sign_in().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            _ = handle.wait_shutdown() => tracing::info!("shutdown requested"),
            _ = tokio::signal::ctrl_c() => tracing::info!("ctrl-c"),
            () = signed_in => {}
        }
        // Let the pipe deliver the reply to the request that asked for the shutdown.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        handle.stop();
        for t in pipe_task.into_iter().chain(sync_task) {
            t.abort();
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if handle.restart_requested() {
            spawn_successor(handle.restart_elevated_requested());
        }
        0
    })
}
