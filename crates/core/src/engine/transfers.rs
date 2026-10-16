//! File transfers: explicit sends ("Senden an"/drop zone), clipboard file lists (virtual files
//! streamed straight from the network, or a staged fallback), progress bookkeeping.

use super::Engine;
use super::session::{PeerLink, uni};
use crate::ipc::{TransferDto, UiEvent};
use deskbridge_clipboard::FileSource;
use deskbridge_files::manager::{Direction, TransferHandle, TransferManager};
use deskbridge_files::scan::PlannedFile;
use deskbridge_files::wire;
use deskbridge_platform::clipboard::VirtualFileProvider;
use deskbridge_proto::frame::read_msg;
use deskbridge_proto::pb;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc::unbounded_channel, oneshot};

/// Max parallel file streams per transfer.
const PARALLEL: usize = 4;

pub(crate) struct UpdateMeta {
    pub version: String,
    pub sha256: Vec<u8>,
}

struct Incoming {
    root: PathBuf,
    handle: TransferHandle,
    remaining: usize,
    failed: Option<String>,
    sem: Arc<Semaphore>,
    /// Staged fallback waits for this.
    done: Option<std_mpsc::Sender<bool>>,
    name: String,
    update: Option<UpdateMeta>,
    last_path: Option<PathBuf>,
}

struct Outgoing {
    plan: Vec<PlannedFile>,
    handle: TransferHandle,
}

#[derive(Default)]
pub(crate) struct Registry {
    incoming: Mutex<HashMap<u64, Incoming>>,
    outgoing: Mutex<HashMap<u64, Outgoing>>,
    virtual_waiters: Mutex<HashMap<(u64, u32), oneshot::Sender<quinn::RecvStream>>>,
}

pub(crate) struct Transfers {
    pub mgr: TransferManager,
    pub reg: Arc<Registry>,
}

impl Transfers {
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        Self {
            mgr: TransferManager::new(state_dir),
            reg: Arc::default(),
        }
    }
}

fn preamble(transfer_id: u64, index: u32) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[..8].copy_from_slice(&transfer_id.to_le_bytes());
    b[8..].copy_from_slice(&index.to_le_bytes());
    b
}

fn target_dir(cfg: &crate::config::Config) -> PathBuf {
    if cfg.files.target_dir.trim().is_empty() {
        deskbridge_files::default_target_dir()
    } else {
        PathBuf::from(&cfg.files.target_dir)
    }
}

impl Engine {
    pub(crate) fn start_transfers(&mut self) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        let (tx, mut rx) = unbounded_channel::<quinn::RecvStream>();
        link.set_uni_sink(uni::FILE, Some(tx));
        let reg = self.xfer.reg.clone();
        let pushes = self.pushes.clone();
        tokio::spawn(async move {
            while let Some(r) = rx.recv().await {
                let reg = reg.clone();
                let pushes = pushes.clone();
                tokio::spawn(handle_file_stream(r, reg, pushes));
            }
        });
    }

    pub(crate) fn stop_transfers(&mut self) {
        if let Some(s) = &self.session {
            s.link.set_uni_sink(uni::FILE, None);
        }
        let mut inc = self.xfer.reg.incoming.lock();
        for (_, i) in inc.drain() {
            i.handle.fail("Verbindung getrennt");
            if let Some(d) = i.done {
                let _ = d.send(false);
            }
        }
        for (_, o) in self.xfer.reg.outgoing.lock().drain() {
            o.handle.fail("Verbindung getrennt");
        }
        self.xfer.reg.virtual_waiters.lock().clear();
    }

    /// UI: send files/folders to the peer.
    pub(crate) fn send_files(&mut self, paths: Vec<String>) -> Result<Value, String> {
        self.send_files_with_hint(paths, String::new())
    }

    pub(crate) fn send_files_with_hint(
        &mut self,
        paths: Vec<String>,
        hint: String,
    ) -> Result<Value, String> {
        let Some(s) = &self.session else {
            return Err("Nicht verbunden".into());
        };
        let link = s.link.clone();
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let plan = deskbridge_files::scan::plan_with_warnings(&paths)
            .map_err(|e| format!("Dateien nicht lesbar: {e}"))?;
        if plan.files.is_empty() {
            return Err("Keine Dateien ausgewählt".into());
        }
        for w in &plan.warnings {
            tracing::warn!("{w}");
        }
        let total = plan.total_bytes();
        let entries: Vec<pb::FileEntry> = plan.files.iter().map(|f| f.entry.clone()).collect();
        let id = self.xfer.mgr.next_id();
        let name = display_name(&entries);
        let handle = self.xfer.mgr.begin(
            id,
            Direction::Send,
            name,
            entries.iter().filter(|e| !e.is_dir).count() as u32,
            total,
            None,
        );
        self.xfer.reg.outgoing.lock().insert(
            id,
            Outgoing {
                plan: plan.files,
                handle,
            },
        );
        link.send_control(pb::control::Msg::SendFiles(pb::SendFiles {
            transfer_id: id,
            files: entries,
            target_hint: hint,
        }));
        self.dirty = true;
        Ok(json!({ "transfer_id": id }))
    }

    /// Peer announces files it wants to send us: pull them all.
    pub(crate) fn on_send_files(&mut self, sf: pb::SendFiles) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        if sf.files.iter().any(|f| {
            deskbridge_files::sanitize::safe_join(std::path::Path::new("x"), &f.rel_path).is_err()
        }) {
            tracing::warn!("SendFiles with unsafe paths rejected");
            return;
        }
        let update_meta = self.update_meta_from_hint(&sf.target_hint);
        let root = if update_meta.is_some() {
            self.paths.data_dir.join("Updates")
        } else {
            target_dir(&self.cfg)
        };
        let _ = std::fs::create_dir_all(&root);
        let total: u64 = sf.files.iter().map(|f| f.size).sum();
        let name = display_name(&sf.files);
        let handle = self.xfer.mgr.begin(
            sf.transfer_id,
            Direction::Receive,
            name.clone(),
            sf.files.iter().filter(|f| !f.is_dir).count() as u32,
            total,
            Some(root.to_string_lossy().into_owned()),
        );
        let sem = Arc::new(Semaphore::new(PARALLEL));
        self.xfer.reg.incoming.lock().insert(
            sf.transfer_id,
            Incoming {
                root: root.clone(),
                handle,
                remaining: sf.files.len(),
                failed: None,
                sem: sem.clone(),
                done: None,
                name,
                update: update_meta,
                last_path: None,
            },
        );
        let id = sf.transfer_id;
        let files = sf.files;
        tokio::spawn(async move {
            for (i, e) in files.iter().enumerate() {
                let Ok(p) = sem.clone().acquire_owned().await else {
                    break;
                };
                p.forget();
                let offset = wire::resume_offset(&root, e);
                link.send_control(pb::control::Msg::FileRequest(pb::FileRequest {
                    transfer_id: id,
                    index: i as u32,
                    offset,
                }));
            }
        });
        self.dirty = true;
    }

    /// Peer wants one of our files.
    pub(crate) fn on_file_request(&mut self, req: pb::FileRequest) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        let (planned, handle) = {
            let out = self.xfer.reg.outgoing.lock();
            match out.get(&req.transfer_id) {
                Some(o) => (
                    o.plan.get(req.index as usize).cloned(),
                    Some(o.handle.clone()),
                ),
                None => (
                    self.clip
                        .as_ref()
                        .and_then(|c| c.sync.local_files(req.transfer_id))
                        .and_then(|v| v.get(req.index as usize).cloned()),
                    None,
                ),
            }
        };
        let Some(planned) = planned else {
            tracing::debug!(
                id = req.transfer_id,
                idx = req.index,
                "file request for unknown transfer"
            );
            return;
        };
        tokio::spawn(async move {
            let res: Result<(), String> = async {
                let mut s = link.open_uni(uni::FILE).await.map_err(|e| e.to_string())?;
                s.write_all(&preamble(req.transfer_id, req.index))
                    .await
                    .map_err(|e| e.to_string())?;
                let mut prog = |n: u64| {
                    if let Some(h) = &handle {
                        h.add_bytes(n);
                    }
                };
                wire::send_file(
                    &mut s,
                    req.transfer_id,
                    req.index,
                    &planned,
                    req.offset,
                    &mut prog,
                )
                .await
                .map_err(|e| e.to_string())?;
                let _ = s.finish();
                // Let the data drain before the stream object goes away.
                let _ = tokio::time::timeout(Duration::from_secs(30), s.stopped()).await;
                Ok(())
            }
            .await;
            if let Err(e) = res {
                tracing::warn!("sending file failed: {e}");
                if let Some(h) = handle {
                    h.fail(e);
                }
            }
        });
    }

    /// Marks outgoing transfers finished once the peer reports completion.
    pub(crate) fn on_transfer_progress(&mut self, p: pb::TransferProgress) {
        if (p.finished || p.failed)
            && let Some(o) = self.xfer.reg.outgoing.lock().remove(&p.transfer_id)
        {
            if p.failed {
                o.handle.fail("Fehler beim Empfänger");
            } else {
                o.handle.finish();
            }
            self.dirty = true;
        }
    }

    pub(crate) fn transfers_dto(&self) -> Vec<TransferDto> {
        self.xfer
            .mgr
            .snapshot()
            .into_iter()
            .map(|t| TransferDto {
                id: t.id,
                direction: t.direction,
                name: t.name,
                files: t.files,
                total_bytes: t.total_bytes,
                done_bytes: t.done_bytes,
                status: t.status,
                path: t.path,
                started_unix_ms: t.started_unix_ms,
            })
            .collect()
    }

    pub(crate) fn has_running_transfers(&self) -> bool {
        self.xfer
            .mgr
            .snapshot()
            .iter()
            .any(|t| t.status == "running")
    }

    pub(crate) fn open_incoming_folder(&self) -> Result<Value, String> {
        let dir = target_dir(&self.cfg);
        let _ = std::fs::create_dir_all(&dir);
        open_in_explorer(&dir, false);
        Ok(json!({ "ok": true }))
    }

    pub(crate) fn show_transfer(&self, id: u64) -> Result<Value, String> {
        let t = self
            .xfer
            .mgr
            .snapshot()
            .into_iter()
            .find(|t| t.id == id)
            .ok_or("Unbekannter Transfer")?;
        if let Some(p) = t.path {
            open_in_explorer(&PathBuf::from(p), false);
        }
        Ok(json!({ "ok": true }))
    }

    pub(crate) fn transfer_file_source(&self, link: PeerLink) -> Arc<dyn FileSource> {
        Arc::new(NetFileSource {
            link,
            reg: self.xfer.reg.clone(),
            mgr: self.xfer.mgr.clone(),
            rt: tokio::runtime::Handle::current(),
            staging: self.paths.incoming_dir(),
        })
    }
}

fn display_name(entries: &[pb::FileEntry]) -> String {
    let top: Vec<&str> = entries
        .iter()
        .filter(|e| !e.rel_path.contains('/'))
        .map(|e| e.rel_path.as_str())
        .collect();
    match top.as_slice() {
        [one] => (*one).to_string(),
        [first, rest @ ..] => format!("{first} und {} weitere", rest.len()),
        [] => entries
            .first()
            .map_or_else(|| "Dateien".to_string(), |e| e.rel_path.clone()),
    }
}

fn open_in_explorer(path: &std::path::Path, select: bool) {
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("explorer.exe");
        if select {
            cmd.arg(format!("/select,{}", path.display()));
        } else {
            cmd.arg(path);
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = (path, select);
    }
}

/// Dispatches one incoming file stream: virtual-file reader or disk receiver.
async fn handle_file_stream(
    mut r: quinn::RecvStream,
    reg: Arc<Registry>,
    pushes: tokio::sync::broadcast::Sender<crate::ipc::Push>,
) {
    let mut pre = [0u8; 12];
    if r.read_exact(&mut pre).await.is_err() {
        return;
    }
    let id = u64::from_le_bytes(pre[..8].try_into().expect("8 bytes"));
    let index = u32::from_le_bytes(pre[8..].try_into().expect("4 bytes"));
    if let Some(w) = reg.virtual_waiters.lock().remove(&(id, index)) {
        let _ = w.send(r);
        return;
    }
    let (root, handle) = {
        let inc = reg.incoming.lock();
        match inc.get(&id) {
            Some(i) => (i.root.clone(), i.handle.clone()),
            None => {
                tracing::debug!(id, "file stream for unknown transfer");
                return;
            }
        }
    };
    let h2 = handle.clone();
    let mut prog = move |n: u64| h2.add_bytes(n);
    let res = wire::receive_file(&mut r, &root, &mut prog).await;
    let mut finished = None;
    {
        let mut inc = reg.incoming.lock();
        if let Some(i) = inc.get_mut(&id) {
            i.sem.add_permits(1);
            i.remaining = i.remaining.saturating_sub(1);
            if let Ok(rf) = &res {
                i.last_path = Some(rf.path.clone());
            }
            if let Err(e) = &res {
                i.failed.get_or_insert_with(|| e.to_string());
            }
            if i.remaining == 0 {
                finished = inc.remove(&id);
            }
        }
    }
    if let Some(i) = finished {
        match &i.failed {
            None => {
                i.handle.finish();
                if let (Some(meta), Some(path)) = (&i.update, &i.last_path) {
                    let ok = super::update::sha256_file(path).is_ok_and(|h| h == meta.sha256);
                    let event = if ok {
                        UiEvent::PartnerUpdateReady {
                            path: path.to_string_lossy().into_owned(),
                            version: meta.version.clone(),
                        }
                    } else {
                        let _ = std::fs::remove_file(path);
                        UiEvent::Notification {
                            level: "error".into(),
                            text: "Das empfangene Update ist beschädigt und wurde verworfen."
                                .into(),
                        }
                    };
                    let _ = pushes.send(crate::ipc::Push::Event { event });
                } else {
                    let _ = pushes.send(crate::ipc::Push::Event {
                        event: UiEvent::Notification {
                            level: "info".into(),
                            text: format!("Empfangen: {}", i.name),
                        },
                    });
                }
            }
            Some(e) => {
                i.handle.fail(e.clone());
            }
        }
        if let Some(d) = i.done {
            let _ = d.send(i.failed.is_none());
        }
    }
}

/// Plugs the transfer engine into clipboard file lists.
struct NetFileSource {
    link: PeerLink,
    reg: Arc<Registry>,
    mgr: TransferManager,
    rt: tokio::runtime::Handle,
    staging: PathBuf,
}

impl FileSource for NetFileSource {
    fn provider(
        &self,
        offer_id: u64,
        files: &[pb::FileEntry],
    ) -> Option<Arc<dyn VirtualFileProvider>> {
        let total: u64 = files.iter().map(|f| f.size).sum();
        let count = files.iter().filter(|f| !f.is_dir).count();
        let handle = self.mgr.begin(
            offer_id,
            Direction::Receive,
            display_name(files),
            count as u32,
            total,
            None,
        );
        Some(Arc::new(NetProvider {
            link: self.link.clone(),
            reg: self.reg.clone(),
            rt: self.rt.clone(),
            id: offer_id,
            handle,
            remaining: Arc::new(AtomicUsize::new(count)),
            files: files.to_vec(),
        }))
    }

    fn staged(&self, offer_id: u64, files: &[pb::FileEntry]) -> Option<Vec<PathBuf>> {
        let root = self.staging.join(offer_id.to_string());
        std::fs::create_dir_all(&root).ok()?;
        let total: u64 = files.iter().map(|f| f.size).sum();
        let handle = self.mgr.begin(
            offer_id,
            Direction::Receive,
            display_name(files),
            files.iter().filter(|f| !f.is_dir).count() as u32,
            total,
            Some(root.to_string_lossy().into_owned()),
        );
        let (dtx, drx) = std_mpsc::channel();
        let sem = Arc::new(Semaphore::new(PARALLEL));
        self.reg.incoming.lock().insert(
            offer_id,
            Incoming {
                root: root.clone(),
                handle,
                remaining: files.len(),
                failed: None,
                sem: sem.clone(),
                done: Some(dtx),
                name: display_name(files),
                update: None,
                last_path: None,
            },
        );
        let link = self.link.clone();
        let list = files.to_vec();
        self.rt.spawn(async move {
            for i in 0..list.len() {
                let Ok(p) = sem.clone().acquire_owned().await else {
                    break;
                };
                p.forget();
                link.send_control(pb::control::Msg::FileRequest(pb::FileRequest {
                    transfer_id: offer_id,
                    index: i as u32,
                    offset: 0,
                }));
            }
        });
        // Wait (generously) for completion; Explorer pastes after this returns.
        match drx.recv_timeout(Duration::from_secs(6 * 3600)) {
            Ok(true) => {}
            _ => return None,
        }
        let mut tops: Vec<String> = files
            .iter()
            .map(|f| f.rel_path.split('/').next().unwrap_or("").to_string())
            .collect();
        tops.sort();
        tops.dedup();
        Some(
            tops.into_iter()
                .filter(|t| !t.is_empty())
                .map(|t| root.join(t))
                .collect(),
        )
    }
}

struct NetProvider {
    link: PeerLink,
    reg: Arc<Registry>,
    rt: tokio::runtime::Handle,
    id: u64,
    handle: TransferHandle,
    remaining: Arc<AtomicUsize>,
    files: Vec<pb::FileEntry>,
}

impl VirtualFileProvider for NetProvider {
    fn open(&self, index: usize) -> deskbridge_platform::Result<Box<dyn Read + Send>> {
        let entry = self
            .files
            .get(index)
            .ok_or_else(|| deskbridge_platform::PlatformError::other("Unbekannter Dateiindex"))?;
        if entry.is_dir {
            return Ok(Box::new(std::io::empty()));
        }
        let (tx, rx) = oneshot::channel();
        self.reg
            .virtual_waiters
            .lock()
            .insert((self.id, index as u32), tx);
        self.link
            .send_control(pb::control::Msg::FileRequest(pb::FileRequest {
                transfer_id: self.id,
                index: index as u32,
                offset: 0,
            }));
        let stream = self
            .rt
            .block_on(tokio::time::timeout(Duration::from_secs(30), rx))
            .map_err(|_| {
                deskbridge_platform::PlatformError::other("Zeitüberschreitung beim Abruf der Datei")
            })?
            .map_err(|_| deskbridge_platform::PlatformError::other("Verbindung getrennt"))?;
        Ok(Box::new(StreamingReader {
            rt: self.rt.clone(),
            r: stream,
            state: ReadState::Start,
            hasher: blake3::Hasher::new(),
            buf: Vec::new(),
            pos: 0,
            handle: self.handle.clone(),
            remaining: self.remaining.clone(),
            counted: false,
        }))
    }
}

enum ReadState {
    Start,
    Data,
    Done,
}

/// Blocking `Read` over a file stream (header, chunk frames, trailer), verifying BLAKE3.
struct StreamingReader {
    rt: tokio::runtime::Handle,
    r: quinn::RecvStream,
    state: ReadState,
    hasher: blake3::Hasher,
    buf: Vec<u8>,
    pos: usize,
    handle: TransferHandle,
    remaining: Arc<AtomicUsize>,
    counted: bool,
}

impl StreamingReader {
    fn finish_one(&mut self, ok: bool, why: &str) {
        if self.counted {
            return;
        }
        self.counted = true;
        if !ok {
            self.handle.fail(why.to_string());
        } else if self.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.handle.finish();
        }
    }

    async fn next_chunk(&mut self) -> std::io::Result<bool> {
        let mut len = [0u8; 4];
        self.r
            .read_exact(&mut len)
            .await
            .map_err(std::io::Error::other)?;
        let n = u32::from_le_bytes(len) as usize;
        if n == 0 {
            let trailer: pb::FileTrailer =
                read_msg(&mut self.r).await.map_err(std::io::Error::other)?;
            let actual = self.hasher.finalize();
            if !trailer.ok || trailer.blake3.as_slice() != actual.as_bytes() {
                return Err(std::io::Error::other("Prüfsumme stimmt nicht überein"));
            }
            return Ok(false);
        }
        if n > deskbridge_files::CHUNK {
            return Err(std::io::Error::other("Ungültige Blockgröße"));
        }
        self.buf.resize(n, 0);
        self.pos = 0;
        self.r
            .read_exact(&mut self.buf)
            .await
            .map_err(std::io::Error::other)?;
        self.hasher.update(&self.buf);
        self.handle.add_bytes(n as u64);
        Ok(true)
    }
}

impl Read for StreamingReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.pos < self.buf.len() {
                let n = out.len().min(self.buf.len() - self.pos);
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            match self.state {
                ReadState::Done => return Ok(0),
                ReadState::Start => {
                    let rt = self.rt.clone();
                    let res =
                        rt.block_on(async { read_msg::<_, pb::FileHeader>(&mut self.r).await });
                    if let Err(e) = res {
                        self.finish_one(false, "Dateikopf ungültig");
                        return Err(std::io::Error::other(e));
                    }
                    self.state = ReadState::Data;
                }
                ReadState::Data => {
                    let rt = self.rt.clone();
                    match rt.block_on(self.next_chunk()) {
                        Ok(true) => {}
                        Ok(false) => {
                            self.state = ReadState::Done;
                            self.finish_one(true, "");
                            return Ok(0);
                        }
                        Err(e) => {
                            self.state = ReadState::Done;
                            self.finish_one(false, &e.to_string());
                            return Err(e);
                        }
                    }
                }
            }
        }
    }
}
