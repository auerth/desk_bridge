//! `deskbridge-agent`: runs the engine, serves the UI pipe.
//!
//! * `deskbridge-agent` – run the agent (single instance per user session)
//! * `deskbridge-agent send <paths…>` – ask the running agent to send files/folders ("Senden an")
//! * `deskbridge-agent --stop` – ask the running agent to quit
//!
//! Environment overrides (used by `scripts/dev-two-instances.ps1` and tests):
//! `DESKBRIDGE_DATA_ROOT`, `DESKBRIDGE_PORT`, `DESKBRIDGE_PIPE`.

#![cfg_attr(not(windows), allow(unused))]

#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod winplatform;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("send") => pipe::client_main(pipe::ClientCommand::Send(args[1..].to_vec())),
        Some("--stop" | "stop") => pipe::client_main(pipe::ClientCommand::Stop),
        Some("--version" | "-V") => {
            println!("deskbridge-agent {}", deskbridge_core::brand::VERSION);
            0
        }
        _ => run_agent(),
    };
    std::process::exit(code);
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
    if let Ok(p) = std::env::var("DESKBRIDGE_PORT") {
        if let Ok(p) = p.parse::<u16>() {
            config.network.port = p;
        }
    }
    let _log = logging::init(&paths.logs_dir(), "agent", &config.general.log_level);
    logging::install_panic_hook(&paths.logs_dir());
    deskbridge_platform_windows::input::init_dpi_awareness();
    deskbridge_platform_windows::input::restore_system_cursors();
    if !config.focus.lid_do_nothing {
        let _ = deskbridge_platform_windows::power::restore_lid_action_if_backup_exists();
    }
    tracing::info!(version = brand::VERSION, "agent starting");

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("db-rt").build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("runtime: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let bind = std::net::SocketAddr::from(([0, 0, 0, 0], config.network.port));
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
        handle.stop();
        pipe_task.abort();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        0
    })
}
