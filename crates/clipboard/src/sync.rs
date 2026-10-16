//! The clipboard sync state machine.

use crate::{FetchError, Fetcher, FileSource, SyncConfig, fetch_timeout};
use deskbridge_files::sanitize::safe_join;
use deskbridge_files::scan::{self, PlannedFile};
use deskbridge_platform::PlatformError;
use deskbridge_platform::clipboard::{
    ClipFormatInfo, ClipboardBackend, ClipboardEvent, ClipboardProvider, FileOffer, mime,
};
use deskbridge_proto::pb;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a write of ours is remembered to swallow the backend's change notification.
const ECHO_WINDOW: Duration = Duration::from_secs(2);
/// Inline text must fit comfortably into one control frame (`MAX_FRAME` is 8 MiB).
const MAX_INLINE_BYTES: u64 = 4 * 1024 * 1024;

/// What we wrote to the local clipboard, to recognise the resulting change event.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fingerprint {
    /// Hash of the plain text.
    Text(u64),
    /// Sorted MIME types of a delayed-rendering registration.
    Formats(Vec<String>),
    /// A file list (virtual or staged).
    Files,
}

fn text_hash(bytes: &[u8]) -> u64 {
    let h = blake3::hash(bytes);
    u64::from_le_bytes(h.as_bytes()[..8].try_into().expect("8 bytes"))
}

fn sorted_mimes<'a>(it: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut v: Vec<String> = it.map(str::to_owned).collect();
    v.sort();
    v.dedup();
    v
}

struct LocalOffer {
    id: u64,
    mimes: Vec<String>,
    files: Vec<PlannedFile>,
}

struct State {
    cfg: SyncConfig,
    current: Option<LocalOffer>,
    guard: Vec<(Fingerprint, Instant)>,
    last_skip: Option<String>,
    /// Ticket of the newest local change event; stale planners must not install their offer.
    ticket: u64,
}

impl State {
    fn skip(&mut self, reason: String) {
        tracing::warn!("Zwischenablage nicht synchronisiert: {reason}");
        self.last_skip = Some(reason);
    }

    fn remember_write(&mut self, fps: impl IntoIterator<Item = Fingerprint>) {
        let now = Instant::now();
        self.guard
            .retain(|(_, t)| now.duration_since(*t) < ECHO_WINDOW);
        for fp in fps {
            self.guard.push((fp, now));
        }
    }

    fn is_echo(&mut self, text: Option<&str>, formats: &[ClipFormatInfo], has_files: bool) -> bool {
        let now = Instant::now();
        self.guard
            .retain(|(_, t)| now.duration_since(*t) < ECHO_WINDOW);
        if self.guard.is_empty() {
            return false;
        }
        // With text we compare the text itself; comparing only the format list could swallow a
        // genuinely new copy of a different text.
        let probe = if has_files {
            Fingerprint::Files
        } else if let Some(t) = text {
            Fingerprint::Text(text_hash(t.as_bytes()))
        } else {
            Fingerprint::Formats(sorted_mimes(formats.iter().map(|f| f.mime.as_str())))
        };
        self.guard.iter().any(|(fp, _)| *fp == probe)
    }
}

/// Synchronisation policy for one connection. All methods take `&self` and are thread-safe.
pub struct ClipboardSync {
    backend: Arc<dyn ClipboardBackend>,
    fetcher: Arc<dyn Fetcher>,
    files: Mutex<Option<Arc<dyn FileSource>>>,
    state: Mutex<State>,
    /// Serialises writes of remote offers to the local clipboard.
    write_lock: Mutex<()>,
    /// A file offer from the peer that waits until the user arrives on this device.
    pending_files: Mutex<Option<pb::ClipboardOffer>>,
    next_id: AtomicU64,
}

fn allowed(cfg: &SyncConfig, m: &str) -> bool {
    match m {
        mime::TEXT => true,
        mime::HTML | mime::RTF | mime::URI_LIST => cfg.sync_rich_text,
        mime::PNG => cfg.sync_images,
        _ => false,
    }
}

fn is_supported(m: &str) -> bool {
    matches!(
        m,
        mime::TEXT | mime::HTML | mime::RTF | mime::PNG | mime::URI_LIST
    )
}

fn add_format(
    kept: &mut Vec<ClipFormatInfo>,
    oversized: &mut Option<u64>,
    max: u64,
    m: &str,
    size: u64,
) {
    if kept.iter().any(|k| k.mime == m) {
        return;
    }
    if size > max {
        *oversized = Some(oversized.map_or(size, |o| o.max(size)));
    } else {
        kept.push(ClipFormatInfo {
            mime: m.to_owned(),
            size,
        });
    }
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
}

impl ClipboardSync {
    #[must_use]
    pub fn new(
        backend: Arc<dyn ClipboardBackend>,
        fetcher: Arc<dyn Fetcher>,
        cfg: SyncConfig,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self {
            backend,
            fetcher,
            files: Mutex::new(None),
            state: Mutex::new(State {
                cfg,
                current: None,
                guard: Vec::new(),
                last_skip: None,
                ticket: 0,
            }),
            write_lock: Mutex::new(()),
            pending_files: Mutex::new(None),
            // Ids stay unique across restarts without any persistence.
            next_id: AtomicU64::new((now_ms << 12) | 1),
        }
    }

    /// Plugs in the file transfer engine for clipboard file lists. Without it, file lists
    /// (incoming) are ignored.
    pub fn set_file_source(&self, src: Arc<dyn FileSource>) {
        *self.files.lock() = Some(src);
    }

    pub fn set_config(&self, cfg: SyncConfig) {
        let mut st = self.state.lock();
        if !cfg.send {
            st.current = None;
        }
        st.cfg = cfg;
    }

    #[must_use]
    pub fn config(&self) -> SyncConfig {
        self.state.lock().cfg.clone()
    }

    /// Why the most recent clipboard content was not synchronised (German, for the UI). Cleared
    /// by the next successful sync in either direction.
    #[must_use]
    pub fn last_skip_reason(&self) -> Option<String> {
        self.state.lock().last_skip.clone()
    }

    #[must_use]
    pub fn current_local_offer_id(&self) -> Option<u64> {
        self.state.lock().current.as_ref().map(|c| c.id)
    }

    /// The planned files of our current local offer, for serving the peer's `FileRequest`s.
    #[must_use]
    pub fn local_files(&self, offer_id: u64) -> Option<Vec<PlannedFile>> {
        let st = self.state.lock();
        st.current
            .as_ref()
            .filter(|c| c.id == offer_id && !c.files.is_empty())
            .map(|c| c.files.clone())
    }

    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // ---------------------------------------------------------------- local -> peer

    /// A local clipboard change from the backend. Returns the offer for the peer, or `None`
    /// (disabled, sensitive, our own echo, too big, nothing syncable). The reason is available
    /// from [`Self::last_skip_reason`].
    pub fn on_local_change(&self, ev: ClipboardEvent) -> Option<pb::ClipboardOffer> {
        let ClipboardEvent::Changed {
            formats,
            sensitive,
            text,
            files,
        } = ev;

        let (cfg, ticket) = {
            let mut st = self.state.lock();
            if st.is_echo(text.as_deref(), &formats, !files.is_empty()) {
                return None;
            }
            // Whatever is on the clipboard now, older offers are stale.
            st.current = None;
            *self.pending_files.lock() = None;
            st.ticket += 1;
            if sensitive && st.cfg.respect_sensitive {
                st.skip(
                    "Sensibler Inhalt (z. B. Passwort-Manager) wird nicht synchronisiert".into(),
                );
                return None;
            }
            if !st.cfg.send {
                st.skip("Senden der Zwischenablage ist deaktiviert".into());
                return None;
            }
            (st.cfg.clone(), st.ticket)
        };

        let text = text.filter(|t| !t.is_empty());
        let mut kept: Vec<ClipFormatInfo> = Vec::new();
        let mut oversized: Option<u64> = None;
        let mut config_filtered = false;
        for f in &formats {
            if !is_supported(&f.mime) {
                continue;
            }
            if !allowed(&cfg, &f.mime) {
                config_filtered = true;
                continue;
            }
            if f.mime == mime::URI_LIST && !files.is_empty() {
                continue; // local paths mean nothing to the peer; the file list replaces it
            }
            if f.mime == mime::TEXT {
                match &text {
                    Some(t) => add_format(
                        &mut kept,
                        &mut oversized,
                        cfg.max_size_bytes,
                        mime::TEXT,
                        t.len() as u64,
                    ),
                    None => add_format(
                        &mut kept,
                        &mut oversized,
                        cfg.max_size_bytes,
                        mime::TEXT,
                        f.size,
                    ),
                }
            } else {
                add_format(
                    &mut kept,
                    &mut oversized,
                    cfg.max_size_bytes,
                    &f.mime,
                    f.size,
                );
            }
        }
        if let Some(t) = &text
            && !kept.iter().any(|k| k.mime == mime::TEXT)
            && oversized.is_none()
        {
            add_format(
                &mut kept,
                &mut oversized,
                cfg.max_size_bytes,
                mime::TEXT,
                t.len() as u64,
            );
        }

        // File lists replace all other formats.
        let mut planned: Vec<PlannedFile> = Vec::new();
        if !files.is_empty() {
            if cfg.sync_files {
                match scan::plan_with_warnings(&files) {
                    Ok(p) => {
                        for w in &p.warnings {
                            tracing::warn!("{w}");
                        }
                        planned = p.files;
                    }
                    Err(e) => {
                        let mut st = self.state.lock();
                        st.skip(format!("Dateien nicht lesbar: {e}"));
                        return None;
                    }
                }
                if planned.is_empty() {
                    self.state
                        .lock()
                        .skip("Keine lesbaren Dateien in der Zwischenablage".into());
                    return None;
                }
                kept.clear();
            } else {
                config_filtered = true;
            }
        }

        if kept.is_empty() && planned.is_empty() {
            let reason = if let Some(o) = oversized {
                format!(
                    "Inhalt zu groß ({} > Limit {})",
                    mib(o),
                    mib(cfg.max_size_bytes)
                )
            } else if config_filtered {
                "Die kopierten Formate sind in den Einstellungen deaktiviert".to_string()
            } else {
                "Kein synchronisierbares Format in der Zwischenablage".to_string()
            };
            self.state.lock().skip(reason);
            return None;
        }

        // Eager text.
        let eager_limit = cfg.eager_limit_bytes.min(MAX_INLINE_BYTES);
        let mut inline_text = Vec::new();
        if let Some(tf) = kept.iter().find(|k| k.mime == mime::TEXT) {
            match &text {
                Some(t) if (t.len() as u64) < eager_limit => inline_text = t.as_bytes().to_vec(),
                None if tf.size > 0 && tf.size < eager_limit => {
                    if let Ok(Some(data)) = self.backend.read(mime::TEXT)
                        && (data.len() as u64) < eager_limit
                    {
                        inline_text = data;
                    }
                }
                _ => {}
            }
        }

        let id = self.new_id();
        let offer = pb::ClipboardOffer {
            id,
            formats: kept
                .iter()
                .map(|k| pb::ClipFormat {
                    mime: k.mime.clone(),
                    size: k.size,
                })
                .collect(),
            files: planned.iter().map(|p| p.entry.clone()).collect(),
            inline_text,
            sensitive: false,
        };

        let mut st = self.state.lock();
        if st.ticket != ticket {
            // A newer change event overtook us; its result wins.
            return None;
        }
        st.current = Some(LocalOffer {
            id,
            mimes: kept.iter().map(|k| k.mime.clone()).collect(),
            files: planned,
        });
        st.last_skip = None;
        Some(offer)
    }

    /// The peer asks for the data of one of our offers. Returns `None` if that offer is no longer
    /// current (the clipboard changed), the format was not offered, or the backend has no data.
    pub fn serve_request(&self, req: &pb::ClipboardRequest) -> Option<Vec<u8>> {
        let max = {
            let st = self.state.lock();
            let cur = st.current.as_ref()?;
            if !st.cfg.send || cur.id != req.id || !cur.mimes.contains(&req.mime) {
                tracing::debug!(
                    "Anfrage {} für {} verworfen (veraltet oder nicht angeboten)",
                    req.id,
                    req.mime
                );
                return None;
            }
            st.cfg.max_size_bytes
        };
        let data = match self.backend.read(&req.mime) {
            Ok(Some(d)) => d,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!("Zwischenablage lesen fehlgeschlagen: {e}");
                return None;
            }
        };
        if data.len() as u64 > max {
            self.state.lock().skip(format!(
                "Inhalt zu groß ({} > Limit {})",
                mib(data.len() as u64),
                mib(max)
            ));
            return None;
        }
        // The clipboard may have changed while we were reading.
        if self.current_local_offer_id() != Some(req.id) {
            return None;
        }
        Some(data)
    }

    // ---------------------------------------------------------------- peer -> local

    /// The peer's offer arrived: eager text is written immediately, other formats are registered
    /// for delayed rendering, file lists go through the [`FileSource`].
    pub fn on_remote_offer(&self, offer: pb::ClipboardOffer) {
        let cfg = {
            let mut st = self.state.lock();
            if !st.cfg.receive {
                st.skip("Empfang der Zwischenablage ist deaktiviert".into());
                return;
            }
            if offer.sensitive {
                st.skip("Sensibler Inhalt wird nicht übernommen".into());
                return;
            }
            st.cfg.clone()
        };
        let _w = self.write_lock.lock();

        if !offer.files.is_empty() {
            if cfg.sync_files && cfg.files_on_arrival {
                // Keep the user's own clipboard until they actually come over.
                *self.pending_files.lock() = Some(offer);
            } else {
                self.apply_remote_files(&offer, &cfg);
            }
            return;
        }
        // Newer content replaces a file offer that was still waiting.
        *self.pending_files.lock() = None;

        // Filter by our own settings and size limit.
        let mut kept: Vec<pb::ClipFormat> = Vec::new();
        let mut oversized: Option<u64> = None;
        let mut config_filtered = false;
        for f in &offer.formats {
            if !is_supported(&f.mime) {
                continue;
            }
            if !allowed(&cfg, &f.mime) {
                config_filtered = true;
                continue;
            }
            if f.size > cfg.max_size_bytes {
                oversized = Some(oversized.map_or(f.size, |o| o.max(f.size)));
                continue;
            }
            if !kept.iter().any(|k| k.mime == f.mime) {
                kept.push(f.clone());
            }
        }
        let inline = (!offer.inline_text.is_empty()
            && offer.inline_text.len() as u64 <= cfg.max_size_bytes)
            .then(|| offer.inline_text.clone());
        if inline.is_some() && !kept.iter().any(|k| k.mime == mime::TEXT) && oversized.is_none() {
            kept.push(pb::ClipFormat {
                mime: mime::TEXT.into(),
                size: offer.inline_text.len() as u64,
            });
        }
        if kept.is_empty() {
            let reason = if let Some(o) = oversized {
                format!(
                    "Inhalt des Partners zu groß ({} > Limit {})",
                    mib(o),
                    mib(cfg.max_size_bytes)
                )
            } else if config_filtered {
                "Die Formate des Partners sind in den Einstellungen deaktiviert".to_string()
            } else {
                "Angebot des Partners enthält kein unterstütztes Format".to_string()
            };
            self.state.lock().skip(reason);
            return;
        }

        let eager_only_text = kept.len() == 1 && kept[0].mime == mime::TEXT && inline.is_some();
        let result = if eager_only_text {
            let text = String::from_utf8_lossy(inline.as_deref().unwrap_or_default()).into_owned();
            self.state
                .lock()
                .remember_write([Fingerprint::Text(text_hash(text.as_bytes()))]);
            self.backend.set_text(&text)
        } else {
            let mut fps = vec![Fingerprint::Formats(sorted_mimes(
                kept.iter().map(|k| k.mime.as_str()),
            ))];
            if let Some(i) = &inline {
                fps.push(Fingerprint::Text(text_hash(i)));
            }
            self.state.lock().remember_write(fps);
            let provider = Arc::new(RemoteProvider {
                fetcher: self.fetcher.clone(),
                offer_id: offer.id,
                inline,
                sizes: kept.iter().map(|k| (k.mime.clone(), k.size)).collect(),
                max: cfg.max_size_bytes,
            });
            let infos = kept
                .iter()
                .map(|k| ClipFormatInfo {
                    mime: k.mime.clone(),
                    size: k.size,
                })
                .collect();
            self.backend.offer(infos, provider)
        };
        self.finish_remote_write(result);
    }

    /// Puts the file offer that was held back on the clipboard (the user arrived on this device).
    pub fn claim_pending_files(&self) {
        let _w = self.write_lock.lock();
        let Some(offer) = self.pending_files.lock().take() else {
            return;
        };
        let cfg = self.state.lock().cfg.clone();
        self.apply_remote_files(&offer, &cfg);
    }

    fn apply_remote_files(&self, offer: &pb::ClipboardOffer, cfg: &SyncConfig) {
        if !cfg.sync_files {
            self.state
                .lock()
                .skip("Dateiübertragung über die Zwischenablage ist deaktiviert".into());
            return;
        }
        let Some(src) = self.files.lock().clone() else {
            self.state
                .lock()
                .skip("Dateiübertragung ist nicht verfügbar".into());
            return;
        };
        // Defence in depth: never even offer paths the receiver would refuse to write.
        if let Some(bad) = offer
            .files
            .iter()
            .find(|f| safe_join(Path::new("x"), &f.rel_path).is_err())
        {
            self.state.lock().skip(format!(
                "Ungültige Dateiliste des Partners ({:?})",
                bad.rel_path
            ));
            return;
        }
        self.state.lock().remember_write([Fingerprint::Files]);
        let result = if let Some(p) = src.provider(offer.id, &offer.files) {
            let offers = offer
                .files
                .iter()
                .map(|f| FileOffer {
                    rel_path: f.rel_path.clone(),
                    size: f.size,
                    mtime_unix_ms: f.mtime_unix_ms,
                    is_dir: f.is_dir,
                })
                .collect();
            self.backend.offer_files(offers, p)
        } else if let Some(paths) = src.staged(offer.id, &offer.files) {
            self.backend.offer_local_files(paths)
        } else {
            self.state
                .lock()
                .skip("Dateien des Partners konnten nicht bereitgestellt werden".into());
            return;
        };
        self.finish_remote_write(result);
    }

    fn finish_remote_write(&self, result: deskbridge_platform::Result<()>) {
        let mut st = self.state.lock();
        match result {
            Ok(()) => {
                // The peer's content now owns the clipboard; our older offers are void.
                st.current = None;
                st.ticket += 1;
                st.last_skip = None;
            }
            Err(e) => st.skip(format!(
                "Zwischenablage konnte nicht beschrieben werden: {e}"
            )),
        }
    }
}

/// Delayed-rendering provider backed by the peer.
struct RemoteProvider {
    fetcher: Arc<dyn Fetcher>,
    offer_id: u64,
    inline: Option<Vec<u8>>,
    sizes: HashMap<String, u64>,
    max: u64,
}

impl ClipboardProvider for RemoteProvider {
    fn fetch(&self, m: &str) -> deskbridge_platform::Result<Vec<u8>> {
        let Some(&size) = self.sizes.get(m) else {
            return Err(PlatformError::other(format!(
                "Format {m} wurde nicht angeboten"
            )));
        };
        if m == mime::TEXT
            && let Some(t) = &self.inline
        {
            return Ok(t.clone());
        }
        let data = self
            .fetcher
            .fetch(self.offer_id, m, fetch_timeout(size))
            .map_err(|e: FetchError| PlatformError::other(e.to_string()))?;
        if data.len() as u64 > self.max {
            return Err(PlatformError::other(
                "Antwort überschreitet das Größenlimit",
            ));
        }
        Ok(data)
    }
}
