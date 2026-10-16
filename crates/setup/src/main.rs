//! `deskbridge-setup.exe`: installer helper for Desk Bridge (plan 12.2).
//!
//! Run elevated. Every subcommand is idempotent; `--dry-run` prints the actions without changing
//! anything; `status` and `check-network` are read-only.

#![cfg_attr(not(windows), allow(dead_code))]

mod brand;
mod cli;
mod driver;
mod firewall;
mod inf;
mod lid;
mod logging;
mod manifest;
mod paths;
mod taskxml;

#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod elevation;
#[cfg(windows)]
mod network;
#[cfg(windows)]
mod sas_policy;
mod service;
#[cfg(windows)]
mod shortcut;
#[cfg(windows)]
mod task;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use clap::Parser;

    let cli = cli::Cli::parse();
    let log_file = logging::init(cli.quiet);
    match run(&cli) {
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            tracing::error!("{e:#}");
            if !cli.quiet
                && let Some(f) = log_file
            {
                eprintln!("(details: {})", f.display());
            }
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run(cli: &cli::Cli) -> anyhow::Result<i32> {
    use cli::Command;

    // Elevation: re-exec through UAC unless quiet (the installer is already elevated then).
    if cli.command.needs_elevation(cli.dry_run) && !elevation::is_elevated() {
        if cli.quiet {
            anyhow::bail!(
                "administrator rights are required: run deskbridge-setup.exe from an elevated prompt"
            );
        }
        eprintln!("Administrator rights are required; asking Windows for elevation...");
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        return elevation::relaunch_elevated(&args);
    }

    let _com = ComGuard::new()?;
    let ctx = commands::Ctx {
        install_dir: paths::resolve_install_dir(cli.install_dir.as_deref())?,
        dry_run: cli.dry_run,
    };
    tracing::debug!(?cli, "start");

    match &cli.command {
        Command::Install(a) => commands::install(&ctx, a),
        Command::Uninstall(a) => commands::uninstall(&ctx, a),
        Command::Status => print_json(&commands::status(&ctx)?),
        Command::CheckNetwork => print_json(&commands::check_network()?),
        Command::MakePrivate => print_json(&commands::make_private(&ctx)?),
        Command::Driver(d) => commands::driver_cmd(&ctx, d),
        Command::Service(s) => commands::service_cmd(&ctx, s),
    }
}

#[cfg(windows)]
fn print_json(v: &serde_json::Value) -> anyhow::Result<i32> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(0)
}

/// Initialises COM (multithreaded apartment) for the lifetime of the command.
#[cfg(windows)]
struct ComGuard;

#[cfg(windows)]
impl ComGuard {
    fn new() -> anyhow::Result<Self> {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
        // SAFETY: called once on the main thread before any COM use.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| anyhow::anyhow!("CoInitializeEx failed: {e}"))?;
        Ok(Self)
    }
}

#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("deskbridge-setup only runs on Windows");
    std::process::ExitCode::from(1)
}
