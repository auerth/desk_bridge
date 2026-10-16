//! Clipboard feature glue: wires `ClipboardSync` to the peer link.
//!
//! All potentially blocking clipboard work (OS calls, blocking fetches) runs on one dedicated
//! worker thread so the actor never stalls and clipboard events stay ordered.

use super::Engine;
use super::session::{PeerLink, uni};
use crate::config::Config;
use deskbridge_clipboard::{ClipboardSync, FetchError, Fetcher, SyncConfig};
use deskbridge_platform::clipboard::ClipboardEvent;
use deskbridge_proto::frame::{read_msg, write_msg};
use deskbridge_proto::pb;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc::unbounded_channel;

type Waiter = std_mpsc::SyncSender<Result<Vec<u8>, FetchError>>;
type Waiters = Arc<Mutex<HashMap<(u64, String), Waiter>>>;

pub(crate) enum ClipJob {
    Local(ClipboardEvent),
    Remote(pb::ClipboardOffer),
    Request(pb::ClipboardRequest),
    Config(SyncConfig),
    Stop,
}

pub(crate) struct ClipRuntime {
    pub sync: Arc<ClipboardSync>,
    jobs: std_mpsc::Sender<ClipJob>,
}

impl ClipRuntime {
    pub fn job(&self, j: ClipJob) {
        let _ = self.jobs.send(j);
    }
}

impl Drop for ClipRuntime {
    fn drop(&mut self) {
        let _ = self.jobs.send(ClipJob::Stop);
    }
}

struct PeerFetcher {
    link: PeerLink,
    waiters: Waiters,
}

impl Fetcher for PeerFetcher {
    fn fetch(&self, offer_id: u64, mime: &str, timeout: Duration) -> Result<Vec<u8>, FetchError> {
        let (tx, rx) = std_mpsc::sync_channel(1);
        let key = (offer_id, mime.to_string());
        self.waiters.lock().insert(key.clone(), tx);
        self.link.send_control(pb::control::Msg::ClipboardRequest(pb::ClipboardRequest {
            id: offer_id,
            mime: mime.to_string(),
        }));
        let res = rx.recv_timeout(timeout);
        self.waiters.lock().remove(&key);
        match res {
            Ok(r) => r,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(FetchError::Timeout),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(FetchError::Disconnected),
        }
    }
}

pub(crate) fn sync_config(cfg: &Config) -> SyncConfig {
    let c = &cfg.clipboard;
    SyncConfig {
        send: c.send,
        receive: c.receive,
        max_size_bytes: u64::from(c.max_size_mb) * 1024 * 1024,
        eager_limit_bytes: u64::from(c.eager_limit_kb) * 1024,
        sync_images: c.sync_images,
        sync_rich_text: c.sync_rich_text,
        sync_files: c.sync_files,
        respect_sensitive: c.respect_sensitive,
    }
}

impl Engine {
    pub(crate) fn start_clipboard(&mut self) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        let waiters: Waiters = Arc::default();
        let fetcher = Arc::new(PeerFetcher { link: link.clone(), waiters: waiters.clone() });
        let sync = Arc::new(ClipboardSync::new(self.platform.clipboard.clone(), fetcher, sync_config(&self.cfg)));
        sync.set_file_source(self.transfer_file_source(link.clone()));

        // inbound clipboard data streams
        let (utx, mut urx) = unbounded_channel::<quinn::RecvStream>();
        link.set_uni_sink(uni::CLIPBOARD_DATA, Some(utx));
        let w = waiters.clone();
        tokio::spawn(async move {
            while let Some(mut r) = urx.recv().await {
                let w = w.clone();
                tokio::spawn(async move {
                    let Ok(h) = read_msg::<_, pb::ClipboardDataHeader>(&mut r).await else { return };
                    let key = (h.id, h.mime.clone());
                    let result = if h.found {
                        let mut data = Vec::with_capacity(h.size.min(64 * 1024 * 1024) as usize);
                        match (&mut r).take(h.size).read_to_end(&mut data).await {
                            Ok(n) if n as u64 == h.size => Ok(data),
                            Ok(_) => Err(FetchError::Disconnected),
                            Err(e) => Err(FetchError::Other(e.to_string())),
                        }
                    } else {
                        Err(FetchError::NotFound)
                    };
                    if let Some(tx) = w.lock().remove(&key) {
                        let _ = tx.send(result);
                    }
                });
            }
        });

        // worker thread
        let (jtx, jrx) = std_mpsc::channel::<ClipJob>();
        let rt = tokio::runtime::Handle::current();
        let sync_w = sync.clone();
        let link_w = link.clone();
        std::thread::Builder::new()
            .name("clipboard-worker".into())
            .spawn(move || {
                while let Ok(job) = jrx.recv() {
                    match job {
                        ClipJob::Local(ev) => {
                            if let Some(offer) = sync_w.on_local_change(ev) {
                                link_w.send_control(pb::control::Msg::ClipboardOffer(offer));
                            }
                        }
                        ClipJob::Remote(offer) => sync_w.on_remote_offer(offer),
                        ClipJob::Config(c) => sync_w.set_config(c),
                        ClipJob::Request(req) => {
                            let data = sync_w.serve_request(&req);
                            let link = link_w.clone();
                            rt.block_on(async move {
                                let res: Result<(), deskbridge_net::NetError> = async {
                                    let mut s = link.open_uni(uni::CLIPBOARD_DATA).await?;
                                    let header = pb::ClipboardDataHeader {
                                        id: req.id,
                                        mime: req.mime.clone(),
                                        size: data.as_ref().map_or(0, |d| d.len() as u64),
                                        found: data.is_some(),
                                    };
                                    write_msg(&mut s, &header).await?;
                                    if let Some(d) = &data {
                                        s.write_all(d).await.map_err(|e| deskbridge_net::NetError::Io(e.into()))?;
                                    }
                                    let _ = s.finish();
                                    Ok(())
                                }
                                .await;
                                if let Err(e) = res {
                                    tracing::debug!("serving clipboard data failed: {e}");
                                }
                            });
                        }
                        ClipJob::Stop => break,
                    }
                }
            })
            .ok();

        // backend events
        let (etx, mut erx) = unbounded_channel::<ClipboardEvent>();
        if let Err(e) = self.platform.clipboard.start(etx) {
            tracing::warn!("clipboard backend failed to start: {e}");
        }
        let m = self.msg_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if m.send(super::Msg::Clipboard(ev)).is_err() {
                    break;
                }
            }
        });

        self.clip = Some(ClipRuntime { sync, jobs: jtx });
    }

    pub(crate) fn stop_clipboard(&mut self) {
        self.platform.clipboard.stop();
        if let Some(s) = &self.session {
            s.link.set_uni_sink(uni::CLIPBOARD_DATA, None);
        }
        self.clip = None;
    }

    pub(crate) fn on_clipboard_event(&mut self, ev: ClipboardEvent) {
        if let Some(c) = &self.clip {
            c.job(ClipJob::Local(ev));
        }
    }

    pub(crate) fn on_clipboard_control(&mut self, msg: pb::control::Msg) -> bool {
        use pb::control::Msg as M;
        let Some(c) = &self.clip else { return matches!(msg, M::ClipboardOffer(_) | M::ClipboardRequest(_) | M::ClipboardClear(_)) };
        match msg {
            M::ClipboardOffer(o) => c.job(ClipJob::Remote(o)),
            M::ClipboardRequest(r) => c.job(ClipJob::Request(r)),
            M::ClipboardClear(_) => {}
            _ => return false,
        }
        true
    }

    pub(crate) fn apply_clip_config(&mut self) {
        if let Some(c) = &self.clip {
            c.job(ClipJob::Config(sync_config(&self.cfg)));
        }
    }
}
