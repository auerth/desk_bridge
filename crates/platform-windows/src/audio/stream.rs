//! Shared plumbing for the capture and render worker threads.

use super::wasapi::{ComGuard, Event, Mmcss};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

/// Lifecycle notifications of a running stream (see `start_with_events`).
///
/// Order: `Started` once the stream runs, then (if the endpoint disappears) `DeviceLost`, and
/// always a final `Stopped` when the worker thread ends for any reason (handle dropped, device
/// lost, sink closed, internal error). A stream that fails to start only reports the error from
/// `start*` and sends no events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent {
    Started,
    /// The endpoint was invalidated (unplugged, powered off, driver reset). The stream ends; the
    /// caller restarts it when `DeviceEvent::Added` arrives. There is no silent fallback device.
    DeviceLost,
    Stopped,
}

/// Why a worker's run loop ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    /// The handle asked us to stop.
    Stopped,
    DeviceLost,
    /// The consumer dropped its receiver.
    SinkClosed,
    Error(String),
}

/// Owns the worker thread of a stream; dropping stops and joins it.
pub(crate) struct StreamThread {
    stop: Arc<Event>,
    join: Option<JoinHandle<()>>,
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Sends `Stopped` when dropped (also when the worker panics), if the stream had started.
struct LifecycleGuard {
    tx: Option<UnboundedSender<StreamEvent>>,
    started: bool,
}

impl LifecycleGuard {
    fn send(&self, ev: StreamEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(ev);
        }
    }
}

impl Drop for LifecycleGuard {
    fn drop(&mut self) {
        if self.started {
            self.send(StreamEvent::Stopped);
        }
    }
}

/// Spawns a stream worker.
///
/// `init` runs on the new thread (COM MTA, MMCSS "Pro Audio" already set up) and creates all COM
/// objects (`S`, not required to be `Send`) plus an info value `I` handed back to the caller.
/// Errors from `init` are returned from here. `run` then owns `S` until the stream ends, so all
/// COM objects are created, used and released on the same thread.
pub(crate) fn spawn_stream<I, S, Init, Run>(
    name: &str,
    lifecycle: Option<UnboundedSender<StreamEvent>>,
    init: Init,
    run: Run,
) -> Result<(StreamThread, I)>
where
    I: Send + 'static,
    Init: FnOnce() -> Result<(S, I)> + Send + 'static,
    Run: FnOnce(S, &Event) -> EndReason + Send + 'static,
{
    let stop = Arc::new(Event::new(true).map_err(super::wasapi::os_err)?);
    let stop_thread = stop.clone();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<I>>();

    let join = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            // Drop order (reverse of declaration): lifecycle guard, MMCSS, COM.
            let _com = ComGuard::init();
            let _mmcss = Mmcss::pro_audio();
            let mut guard = LifecycleGuard {
                tx: lifecycle,
                started: false,
            };

            let (state, info) = match init() {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            guard.started = true;
            guard.send(StreamEvent::Started);
            if ready_tx.send(Ok(info)).is_err() {
                return; // starter went away
            }
            let reason = run(state, &stop_thread);
            match &reason {
                EndReason::DeviceLost => {
                    tracing::warn!("audio stream ended: device lost");
                    guard.send(StreamEvent::DeviceLost);
                }
                EndReason::Error(msg) => tracing::warn!("audio stream ended with error: {msg}"),
                EndReason::SinkClosed => tracing::debug!("audio stream ended: sink closed"),
                EndReason::Stopped => {}
            }
        })
        .map_err(|e| PlatformError::other(format!("spawn audio thread: {e}")))?;

    let thread = StreamThread {
        stop,
        join: Some(join),
    };
    match ready_rx.recv() {
        Ok(Ok(info)) => Ok((thread, info)),
        Ok(Err(e)) => {
            thread.stop.set();
            Err(e)
        }
        Err(_) => Err(PlatformError::other("audio thread died during start")),
    }
}
