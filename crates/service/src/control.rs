//! The service control pipe (`\\.\pipe\deskbridge-service`): `ping`, `send_sas`, `helper_status`,
//! `cable_mic`.

use crate::pipe_server::Handler;
use crate::protocol::{ErrorCode, PROTOCOL_VERSION, Request, Response};
use crate::winutil::wide;
use parking_lot::Mutex;
use std::sync::Arc;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{PCSTR, PCWSTR};

/// Why Ctrl+Alt+Del could not be generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SasError {
    /// `SoftwareSASGeneration` does not allow services (value 0, absent or 2).
    NotAllowedByPolicy,
    /// `sas.dll` / `SendSAS` not available.
    Unavailable(String),
    /// Switched off by `--no-sas`.
    Disabled,
}

impl SasError {
    #[must_use]
    pub fn to_response(&self) -> Response {
        match self {
            Self::NotAllowedByPolicy => Response::error(
                ErrorCode::SasNotAllowed,
                "SoftwareSASGeneration policy does not allow services (set it to 1 or 3)",
            ),
            Self::Unavailable(why) => Response::error(ErrorCode::Unsupported, why.clone()),
            Self::Disabled => Response::error(ErrorCode::Disabled, "SendSAS disabled (--no-sas)"),
        }
    }
}

/// Generates the secure attention sequence.
pub trait SasSender: Send + Sync {
    fn send(&self) -> Result<(), SasError>;
}

/// `--no-sas`: never touches `sas.dll`.
pub struct DisabledSas;

impl SasSender for DisabledSas {
    fn send(&self) -> Result<(), SasError> {
        Err(SasError::Disabled)
    }
}

/// Registry policy that controls who may call `SendSAS`.
pub const SAS_POLICY_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
pub const SAS_POLICY_VALUE: &str = "SoftwareSASGeneration";

/// Does the policy value `v` allow a *service* to call `SendSAS(FALSE)`?
///
/// 0 = nobody, 1 = services, 2 = ease-of-access applications, 3 = both. Without the policy
/// Windows silently ignores the call, which is why we check up front and report it.
#[must_use]
pub fn policy_allows_services(v: Option<u32>) -> bool {
    matches!(v, Some(1 | 3))
}

/// Reads `SoftwareSASGeneration` (HKLM). `None` if the value does not exist.
#[must_use]
pub fn read_sas_policy() -> Option<u32> {
    let key = wide(SAS_POLICY_KEY);
    let val = wide(SAS_POLICY_VALUE);
    let mut data = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: valid NUL-terminated names; `data`/`len` describe a DWORD buffer.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&raw mut len),
        )
    };
    r.is_ok().then_some(data)
}

/// Real `SendSAS(FALSE)` from `sas.dll`, loaded on demand from System32.
///
/// Requirements: the caller is a service (LocalSystem) and the policy
/// `HKLM\...\Policies\System\SoftwareSASGeneration` is 1 (services) or 3 (services and
/// ease-of-access apps). The installer/administrator sets it; this code never changes system
/// policy. Without it the request is refused with [`ErrorCode::SasNotAllowed`].
pub struct SystemSas;

impl SasSender for SystemSas {
    fn send(&self) -> Result<(), SasError> {
        if !policy_allows_services(read_sas_policy()) {
            return Err(SasError::NotAllowedByPolicy);
        }
        let dll = wide("sas.dll");
        // SAFETY: loads a system DLL by name from System32 only; the function pointer is used
        // only while the module is loaded and has the documented signature
        // `void WINAPI SendSAS(BOOL AsUser)`.
        unsafe {
            let module = LoadLibraryExW(PCWSTR(dll.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                .map_err(|e| SasError::Unavailable(format!("sas.dll: {e}")))?;
            let result = match GetProcAddress(module, PCSTR(c"SendSAS".as_ptr().cast())) {
                Some(f) => {
                    let send: unsafe extern "system" fn(i32) = std::mem::transmute(f);
                    send(0);
                    Ok(())
                }
                None => Err(SasError::Unavailable("SendSAS export missing".into())),
            };
            let _ = FreeLibrary(module);
            result
        }
    }
}

/// What the control pipe reports about the helper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HelperStatus {
    pub running: bool,
    pub pid: u32,
    pub session: u32,
}

/// Shared with the supervisor, which keeps it up to date.
pub type SharedHelperStatus = Arc<Mutex<HelperStatus>>;

/// Handler of the service control pipe.
pub struct ControlHandler {
    pub sas: Arc<dyn SasSender>,
    pub helper: SharedHelperStatus,
    pub cable_mic: Arc<crate::cable_mic::CableMic>,
}

impl Handler for ControlHandler {
    fn handle(&self, req: Request) -> Option<Response> {
        Some(match req {
            // Injection commands are fire-and-forget on the wire, so refusing them must stay
            // silent too (an unexpected response would desynchronise the client).
            r if r.is_injection() => {
                tracing::debug!("ignoring injection request on the control pipe");
                return None;
            }
            Request::Ping => Response::Pong {
                version: PROTOCOL_VERSION,
                pid: std::process::id(),
            },
            Request::SendSas => match self.sas.send() {
                Ok(()) => {
                    tracing::info!("SendSAS issued");
                    Response::Ok
                }
                Err(e) => {
                    tracing::warn!("SendSAS refused: {e:?}");
                    e.to_response()
                }
            },
            Request::HelperStatus => {
                let s = *self.helper.lock();
                Response::HelperStatus {
                    running: s.running,
                    pid: s.pid,
                    session: s.session,
                }
            }
            Request::CableMic { enabled, managed } => {
                match self.cable_mic.request(enabled, managed) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::error(ErrorCode::Failed, e),
                }
            }
            _ => Response::error(ErrorCode::BadRequest, "not served by the service pipe"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingSas(Mutex<u32>);
    impl SasSender for CountingSas {
        fn send(&self) -> Result<(), SasError> {
            *self.0.lock() += 1;
            Ok(())
        }
    }

    #[test]
    fn policy_values() {
        assert!(!policy_allows_services(None));
        assert!(!policy_allows_services(Some(0)));
        assert!(policy_allows_services(Some(1)));
        assert!(!policy_allows_services(Some(2)));
        assert!(policy_allows_services(Some(3)));
        assert!(!policy_allows_services(Some(4)));
    }

    #[test]
    fn control_handler_dispatch() {
        let sas = Arc::new(CountingSas(Mutex::new(0)));
        let helper: SharedHelperStatus = Arc::new(Mutex::new(HelperStatus {
            running: true,
            pid: 9,
            session: 1,
        }));
        let h = ControlHandler {
            sas: sas.clone(),
            helper,
            cable_mic: crate::cable_mic::CableMic::load(),
        };
        assert!(matches!(
            h.handle(Request::Ping),
            Some(Response::Pong { .. })
        ));
        assert_eq!(h.handle(Request::SendSas), Some(Response::Ok));
        assert_eq!(*sas.0.lock(), 1);
        assert_eq!(
            h.handle(Request::HelperStatus),
            Some(Response::HelperStatus {
                running: true,
                pid: 9,
                session: 1
            })
        );
        assert_eq!(h.handle(Request::ReleaseAll), None);
        assert_eq!(*sas.0.lock(), 1);
    }

    #[test]
    fn disabled_sas_reports_disabled() {
        let h = ControlHandler {
            sas: Arc::new(DisabledSas),
            helper: Arc::new(Mutex::new(HelperStatus::default())),
            cable_mic: crate::cable_mic::CableMic::load(),
        };
        assert!(matches!(
            h.handle(Request::SendSas),
            Some(Response::Error {
                code: ErrorCode::Disabled,
                ..
            })
        ));
    }

    #[test]
    fn sas_errors_map_to_codes() {
        assert!(matches!(
            SasError::NotAllowedByPolicy.to_response(),
            Response::Error {
                code: ErrorCode::SasNotAllowed,
                ..
            }
        ));
        assert!(matches!(
            SasError::Unavailable("x".into()).to_response(),
            Response::Error {
                code: ErrorCode::Unsupported,
                ..
            }
        ));
    }
}
