//! Command line definition (kept platform independent so it is unit-testable everywhere).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Desk Bridge installer helper. Run elevated; every subcommand is idempotent.
#[derive(Debug, Parser)]
#[command(name = "deskbridge-setup", version, about, long_about = None)]
pub struct Cli {
    /// Directory containing deskbridge-agent.exe (default: the directory of this executable).
    #[arg(long, global = true, value_name = "PATH")]
    pub install_dir: Option<PathBuf>,

    /// No prompts, no console chatter (used by the NSIS installer). Errors still go to the log.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Print what would be done, change nothing.
    #[arg(long, global = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Firewall rule, scheduled task, SendTo shortcut and (optionally) drivers.
    Install(InstallArgs),
    /// Remove everything `install` created (reads installed.json), restore the lid action.
    Uninstall(UninstallArgs),
    /// Print what is installed as JSON (read-only, no elevation needed).
    Status,
    /// Report the category (Public/Private/Domain) of the connected networks as JSON.
    CheckNetwork,
    /// Switch the connected Public network(s) to Private (system setting; ask the user first).
    MakePrivate,
    /// Driver store / root-enumerated device management.
    #[command(subcommand)]
    Driver(DriverCommand),
    /// Windows service management for deskbridge-service.exe (built in M6).
    #[command(subcommand)]
    Service(ServiceCommand),
}

#[derive(Debug, Args, Default)]
pub struct InstallArgs {
    /// Also open the Domain firewall profile (default: Private only, plan 7.5). Use on PCs
    /// whose network is classified as a domain network.
    #[arg(long)]
    pub include_domain: bool,
    /// Do not create the firewall rule.
    #[arg(long)]
    pub no_firewall: bool,
    /// Do not register the scheduled task.
    #[arg(long)]
    pub no_task: bool,
    /// Do not create the SendTo shortcut.
    #[arg(long)]
    pub no_sendto: bool,
    /// Do not install the driver packages listed in drivers/drivers.json.
    #[arg(long)]
    pub skip_drivers: bool,
    /// Do not start the agent task after registering it.
    #[arg(long)]
    pub no_start: bool,
    /// Also register deskbridge-service.exe as a Windows service (M6).
    #[arg(long)]
    pub with_service: bool,
    /// Run the task at logon of this user (DOMAIN\user) instead of any user.
    #[arg(long, value_name = "DOMAIN\\USER")]
    pub task_user: Option<String>,
}

#[derive(Debug, Args, Default)]
pub struct UninstallArgs {
    /// Also delete %APPDATA% / %LOCALAPPDATA% data (identity, peers, config, logs).
    #[arg(long)]
    pub purge_user_data: bool,
}

#[derive(Debug, Subcommand)]
pub enum DriverCommand {
    /// Stage a driver package and create its root-enumerated device (once).
    Install {
        /// Path to the .inf file inside the extracted driver package.
        #[arg(long)]
        inf: PathBuf,
        /// Hardware ID of the device to create, e.g. Root\DeskBridgeDisplay.
        #[arg(long)]
        hwid: String,
        /// Friendly name used for logging and the device description.
        #[arg(long)]
        name: String,
    },
    /// Remove the device(s) with this hardware ID and delete the driver package.
    Uninstall {
        #[arg(long)]
        hwid: String,
    },
    /// List the hardware IDs declared by an INF (offline, no changes).
    Inspect {
        #[arg(long)]
        inf: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Register the service (skipped with a warning if deskbridge-service.exe is absent).
    Install,
    /// Stop and delete the service (no-op if it does not exist).
    Uninstall,
    /// Start the service.
    Start,
    /// Stop the service.
    Stop,
}

impl Command {
    /// Does this command change system state and therefore need an elevated token?
    #[must_use]
    pub fn needs_elevation(&self, dry_run: bool) -> bool {
        if dry_run {
            return false;
        }
        match self {
            Self::Install(_) | Self::Uninstall(_) | Self::MakePrivate => true,
            Self::Driver(DriverCommand::Inspect { .. }) => false,
            Self::Driver(_) | Self::Service(_) => true,
            Self::Status | Self::CheckNetwork => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let mut v = vec!["deskbridge-setup"];
        v.extend_from_slice(args);
        Cli::try_parse_from(v).expect("parse")
    }

    #[test]
    fn install_quiet_dry_run() {
        let cli = parse(&[
            "install",
            "--quiet",
            "--dry-run",
            "--install-dir",
            r"C:\x y",
        ]);
        assert!(cli.quiet && cli.dry_run);
        assert_eq!(
            cli.install_dir.as_deref(),
            Some(std::path::Path::new(r"C:\x y"))
        );
        assert!(matches!(cli.command, Command::Install(_)));
        assert!(!cli.command.needs_elevation(cli.dry_run));
    }

    #[test]
    fn global_flags_after_subcommand() {
        let cli = parse(&["uninstall", "--purge-user-data", "-q"]);
        assert!(cli.quiet);
        match cli.command {
            Command::Uninstall(a) => assert!(a.purge_user_data),
            _ => panic!(),
        }
    }

    #[test]
    fn driver_install_requires_all_args() {
        assert!(Cli::try_parse_from(["x", "driver", "install", "--inf", "a.inf"]).is_err());
        let cli = parse(&[
            "driver",
            "install",
            "--inf",
            "a.inf",
            "--hwid",
            r"Root\Foo",
            "--name",
            "Foo",
        ]);
        match cli.command {
            Command::Driver(DriverCommand::Install { hwid, name, .. }) => {
                assert_eq!(hwid, r"Root\Foo");
                assert_eq!(name, "Foo");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn elevation_requirements() {
        assert!(parse(&["install"]).command.needs_elevation(false));
        assert!(!parse(&["status"]).command.needs_elevation(false));
        assert!(!parse(&["check-network"]).command.needs_elevation(false));
        assert!(parse(&["make-private"]).command.needs_elevation(false));
        assert!(parse(&["service", "stop"]).command.needs_elevation(false));
        assert!(
            !parse(&["driver", "inspect", "--inf", "a.inf"])
                .command
                .needs_elevation(false)
        );
        assert!(!parse(&["uninstall"]).command.needs_elevation(true));
    }

    #[test]
    fn unknown_subcommand_rejected() {
        assert!(Cli::try_parse_from(["x", "frobnicate"]).is_err());
    }
}
