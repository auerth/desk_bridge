//! Elevation check and UAC re-launch.

use std::ffi::OsString;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR};

/// Is the current process token elevated (full administrator token)?
#[must_use]
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut ret = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
    }
    .is_ok();
    let _ = unsafe { CloseHandle(token) };
    ok && elevation.TokenIsElevated != 0
}

/// Quote one argument for a Windows command line.
fn quote(arg: &std::ffi::OsStr) -> String {
    let s = arg.to_string_lossy();
    if !s.is_empty() && !s.contains([' ', '\t', '"']) {
        return s.into_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Re-run this executable with the same arguments through the UAC prompt and wait for it.
/// Returns the elevated process' exit code.
pub fn relaunch_elevated(args: &[OsString]) -> Result<i32> {
    let exe = std::env::current_exe().context("current_exe")?;
    let params = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
    let verb = HSTRING::from("runas");
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(params);

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
        .context("could not start the elevated process (UAC prompt declined?)")?;
    let process = info.hProcess;
    if process.is_invalid() {
        bail!("elevated process handle missing");
    }
    let wait = unsafe { WaitForSingleObject(process, INFINITE) };
    let mut code = 0u32;
    let got = unsafe { GetExitCodeProcess(process, &mut code) };
    let _ = unsafe { CloseHandle(process) };
    if wait != WAIT_OBJECT_0 || got.is_err() {
        bail!("waiting for the elevated process failed");
    }
    Ok(code as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn quoting() {
        assert_eq!(quote(OsStr::new("install")), "install");
        assert_eq!(
            quote(OsStr::new(r"C:\Program Files\x")),
            r#""C:\Program Files\x""#
        );
        assert_eq!(quote(OsStr::new(r#"a"b"#)), r#""a\"b""#);
        assert_eq!(
            quote(OsStr::new(r"C:\dir with space\")),
            r#""C:\dir with space\\""#
        );
        assert_eq!(quote(OsStr::new("")), r#""""#);
    }
}
