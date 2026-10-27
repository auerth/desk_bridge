//! Named pipe server (UI protocol), single-instance mutex and the small CLI client.

use deskbridge_core::brand;
use deskbridge_core::engine::EngineHandle;
use deskbridge_core::ipc_server::serve_connection;
use std::ffi::c_void;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// Pipe name: `DESKBRIDGE_PIPE` (full path or bare name) or the default.
pub fn pipe_name() -> String {
    match std::env::var("DESKBRIDGE_PIPE") {
        Ok(v) if !v.is_empty() => {
            if v.starts_with("\\\\") {
                v
            } else {
                format!("\\\\.\\pipe\\{v}")
            }
        }
        _ => brand::UI_PIPE_NAME.to_string(),
    }
}

/// Holds the single-instance mutex for the process lifetime.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    pub fn acquire(pipe_name: &str) -> Option<Self> {
        let suffix: String = pipe_name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let name = if pipe_name == brand::UI_PIPE_NAME {
            brand::AGENT_INSTANCE_ID.to_string()
        } else {
            format!("{}-{suffix}", brand::AGENT_INSTANCE_ID)
        };
        Self::acquire_name(&name)
    }

    /// Holds the mutex `name` (the logon-screen agent has its own, see `brand`).
    pub fn acquire_name(name: &str) -> Option<Self> {
        // SAFETY: plain Win32 call with a valid, NUL-terminated name.
        unsafe {
            let h = CreateMutexW(None, false, &HSTRING::from(name)).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return None;
            }
            Some(Self(h))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateMutexW.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// SDDL granting full access to the current user and SYSTEM only.
fn pipe_sddl() -> Option<String> {
    // SAFETY: standard token query; buffers sized by the first call.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let r = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        r.ok()?;
        let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid).ok()?;
        let s = sid.to_string().ok();
        let _ = LocalFree(HLOCAL(sid.0.cast()));
        s.map(|sid| format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"))
    }
}

/// Accepts UI connections forever.
pub async fn serve(name: String, engine: EngineHandle) {
    let sddl = pipe_sddl();
    if sddl.is_none() {
        tracing::warn!("could not build pipe ACL, falling back to the default");
    }
    let mut first = true;
    loop {
        let server = match create_server(&name, first, sddl.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("cannot create UI pipe {name}: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        first = false;
        if let Err(e) = server.connect().await {
            tracing::debug!("pipe connect: {e}");
            continue;
        }
        let engine = engine.clone();
        tokio::spawn(async move { serve_connection(server, engine).await });
    }
}

fn create_server(
    name: &str,
    first: bool,
    sddl: Option<&str>,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first).reject_remote_clients(true);
    let Some(sddl) = sddl else {
        return opts.create(name);
    };
    // SAFETY: the descriptor stays alive until after the call returns (the kernel copies it).
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let r = opts.create_with_security_attributes_raw(
            name,
            (&mut sa as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
        );
        let _ = LocalFree(HLOCAL(sd.0));
        r
    }
}

pub enum ClientCommand {
    Send(Vec<String>),
    Stop,
}

/// CLI client: one request, print the answer, return an exit code.
pub fn client_main(cmd: ClientCommand) -> i32 {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return 1;
    };
    rt.block_on(async {
        let req = match cmd {
            ClientCommand::Send(paths) => {
                if paths.is_empty() {
                    eprintln!("usage: deskbridge-agent send <path>...");
                    return 2;
                }
                let abs: Vec<String> = paths
                    .iter()
                    .map(|p| {
                        std::fs::canonicalize(p).map_or_else(
                            |_| p.clone(),
                            |c| c.to_string_lossy().trim_start_matches("\\\\?\\").to_string(),
                        )
                    })
                    .collect();
                serde_json::json!({"id": 1, "req": {"type": "send_files", "paths": abs}})
            }
            ClientCommand::Stop => serde_json::json!({"id": 1, "req": {"type": "shutdown_agent"}}),
        };
        let client = match ClientOptions::new().open(pipe_name()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{} agent is not running: {e}", brand::PRODUCT_NAME);
                return 1;
            }
        };
        let (rd, mut wr) = tokio::io::split(client);
        if wr.write_all(format!("{req}\n").as_bytes()).await.is_err() {
            return 1;
        }
        let mut lines = BufReader::new(rd).lines();
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(10));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                () = &mut deadline => return 1,
                l = lines.next_line() => match l {
                    Ok(Some(line)) => {
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                        if v.get("id").and_then(serde_json::Value::as_u64) == Some(1) {
                            if v["ok"].as_bool() == Some(true) {
                                return 0;
                            }
                            eprintln!("{}", v["error"].as_str().unwrap_or("error"));
                            return 1;
                        }
                    }
                    _ => return 1,
                }
            }
        }
    })
}
