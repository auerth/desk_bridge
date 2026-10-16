//! Process creation in the console session and process lookup.
//!
//! Token juggling, in short:
//! * **Agent**: `WTSQueryUserToken(session)` yields the user's logon token. For an administrator
//!   with UAC that is the *filtered* (medium) half; `TokenLinkedToken` gives the elevated half,
//!   which we duplicate to a primary token. This is the same token the scheduled task
//!   ("highest privileges") produces, so the agent behaves identically however it was started.
//! * **Helper**: our own SYSTEM token, duplicated as a primary token with `TokenSessionId` set to
//!   the console session, so it can attach to that session's `winsta0\Winlogon` desktop.
//!   Both need `SeTcbPrivilege`/`SeAssignPrimaryTokenPrivilege`, which LocalSystem services have.

use crate::winutil::{OwnedHandle, wide};
use std::ffi::c_void;
use std::io;
use std::path::Path;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, SetTokenInformation,
    TOKEN_ACCESS_MASK, TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN, TokenElevationType,
    TokenElevationTypeLimited, TokenLinkedToken, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
    GetExitCodeProcess, OpenMutexW, OpenProcess, OpenProcessToken, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE, TerminateProcess,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// `MAXIMUM_ALLOWED`
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
/// `TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID`
const TOKEN_FOR_HELPER: u32 = 0x0002 | 0x0008 | 0x0001 | 0x0080 | 0x0100;

/// What a freshly created process is.
#[derive(Debug)]
pub struct Launched {
    pub process: OwnedHandle,
    pub pid: u32,
}

/// How to derive the agent token from the one `WTSQueryUserToken` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPlan {
    /// Already elevated (UAC off, built-in Administrator) or a non-admin user: use as is.
    UseDirect,
    /// Filtered half of a split token: switch to the linked (elevated) token.
    UseLinked,
}

/// Pure decision on `TokenElevationType`.
#[must_use]
pub fn plan_token(elevation_type: i32) -> TokenPlan {
    if elevation_type == TokenElevationTypeLimited.0 { TokenPlan::UseLinked } else { TokenPlan::UseDirect }
}

fn win_err(e: &windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

fn user_token(session: u32) -> io::Result<OwnedHandle> {
    let mut t = HANDLE::default();
    // SAFETY: valid out pointer; requires SeTcbPrivilege (LocalSystem service).
    unsafe { WTSQueryUserToken(session, &mut t) }.map_err(|e| win_err(&e))?;
    Ok(OwnedHandle(t))
}

fn duplicate_primary(src: HANDLE) -> io::Result<OwnedHandle> {
    let mut out = HANDLE::default();
    // SAFETY: valid handles; produces a new primary token owned by the caller.
    unsafe {
        DuplicateTokenEx(
            src,
            TOKEN_ACCESS_MASK(MAXIMUM_ALLOWED),
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut out,
        )
    }
    .map_err(|e| win_err(&e))?;
    Ok(OwnedHandle(out))
}

/// Result of [`agent_token`].
pub struct AgentToken {
    pub token: OwnedHandle,
    pub elevated: bool,
    /// SID string of the user the token belongs to.
    pub user_sid: Option<String>,
}

/// Elevated primary token of the user logged on in `session`.
pub fn agent_token(session: u32) -> io::Result<AgentToken> {
    let user = user_token(session)?;
    let user_sid = crate::winutil::token_user_sid(user.raw());
    let mut ty = TOKEN_ELEVATION_TYPE(0);
    let mut len = 0u32;
    // SAFETY: out buffer is a TOKEN_ELEVATION_TYPE-sized value.
    unsafe {
        GetTokenInformation(
            user.raw(),
            TokenElevationType,
            Some((&raw mut ty).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut len,
        )
    }
    .map_err(|e| win_err(&e))?;
    match plan_token(ty.0) {
        TokenPlan::UseLinked => {
            let mut linked = TOKEN_LINKED_TOKEN::default();
            // SAFETY: out buffer is a TOKEN_LINKED_TOKEN; the returned handle is owned by us.
            unsafe {
                GetTokenInformation(
                    user.raw(),
                    TokenLinkedToken,
                    Some((&raw mut linked).cast::<c_void>()),
                    size_of::<TOKEN_LINKED_TOKEN>() as u32,
                    &mut len,
                )
            }
            .map_err(|e| win_err(&e))?;
            let linked = OwnedHandle(linked.LinkedToken);
            Ok(AgentToken { token: duplicate_primary(linked.raw())?, elevated: true, user_sid })
        }
        // WTSQueryUserToken already returns a primary token.
        TokenPlan::UseDirect => {
            Ok(AgentToken { token: user, elevated: ty.0 == 2 /* TokenElevationTypeFull */, user_sid })
        }
    }
}

/// SID string of the user logged on in `session`.
#[must_use]
pub fn session_user_sid(session: u32) -> Option<String> {
    user_token(session).ok().and_then(|t| crate::winutil::token_user_sid(t.raw()))
}

/// SYSTEM primary token whose session id is `session`.
fn system_token_for_session(session: u32) -> io::Result<OwnedHandle> {
    let mut own = HANDLE::default();
    // SAFETY: valid out pointer.
    unsafe { OpenProcessToken(GetCurrentProcess(), windows::Win32::Security::TOKEN_ACCESS_MASK(TOKEN_FOR_HELPER), &mut own) }
        .map_err(|e| win_err(&e))?;
    let own = OwnedHandle(own);
    let dup = duplicate_primary(own.raw())?;
    // SAFETY: `session` is a u32 as TokenSessionId requires.
    unsafe {
        SetTokenInformation(
            dup.raw(),
            TokenSessionId,
            (&raw const session).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    }
    .map_err(|e| win_err(&e))?;
    Ok(dup)
}

/// `"exe" args`
#[must_use]
pub fn command_line(exe: &Path, args: &[String]) -> String {
    let mut s = format!("\"{}\"", exe.display());
    for a in args {
        s.push(' ');
        if a.contains([' ', '\t', '"']) {
            s.push('"');
            s.push_str(&a.replace('"', "\\\""));
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

fn create_as_user(
    token: HANDLE,
    exe: &Path,
    args: &[String],
    desktop: &str,
    env: Option<*const c_void>,
) -> io::Result<Launched> {
    let mut cmd = wide(&command_line(exe, args));
    let app = wide(&exe.to_string_lossy());
    let mut desk = wide(desktop);
    let dir = exe.parent().map(|p| wide(&p.to_string_lossy()));
    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desk.as_mut_ptr()),
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    let mut flags = CREATE_NO_WINDOW;
    if env.is_some() {
        flags |= CREATE_UNICODE_ENVIRONMENT;
    }
    // SAFETY: all buffers are NUL-terminated and outlive the call; `env` (if any) is a valid
    // Unicode environment block; `cmd` is mutable as CreateProcess requires.
    unsafe {
        CreateProcessAsUserW(
            token,
            PCWSTR(app.as_ptr()),
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            false,
            flags,
            env,
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &si,
            &mut pi,
        )
    }
    .map_err(|e| win_err(&e))?;
    drop(OwnedHandle(pi.hThread));
    Ok(Launched { process: OwnedHandle(pi.hProcess), pid: pi.dwProcessId })
}

/// Starts the agent in `session` with the user's elevated token on `winsta0\default`.
pub fn launch_agent(session: u32, exe: &Path) -> io::Result<(Launched, bool)> {
    let tok = agent_token(session)?;
    let mut env: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid token; the block is destroyed after CreateProcessAsUserW returned.
    let have_env = unsafe { CreateEnvironmentBlock(&mut env, tok.token.raw(), false) }.is_ok();
    let r = create_as_user(
        tok.token.raw(),
        exe,
        &[],
        r"winsta0\default",
        have_env.then_some(env.cast_const()),
    );
    if have_env {
        // SAFETY: `env` came from CreateEnvironmentBlock.
        unsafe {
            let _ = DestroyEnvironmentBlock(env);
        }
    }
    r.map(|l| (l, tok.elevated))
}

/// Starts `exe --helper ...` as SYSTEM on `winsta0\winlogon` of `session`.
pub fn launch_helper(session: u32, exe: &Path, args: &[String]) -> io::Result<Launched> {
    let token = system_token_for_session(session)?;
    create_as_user(token.raw(), exe, args, r"winsta0\winlogon", None)
}

/// Exit code if the process has ended, `None` while it runs.
#[must_use]
pub fn exit_code(process: &OwnedHandle) -> Option<u32> {
    // SAFETY: valid process handle with SYNCHRONIZE/QUERY access.
    unsafe {
        if WaitForSingleObject(process.raw(), 0) != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        GetExitCodeProcess(process.raw(), &mut code).ok()?;
        Some(code)
    }
}

/// Kills a process we started (helper on shutdown or after a session change).
pub fn terminate(process: &OwnedHandle) {
    // SAFETY: valid handle with PROCESS_TERMINATE (we created the process).
    unsafe {
        let _ = TerminateProcess(process.raw(), 1);
    }
}

/// Opens `pid` for waiting and exit-code queries.
#[must_use]
pub fn open_for_wait(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: plain OpenProcess.
    unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .ok()
        .map(OwnedHandle)
}

/// PID of a process called `exe_name` (case-insensitive) running in `session`.
#[must_use]
pub fn find_process_in_session(exe_name: &str, session: u32) -> Option<u32> {
    // SAFETY: standard snapshot walk; `entry.dwSize` is initialised as required.
    unsafe {
        let snap = OwnedHandle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap.raw(), &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(exe_name) {
                let mut sid = 0u32;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut sid).is_ok() && sid == session {
                    return Some(entry.th32ProcessID);
                }
            }
            ok = Process32NextW(snap.raw(), &mut entry).is_ok();
        }
        None
    }
}

/// Does a mutex called `name` exist (the agent's single-instance mutex)?
#[must_use]
pub fn mutex_exists(name: &str) -> bool {
    let w = wide(name);
    // SAFETY: NUL-terminated name; the handle is closed by the wrapper.
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(w.as_ptr())) } {
        Ok(h) => {
            drop(OwnedHandle(h));
            true
        }
        // Exists but we may not open it: still an existing instance.
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_plan() {
        assert_eq!(plan_token(TokenElevationTypeLimited.0), TokenPlan::UseLinked);
        assert_eq!(plan_token(1), TokenPlan::UseDirect); // Default (standard user, UAC off)
        assert_eq!(plan_token(2), TokenPlan::UseDirect); // Full
    }

    #[test]
    fn command_lines() {
        let exe = Path::new(r"C:\Program Files\Desk Bridge\deskbridge-service.exe");
        assert_eq!(
            command_line(exe, &["--helper".into(), "--user-sid".into(), "S-1-5-18".into()]),
            r#""C:\Program Files\Desk Bridge\deskbridge-service.exe" --helper --user-sid S-1-5-18"#
        );
        assert_eq!(command_line(exe, &["a b".into()]), format!("\"{}\" \"a b\"", exe.display()));
    }

    #[test]
    fn process_lookup_finds_a_running_process_and_missing_mutex() {
        // Our own test executable is running in our own session.
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_string_lossy().into_owned();
        let mut sid = 0u32;
        // SAFETY: valid out pointer.
        unsafe { ProcessIdToSessionId(std::process::id(), &mut sid) }.unwrap();
        assert!(find_process_in_session(&name, sid).is_some());
        assert!(find_process_in_session("definitely-not-a-process.exe", sid).is_none());
        assert!(!mutex_exists(r"Global\softwelop-deskbridge-test-does-not-exist"));
    }
}
