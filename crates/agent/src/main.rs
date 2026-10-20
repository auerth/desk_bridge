//! `deskbridge-agent`: runs the engine, serves the UI pipe.
//!
//! * `deskbridge-agent` – run the agent (single instance per user session)
//! * `deskbridge-agent send <paths…>` – ask the running agent to send files/folders ("Senden an")
//! * `deskbridge-agent --stop` – ask the running agent to quit
//! * `deskbridge-agent --after-pid <pid>` – run the agent once process `pid` has exited (used by
//!   "Agent neu starten": the old instance starts its successor right before it exits)
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
#[cfg(windows)]
fn spawn_successor() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("restart: cannot locate own executable: {e}");
            return;
        }
    };
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

#[cfg(not(windows))]
fn main() {
    eprintln!("deskbridge-agent currently supports Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
fn run_agent() -> i32 {
    use deskbridge_core::{brand, config::Config, engine, logging, paths::AppPaths};

    let paths = match std::env::var("DESKBRIDGE_DATA_ROOT") {
        Ok(root) if !root.is_empty() => AppPaths::under(std::path::Path::new(&root)),
        _ => AppPaths::system(),
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
            platform: winplatform::build(),
            bind,
            enable_discovery: true,
            identity: None,
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
        if !deskbridge_platform_windows::input::is_elevated() {
            handle.warn(
                "elevation",
                "Desk Bridge läuft ohne Administratorrechte: Eingaben in Programme mit erhöhten Rechten (z. B. Task-Manager) sind nicht möglich.",
            );
        }
        let pipe_task = tokio::spawn(pipe::serve(pipe_name, handle.clone()));
        tokio::select! {
            _ = handle.wait_shutdown() => tracing::info!("shutdown requested"),
            _ = tokio::signal::ctrl_c() => tracing::info!("ctrl-c"),
        }
        // Let the pipe deliver the reply to the request that asked for the shutdown.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        handle.stop();
        pipe_task.abort();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if handle.restart_requested() {
            spawn_successor();
        }
        0
    })
}
