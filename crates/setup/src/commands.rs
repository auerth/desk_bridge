//! Orchestration of the subcommands. System calls live in the sibling modules; this file only
//! decides *what* to do and honours `--dry-run`.

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::brand::{AGENT_EXE, AGENT_TASK_NAME, FIREWALL_RULE_NAME, SERVICE_EXE, SERVICE_NAME};
use crate::cli::{DriverCommand, InstallArgs, ServiceCommand, UninstallArgs};
use crate::driver::{self, DriverSpec};
use crate::inf::{decode_inf, parse_inf};
use crate::lid::{LidBackup, parse_active_scheme};
use crate::manifest::{DriverRecord, Manifest};
use crate::taskxml::TaskPrincipal;
use crate::{firewall, network, paths, service, shortcut, task};

/// Exit code when some (not all) steps failed.
pub const EXIT_PARTIAL: i32 = 2;

pub struct Ctx {
    pub install_dir: PathBuf,
    pub dry_run: bool,
}

impl Ctx {
    /// Announce a planned action. Always printed in dry-run mode, otherwise only logged.
    fn plan(&self, msg: &str) {
        if self.dry_run {
            println!("[dry-run] {msg}");
        } else {
            tracing::info!("{msg}");
        }
    }

    fn agent_exe(&self) -> PathBuf {
        paths::agent_exe(&self.install_dir)
    }
}

/// Run one step; failures are logged and collected so the remaining steps still run.
fn step<T>(failures: &mut Vec<String>, what: &str, f: impl FnOnce() -> Result<T>) -> Option<T> {
    match f() {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::error!("{what}: {e:#}");
            failures.push(format!("{what}: {e:#}"));
            None
        }
    }
}

fn finish(failures: &[String]) -> Result<i32> {
    if failures.is_empty() {
        Ok(0)
    } else {
        tracing::error!("{} step(s) failed:", failures.len());
        for f in failures {
            tracing::error!("  - {f}");
        }
        Ok(EXIT_PARTIAL)
    }
}

// ---------------------------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------------------------

pub fn install(ctx: &Ctx, args: &InstallArgs) -> Result<i32> {
    let dir = &ctx.install_dir;
    let agent = ctx.agent_exe();
    ctx.plan(&format!("install dir: {}", dir.display()));
    if !agent.is_file() {
        tracing::warn!(
            "{} not found (continuing; the task will start it once it exists)",
            agent.display()
        );
    }

    let mut manifest = Manifest::load(dir)?.unwrap_or_else(|| Manifest::new(dir));
    manifest.version = env!("CARGO_PKG_VERSION").to_owned();
    manifest.install_dir = dir.display().to_string();
    let mut failures = Vec::new();

    // (a) firewall rule
    if !args.no_firewall {
        ctx.plan(&format!(
            "create firewall rule {FIREWALL_RULE_NAME:?}: inbound UDP {}, {} profile, program {}",
            firewall::local_ports(),
            if args.include_domain {
                "Private+Domain"
            } else {
                "Private"
            },
            agent.display()
        ));
        if !ctx.dry_run
            && step(&mut failures, "firewall rule", || {
                firewall::add_rule(&agent, args.include_domain)
            })
            .is_some()
        {
            manifest.firewall_rule = Some(FIREWALL_RULE_NAME.to_owned());
        }
    }

    // (b) scheduled task
    let mut task_ok = false;
    if !args.no_task {
        let principal = args
            .task_user
            .clone()
            .map_or(TaskPrincipal::AnyUser, TaskPrincipal::User);
        ctx.plan(&format!(
            "register scheduled task {AGENT_TASK_NAME:?}: at logon of {}, highest privileges, runs {}",
            match &principal {
                TaskPrincipal::AnyUser => "any user".to_owned(),
                TaskPrincipal::User(u) => u.clone(),
            },
            agent.display()
        ));
        if !ctx.dry_run
            && step(&mut failures, "scheduled task", || {
                task::register(&agent, dir, &principal)
            })
            .is_some()
        {
            manifest.scheduled_task = Some(AGENT_TASK_NAME.to_owned());
            task_ok = true;
        }
    }

    // (c) SendTo shortcut
    if !args.no_sendto {
        match paths::sendto_dir() {
            Some(sendto) => {
                let lnk = shortcut::shortcut_path(&sendto);
                ctx.plan(&format!(
                    "create SendTo shortcut {} -> {} send",
                    lnk.display(),
                    agent.display()
                ));
                if !ctx.dry_run {
                    if let Some(p) = step(&mut failures, "SendTo shortcut", || {
                        shortcut::create(&sendto, &agent)
                    }) {
                        manifest.send_to_shortcut = Some(p.display().to_string());
                    }
                }
            }
            None => tracing::warn!("%APPDATA% is not set; skipping the SendTo shortcut"),
        }
    }

    // (d) drivers from the shipped catalogue
    if !args.skip_drivers {
        match driver::load_catalogue(dir) {
            Ok(specs) if specs.is_empty() => {
                tracing::info!("no driver catalogue; skipping drivers")
            }
            Ok(specs) => {
                for spec in specs {
                    ctx.plan(&format!(
                        "install driver {:?}: {} (hwid {})",
                        spec.name,
                        spec.inf.display(),
                        spec.hwid
                    ));
                    let what = format!("driver {}", spec.name);
                    if let Some(out) = step(&mut failures, &what, || {
                        driver::copy_files(&spec, ctx.dry_run)?;
                        driver::install(&spec, ctx.dry_run)
                    }) {
                        report_driver_install(ctx, &spec, &out);
                        if !ctx.dry_run {
                            manifest.upsert_driver(DriverRecord {
                                name: spec.name.clone(),
                                inf: spec.inf.display().to_string(),
                                hwid: spec.hwid.clone(),
                                files: spec
                                    .files
                                    .iter()
                                    .map(|f| f.to.display().to_string())
                                    .collect(),
                            });
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("driver catalogue: {e:#}");
                failures.push(format!("driver catalogue: {e:#}"));
            }
        }
    }

    // (e) service (M6): secure desktop helper, Ctrl+Alt+Del, crash watchdog
    if !args.no_service {
        let exe = dir.join(SERVICE_EXE);
        ctx.plan(&format!(
            "register service {SERVICE_NAME} -> {} (skipped if absent)",
            exe.display()
        ));
        if !ctx.dry_run
            && step(&mut failures, "service", || service::install(&exe)).unwrap_or(false)
        {
            manifest.service = Some(SERVICE_NAME.to_owned());
        }
    }

    // (f) allow the service to generate Ctrl+Alt+Del (only if the admin has not set the policy)
    if !args.no_service && !args.no_sas_policy {
        ctx.plan("set policy SoftwareSASGeneration=1 if not configured (Ctrl+Alt+End support)");
        if !ctx.dry_run {
            if let Some(true) = step(&mut failures, "SAS policy", crate::sas_policy::enable_if_unset) {
                manifest.sas_policy = true;
            }
        }
    }

    if ctx.dry_run {
        ctx.plan(&format!("write {}", Manifest::path(dir).display()));
        if !args.no_task && !args.no_start {
            ctx.plan(&format!("start task {AGENT_TASK_NAME:?} now"));
        }
        return Ok(0);
    }

    step(&mut failures, "writing installed.json", || {
        manifest.save(dir)
    });

    if task_ok && !args.no_start {
        // Not fatal: the task also starts at the next logon.
        if let Err(e) = task::run_now() {
            tracing::warn!("could not start the agent task now: {e:#}");
        }
    }
    finish(&failures)
}

fn report_driver_install(ctx: &Ctx, spec: &DriverSpec, out: &driver::InstallOutcome) {
    let state = if out.device_existed {
        "device exists (not created again)"
    } else {
        "device will be created"
    };
    if ctx.dry_run {
        println!("[dry-run]   {}: {state}", spec.name);
    } else {
        tracing::info!(
            "driver {}: created={}, existed={}, reboot_required={}",
            spec.name,
            out.device_created,
            out.device_existed,
            out.reboot_required
        );
        if out.reboot_required {
            tracing::warn!("driver {} needs a reboot to finish", spec.name);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// uninstall
// ---------------------------------------------------------------------------------------------

pub fn uninstall(ctx: &Ctx, args: &UninstallArgs) -> Result<i32> {
    let dir = &ctx.install_dir;
    let manifest = Manifest::load(dir)?;
    if manifest.is_none() {
        tracing::info!(
            "no installed.json in {}; removing by well-known names",
            dir.display()
        );
    }
    let mut failures = Vec::new();

    // service first: it supervises the agent
    ctx.plan(&format!(
        "stop and delete service {SERVICE_NAME} (if registered)"
    ));
    if !ctx.dry_run {
        step(&mut failures, "service", service::uninstall);
    }

    ctx.plan(&format!("delete scheduled task {AGENT_TASK_NAME:?}"));
    if !ctx.dry_run {
        step(&mut failures, "scheduled task", task::delete);
    }

    ctx.plan(&format!("delete firewall rule {FIREWALL_RULE_NAME:?}"));
    if !ctx.dry_run {
        step(&mut failures, "firewall rule", firewall::remove_rule);
    }

    // SendTo shortcut: prefer the recorded path, fall back to the default location.
    let lnk = manifest
        .as_ref()
        .and_then(|m| m.send_to_shortcut.as_deref().map(PathBuf::from))
        .or_else(|| paths::sendto_dir().map(|d| shortcut::shortcut_path(&d)));
    if let Some(lnk) = lnk {
        ctx.plan(&format!("delete SendTo shortcut {}", lnk.display()));
        if !ctx.dry_run {
            step(&mut failures, "SendTo shortcut", || shortcut::remove(&lnk));
        }
    }

    // drivers installed by us: recorded ones plus anything the catalogue lists
    let mut hwids: Vec<(String, String, Vec<PathBuf>)> = manifest
        .as_ref()
        .map(|m| {
            m.drivers
                .iter()
                .map(|d| {
                    (
                        d.name.clone(),
                        d.hwid.clone(),
                        d.files.iter().map(PathBuf::from).collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if let Ok(specs) = driver::load_catalogue(dir) {
        for s in specs {
            if !hwids
                .iter()
                .any(|(_, h, _)| crate::inf::hwid_eq(h, &s.hwid))
            {
                let files = s.files.iter().map(|f| f.to.clone()).collect();
                hwids.push((s.name, s.hwid, files));
            }
        }
    }
    for (name, hwid, files) in hwids {
        ctx.plan(&format!(
            "remove driver {name:?} (hwid {hwid}) and its devices"
        ));
        let what = format!("driver {name}");
        if let Some(out) = step(&mut failures, &what, || {
            driver::uninstall(&hwid, ctx.dry_run)
        }) {
            if ctx.dry_run {
                println!(
                    "[dry-run]   {} device(s), {} package(s) found",
                    out.devices_removed, out.packages_removed
                );
            } else if out.reboot_required {
                tracing::warn!("removing {name} needs a reboot to finish");
            }
        }
        for f in files {
            ctx.plan(&format!("delete driver config file {}", f.display()));
            if !ctx.dry_run {
                let _ = std::fs::remove_file(&f);
                if let Some(parent) = f.parent() {
                    let _ = std::fs::remove_dir(parent); // only succeeds when empty
                }
            }
        }
    }

    // SoftwareSASGeneration: only if we created it
    if manifest.as_ref().is_some_and(|m| m.sas_policy) {
        ctx.plan("remove policy SoftwareSASGeneration (created by the installer)");
        if !ctx.dry_run {
            step(&mut failures, "SAS policy", crate::sas_policy::remove);
        }
    }

    // lid action
    step(&mut failures, "lid action", || restore_lid(ctx));

    if args.purge_user_data {
        step(&mut failures, "user data", || purge_user_data(ctx));
    }

    if ctx.dry_run {
        ctx.plan(&format!("delete {}", Manifest::path(dir).display()));
        return Ok(0);
    }
    if failures.is_empty() {
        step(&mut failures, "removing installed.json", || {
            Manifest::remove_file(dir)
        });
    }
    finish(&failures)
}

fn powercfg(args: &[String]) -> Result<String> {
    let out = Process::new("powercfg")
        .args(args)
        .output()
        .context("starting powercfg")?;
    if !out.status.success() {
        bail!(
            "powercfg {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Put the lid-close action back the way the agent found it.
fn restore_lid(ctx: &Ctx) -> Result<()> {
    let Some(path) = paths::lid_backup_file() else {
        return Ok(());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let backup = LidBackup::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
    let cmds = backup.powercfg_commands();
    for c in &cmds {
        ctx.plan(&format!("powercfg {}", c.join(" ")));
    }
    if ctx.dry_run {
        ctx.plan(&format!("delete {}", path.display()));
        return Ok(());
    }
    for c in &cmds {
        powercfg(c)?;
    }
    // Re-apply only if the restored scheme is the active one; never switch the user's plan.
    let active = powercfg(&["/getactivescheme".to_owned()])
        .ok()
        .and_then(|o| parse_active_scheme(&o));
    if backup.scheme.is_empty()
        || active
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(&backup.scheme))
    {
        powercfg(&["/setactive".to_owned(), backup.scheme_arg().to_owned()])?;
    }
    std::fs::remove_file(&path).ok();
    tracing::info!("lid action restored (ac={}, dc={})", backup.ac, backup.dc);
    Ok(())
}

/// Delete config/identity/peers/logs. The open `setup.log` is left in place.
fn purge_user_data(ctx: &Ctx) -> Result<()> {
    let roaming = paths::roaming_data_dir();
    let local = paths::local_data_dir();
    for d in roaming.iter().chain(local.iter()) {
        ctx.plan(&format!("delete user data {}", d.display()));
    }
    if ctx.dry_run {
        return Ok(());
    }
    if let Some(d) = &roaming {
        remove_tree_except_setup_log(d)?;
    }
    if let Some(d) = &local {
        remove_tree_except_setup_log(d)?;
    }
    Ok(())
}

fn remove_tree_except_setup_log(dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.file_name().is_some_and(|n| n == "setup.log") {
            continue;
        }
        if p.is_dir() {
            remove_tree_except_setup_log(&p)?;
            let _ = std::fs::remove_dir(&p); // fails (harmlessly) if setup.log is inside
        } else {
            std::fs::remove_file(&p).with_context(|| format!("deleting {}", p.display()))?;
        }
    }
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// status / network
// ---------------------------------------------------------------------------------------------

fn tri(r: Result<bool>) -> Value {
    match r {
        Ok(b) => json!({ "present": b }),
        Err(e) => json!({ "present": null, "error": format!("{e:#}") }),
    }
}

/// Read-only report of what is installed.
pub fn status(ctx: &Ctx) -> Result<Value> {
    let manifest = Manifest::load(&ctx.install_dir);
    let task_state = match task::presence() {
        Ok(task::Presence::Present) => json!({ "present": true }),
        Ok(task::Presence::Absent) => json!({ "present": false }),
        Ok(task::Presence::AccessDenied) => {
            json!({ "present": null, "note": "access denied (task is owned by an elevated principal); run elevated for an exact answer" })
        }
        Err(e) => json!({ "present": null, "error": format!("{e:#}") }),
    };
    let service_state = match service::query() {
        Ok(Some(running)) => json!({ "present": true, "running": running }),
        Ok(None) => json!({ "present": false }),
        Err(e) => json!({ "present": null, "error": format!("{e:#}") }),
    };
    let sendto = paths::sendto_dir().map(|d| shortcut::shortcut_path(&d));
    let sendto_state = json!({
        "present": sendto.as_ref().map(|p| p.is_file()),
        "path": sendto.as_ref().map(|p| p.display().to_string()),
    });

    // Drivers: everything we know about (manifest + catalogue), with the live device state.
    let mut known: Vec<(String, String)> = Vec::new();
    if let Ok(Some(m)) = &manifest {
        known.extend(m.drivers.iter().map(|d| (d.name.clone(), d.hwid.clone())));
    }
    if let Ok(specs) = driver::load_catalogue(&ctx.install_dir) {
        for s in specs {
            if !known.iter().any(|(_, h)| crate::inf::hwid_eq(h, &s.hwid)) {
                known.push((s.name, s.hwid));
            }
        }
    }
    let drivers: Vec<Value> = known
        .into_iter()
        .map(|(name, hwid)| {
            let present = driver::device_present(&hwid);
            let mut v = tri(present);
            v["name"] = json!(name);
            v["hwid"] = json!(hwid);
            v
        })
        .collect();

    let manifest_json = match &manifest {
        Ok(Some(m)) => {
            json!({ "present": true, "version": m.version, "installed_at_unix": m.installed_at_unix })
        }
        Ok(None) => json!({ "present": false }),
        Err(e) => json!({ "present": null, "error": format!("{e:#}") }),
    };

    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "install_dir": ctx.install_dir.display().to_string(),
        "elevated": crate::elevation::is_elevated(),
        "files": {
            "agent": ctx.agent_exe().is_file(),
            "service": ctx.install_dir.join(SERVICE_EXE).is_file(),
            "setup": ctx.install_dir.join(crate::brand::SETUP_EXE).is_file(),
        },
        "manifest": manifest_json,
        "firewall_rule": tri(firewall::is_present()),
        "scheduled_task": task_state,
        "send_to_shortcut": sendto_state,
        "service": service_state,
        "drivers": drivers,
        "lid_backup_present": paths::lid_backup_file().is_some_and(|p| p.is_file()),
    }))
}

pub fn check_network() -> Result<Value> {
    let nets = network::list_connected()?;
    let has_public = nets.iter().any(|n| n.category == network::Category::Public);
    let has_domain = nets.iter().any(|n| n.category == network::Category::Domain);
    Ok(json!({
        "networks": nets,
        "connected": nets.len(),
        "has_public": has_public,
        // The firewall rule only opens the Private profile.
        "peer_traffic_allowed": !nets.is_empty() && nets.iter().all(|n| n.category == network::Category::Private),
        "has_domain": has_domain,
    }))
}

pub fn make_private(ctx: &Ctx) -> Result<Value> {
    let changed = network::make_private(ctx.dry_run)?;
    if ctx.dry_run {
        for n in &changed {
            println!("[dry-run] set network {n:?} to Private");
        }
    }
    Ok(json!({ "changed": changed, "dry_run": ctx.dry_run }))
}

// ---------------------------------------------------------------------------------------------
// driver / service subcommands
// ---------------------------------------------------------------------------------------------

pub fn driver_cmd(ctx: &Ctx, cmd: &DriverCommand) -> Result<i32> {
    match cmd {
        DriverCommand::Inspect { inf } => {
            let bytes = std::fs::read(inf).with_context(|| format!("reading {}", inf.display()))?;
            let info = parse_inf(&decode_inf(&bytes));
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "class": info.class,
                    "class_guid": info.class_guid,
                    "provider": info.provider,
                    "driver_ver": info.driver_ver,
                    "hwids": info.hwids,
                }))?
            );
            Ok(0)
        }
        DriverCommand::Install { inf, hwid, name } => {
            let spec = DriverSpec {
                inf: inf.clone(),
                hwid: hwid.clone(),
                name: name.clone(),
                ..Default::default()
            };
            ctx.plan(&format!(
                "install driver {name:?}: {} (hwid {hwid})",
                inf.display()
            ));
            let out = driver::install(&spec, ctx.dry_run)?;
            report_driver_install(ctx, &spec, &out);
            if !ctx.dry_run {
                let mut m = Manifest::load(&ctx.install_dir)?
                    .unwrap_or_else(|| Manifest::new(&ctx.install_dir));
                let abs = paths::strip_verbatim(&std::fs::canonicalize(inf)?);
                m.upsert_driver(DriverRecord {
                    name: name.clone(),
                    inf: abs.display().to_string(),
                    hwid: hwid.clone(),
                    files: Vec::new(),
                });
                m.save(&ctx.install_dir).ok();
            }
            Ok(0)
        }
        DriverCommand::Uninstall { hwid } => {
            ctx.plan(&format!(
                "remove devices with hwid {hwid} and their driver packages"
            ));
            let out = driver::uninstall(hwid, ctx.dry_run)?;
            println!(
                "{}{} device(s), {} package(s){}",
                if ctx.dry_run {
                    "[dry-run] would remove "
                } else {
                    "removed "
                },
                out.devices_removed,
                out.packages_removed,
                if out.reboot_required {
                    " (reboot required)"
                } else {
                    ""
                }
            );
            if !ctx.dry_run {
                if let Ok(Some(mut m)) = Manifest::load(&ctx.install_dir) {
                    m.drivers.retain(|d| !crate::inf::hwid_eq(&d.hwid, hwid));
                    m.save(&ctx.install_dir).ok();
                }
            }
            Ok(0)
        }
    }
}

pub fn service_cmd(ctx: &Ctx, cmd: &ServiceCommand) -> Result<i32> {
    let exe = ctx.install_dir.join(SERVICE_EXE);
    match cmd {
        ServiceCommand::Install => {
            ctx.plan(&format!(
                "register service {SERVICE_NAME} -> {}",
                exe.display()
            ));
            if ctx.dry_run {
                return Ok(0);
            }
            let registered = service::install(&exe)?;
            if registered {
                let mut m = Manifest::load(&ctx.install_dir)?
                    .unwrap_or_else(|| Manifest::new(&ctx.install_dir));
                m.service = Some(SERVICE_NAME.to_owned());
                m.save(&ctx.install_dir).ok();
            } else {
                println!(
                    "{AGENT_EXE}/{SERVICE_EXE}: service executable not present, nothing registered"
                );
            }
            Ok(0)
        }
        ServiceCommand::Uninstall => {
            ctx.plan(&format!("stop and delete service {SERVICE_NAME}"));
            if !ctx.dry_run {
                service::uninstall()?;
                if let Ok(Some(mut m)) = Manifest::load(&ctx.install_dir) {
                    m.service = None;
                    m.save(&ctx.install_dir).ok();
                }
            }
            Ok(0)
        }
        ServiceCommand::Start => {
            ctx.plan(&format!("start service {SERVICE_NAME}"));
            if !ctx.dry_run && !service::start()? {
                bail!("service {SERVICE_NAME} is not registered");
            }
            Ok(0)
        }
        ServiceCommand::Stop => {
            ctx.plan(&format!("stop service {SERVICE_NAME}"));
            if !ctx.dry_run && !service::stop()? {
                bail!("service {SERVICE_NAME} is not registered");
            }
            Ok(0)
        }
    }
}
