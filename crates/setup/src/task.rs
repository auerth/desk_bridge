//! Scheduled task `DeskBridge Agent` (Task Scheduler COM API, XML registration).

use std::path::Path;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::TaskScheduler::{
    IRegisteredTask, ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_NONE,
    TaskScheduler,
};
use windows::core::{BSTR, VARIANT};

use crate::brand::AGENT_TASK_NAME;
use crate::taskxml::{TaskPrincipal, agent_task_xml};

/// Tri-state for read-only queries that may be denied to non-admins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
    /// The Task Scheduler denied read access (task exists but belongs to an elevated principal).
    AccessDenied,
}

fn root_folder() -> Result<ITaskFolder> {
    let svc: ITaskService = unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
        .context("CoCreateInstance(TaskScheduler)")?;
    let empty = VARIANT::default();
    unsafe { svc.Connect(&empty, &empty, &empty, &empty) }.context("ITaskService::Connect")?;
    unsafe { svc.GetFolder(&BSTR::from("\\")) }.context("ITaskService::GetFolder(\\)")
}

fn get_task(folder: &ITaskFolder) -> windows::core::Result<IRegisteredTask> {
    unsafe { folder.GetTask(&BSTR::from(AGENT_TASK_NAME)) }
}

/// Read-only query.
pub fn presence() -> Result<Presence> {
    let folder = root_folder()?;
    match get_task(&folder) {
        Ok(_) => Ok(Presence::Present),
        Err(e) if e.code() == E_ACCESSDENIED => Ok(Presence::AccessDenied),
        Err(_) => Ok(Presence::Absent),
    }
}

/// Create or update the task. Falls back to `schtasks /create /xml` if COM registration fails.
pub fn register(agent_exe: &Path, working_dir: &Path, principal: &TaskPrincipal) -> Result<()> {
    let xml = agent_task_xml(agent_exe, working_dir, principal);
    match register_com(&xml) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("task registration via COM failed ({e:#}); falling back to schtasks");
            register_schtasks(&xml)
        }
    }
}

fn register_com(xml: &str) -> Result<()> {
    let folder = root_folder()?;
    let empty = VARIANT::default();
    unsafe {
        folder.RegisterTask(
            &BSTR::from(AGENT_TASK_NAME),
            &BSTR::from(xml),
            TASK_CREATE_OR_UPDATE.0,
            &empty,
            &empty,
            TASK_LOGON_NONE,
            &empty,
        )
    }
    .context("ITaskFolder::RegisterTask")?;
    Ok(())
}

fn register_schtasks(xml: &str) -> Result<()> {
    // schtasks wants UTF-16 with BOM for XML that declares encoding="UTF-16".
    let path = std::env::temp_dir().join(format!("deskbridge-task-{}.xml", std::process::id()));
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&path, bytes).context("writing temporary task xml")?;
    let out = std::process::Command::new("schtasks")
        .args(["/create", "/f", "/tn", AGENT_TASK_NAME, "/xml"])
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);
    let out = out.context("starting schtasks")?;
    if !out.status.success() {
        bail!(
            "schtasks /create failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Start the task now (so no re-logon is needed after installing).
pub fn run_now() -> Result<()> {
    let folder = root_folder()?;
    let task = get_task(&folder).context("task not found")?;
    unsafe { task.Run(&VARIANT::default()) }.context("IRegisteredTask::Run")?;
    Ok(())
}

/// Delete the task; a missing task is not an error.
pub fn delete() -> Result<()> {
    let folder = root_folder()?;
    match unsafe { folder.DeleteTask(&BSTR::from(AGENT_TASK_NAME), 0) } {
        Ok(()) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e).context("ITaskFolder::DeleteTask"),
    }
}

/// HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND) is what the Task Scheduler returns for missing tasks.
fn is_not_found(e: &windows::core::Error) -> bool {
    e.code().0 == 0x8007_0002_u32 as i32
}
