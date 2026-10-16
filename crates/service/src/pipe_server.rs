//! Generic named-pipe server for the service protocol (tokio).
//!
//! One instance serves both pipes: the helper pipe (injection) and the service control pipe. A
//! [`Handler`] receives decoded requests; the server writes its responses back in order.

use crate::pipe_security::{PipeAccess, SecurityDescriptor, build_sddl};
use crate::protocol::{ErrorCode, FrameBuffer, Request, Response};
use std::ffi::c_void;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{Notify, watch};

/// Receives requests from one pipe.
pub trait Handler: Send + Sync + 'static {
    /// Handles one request; `Some` is written back, `None` stays silent (injection commands).
    fn handle(&self, req: Request) -> Option<Response>;

    /// A client connection ended (cleanly, broken pipe or protocol error). The helper releases
    /// all keys here: a crashed agent must never leave a key pressed.
    fn on_disconnect(&self) {}
}

/// Supplies the pipe access policy; re-evaluated every time a new pipe instance is created so a
/// changed interactive user is picked up (see `refresh` in [`serve`]).
pub type AccessFn = Arc<dyn Fn() -> PipeAccess + Send + Sync>;

/// Accepts clients on `name` until `shutdown` turns `true`.
///
/// `refresh` makes the server drop its idle pending instance and create a new one with a freshly
/// evaluated DACL (after logon/logoff). Already connected clients are not affected.
pub async fn serve(
    name: String,
    access: AccessFn,
    handler: Arc<dyn Handler>,
    mut shutdown: watch::Receiver<bool>,
    refresh: Arc<Notify>,
) {
    let mut first = true;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let server = match create_instance(&name, first, &access()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("cannot create pipe {name}: {e}");
                tokio::select! {
                    () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                    _ = shutdown.changed() => {}
                }
                continue;
            }
        };
        first = false;
        tokio::select! {
            r = server.connect() => {
                if let Err(e) = r {
                    tracing::debug!("pipe {name} connect: {e}");
                    continue;
                }
                let handler = handler.clone();
                let sd = shutdown.clone();
                let label = name.clone();
                tokio::spawn(connection(server, handler, sd, label));
            }
            () = refresh.notified() => {
                tracing::debug!("pipe {name}: recreating instance with refreshed access");
            }
            _ = shutdown.changed() => return,
        }
    }
}

/// Runs `on_disconnect` even if the connection task panics or is cancelled.
struct DisconnectGuard(Arc<dyn Handler>);

impl Drop for DisconnectGuard {
    fn drop(&mut self) {
        self.0.on_disconnect();
    }
}

async fn connection(
    mut pipe: NamedPipeServer,
    handler: Arc<dyn Handler>,
    mut shutdown: watch::Receiver<bool>,
    label: String,
) {
    let _guard = DisconnectGuard(handler.clone());
    let mut frames = FrameBuffer::default();
    let mut buf = [0u8; 512];
    loop {
        let n = tokio::select! {
            r = pipe.read(&mut buf) => match r {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            },
            _ = shutdown.changed() => return,
        };
        frames.push(&buf[..n]);
        loop {
            let payload = match frames.next_frame() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("pipe {label}: dropping client, {e}");
                    return;
                }
            };
            let response = match Request::decode(&payload) {
                Ok(req) => handler.handle(req),
                Err(e) => Some(Response::error(ErrorCode::BadRequest, e.to_string())),
            };
            if let Some(resp) = response {
                if pipe.write_all(&resp.encode()).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn create_instance(name: &str, first: bool, access: &PipeAccess) -> std::io::Result<NamedPipeServer> {
    let sd = SecurityDescriptor::from_sddl(&build_sddl(access))?;
    let mut sa = sd.attributes();
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: `sa` and the descriptor it points to live until after the call; the kernel copies
    // the descriptor into the pipe object.
    unsafe { opts.create_with_security_attributes_raw(name, (&raw mut sa).cast::<c_void>()) }
}
