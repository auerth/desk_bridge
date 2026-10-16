//! The service's moving parts, independent of the SCM: supervisor thread + control pipe.
//! Used by the real service (`winsvc`) and by `--console`.

use crate::control::{ControlHandler, SasSender};
use crate::pipe_security::PipeAccess;
use crate::pipe_server::{self, AccessFn};
use crate::protocol::SERVICE_PIPE;
use crate::supervisor::{Event, Options, Shared, Supervisor};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::watch;

pub struct RuntimeOptions {
    pub exe_dir: PathBuf,
    pub sas: Arc<dyn SasSender>,
    pub manage_agent: bool,
    pub manage_helper: bool,
    pub control_pipe: String,
}

impl RuntimeOptions {
    #[must_use]
    pub fn new(sas: Arc<dyn SasSender>) -> Self {
        Self {
            exe_dir: crate::winutil::exe_dir(),
            sas,
            manage_agent: true,
            manage_helper: true,
            control_pipe: SERVICE_PIPE.to_string(),
        }
    }
}

pub struct Runtime {
    tokio: Option<tokio::runtime::Runtime>,
    shutdown: watch::Sender<bool>,
    events: Sender<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn start(opts: RuntimeOptions) -> std::io::Result<Self> {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("dbs-rt")
            .build()?;
        crate::launch::enable_service_privileges();
        let shared = Shared::new();
        let (shutdown, shutdown_rx) = watch::channel(false);

        let access: AccessFn = {
            let shared = shared.clone();
            Arc::new(move || PipeAccess::production(shared.user_sid.lock().clone()))
        };
        let handler = Arc::new(ControlHandler {
            sas: opts.sas.clone(),
            helper: shared.helper.clone(),
            cable_mic: shared.cable_mic.clone(),
        });
        tokio.spawn(pipe_server::serve(
            opts.control_pipe.clone(),
            access,
            handler,
            shutdown_rx,
            shared.sid_changed.clone(),
        ));

        let (events, rx) = channel();
        let supervisor = Supervisor::new(
            Options {
                exe_dir: opts.exe_dir,
                manage_agent: opts.manage_agent,
                manage_helper: opts.manage_helper,
            },
            shared,
        );
        let thread = std::thread::Builder::new()
            .name("dbs-supervisor".into())
            .spawn(move || {
                // A panic here must not take the SCM callbacks down with it.
                let r =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| supervisor.run(&rx)));
                if r.is_err() {
                    tracing::error!("supervisor thread panicked");
                }
            })?;
        Ok(Self {
            tokio: Some(tokio),
            shutdown,
            events,
            thread: Some(thread),
        })
    }

    /// Forwards `SERVICE_CONTROL_SESSIONCHANGE`.
    pub fn session_change(&self, event_type: u32, session: u32) {
        let _ = self.events.send(Event::SessionChange(event_type, session));
    }

    /// Stops the supervisor (and with it the helper) and the pipe server.
    pub fn stop(&mut self) {
        let _ = self.events.send(Event::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = self.shutdown.send(true);
        if let Some(rt) = self.tokio.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
