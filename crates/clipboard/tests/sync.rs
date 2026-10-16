//! Scenario tests of the clipboard sync policy on top of `MockClipboard`.

use deskbridge_clipboard::{ClipboardSync, FetchError, Fetcher, FileSource, SyncConfig};
use deskbridge_files::scan::PlannedFile;
use deskbridge_platform::clipboard::{
    ClipFormatInfo, ClipboardBackend, ClipboardEvent, VirtualFileProvider, mime,
};
use deskbridge_platform::mock::MockClipboard;
use deskbridge_platform::{PlatformError, UnboundedReceiver, unbounded_channel};
use deskbridge_proto::pb;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------- fakes

#[derive(Default)]
struct FakeFetcher {
    data: Mutex<HashMap<(u64, String), Vec<u8>>>,
    calls: Mutex<Vec<(u64, String, Duration)>>,
    fail: Mutex<Option<FetchError>>,
}

impl FakeFetcher {
    fn put(&self, id: u64, m: &str, d: &[u8]) {
        self.data.lock().insert((id, m.to_owned()), d.to_vec());
    }
    fn calls(&self) -> Vec<(u64, String, Duration)> {
        self.calls.lock().clone()
    }
}

impl Fetcher for FakeFetcher {
    fn fetch(&self, offer_id: u64, m: &str, timeout: Duration) -> Result<Vec<u8>, FetchError> {
        self.calls.lock().push((offer_id, m.to_owned(), timeout));
        if let Some(e) = self.fail.lock().clone() {
            return Err(e);
        }
        self.data
            .lock()
            .get(&(offer_id, m.to_owned()))
            .cloned()
            .ok_or(FetchError::NotFound)
    }
}

struct NullFiles;
impl VirtualFileProvider for NullFiles {
    fn open(&self, _index: usize) -> deskbridge_platform::Result<Box<dyn Read + Send>> {
        Err(PlatformError::other("unused"))
    }
}

#[derive(Default)]
struct FakeFileSource {
    virtual_ok: bool,
    staged: Option<Vec<PathBuf>>,
    seen: Mutex<Vec<(u64, usize)>>,
}

impl FileSource for FakeFileSource {
    fn provider(
        &self,
        offer_id: u64,
        files: &[pb::FileEntry],
    ) -> Option<Arc<dyn VirtualFileProvider>> {
        self.seen.lock().push((offer_id, files.len()));
        self.virtual_ok
            .then(|| Arc::new(NullFiles) as Arc<dyn VirtualFileProvider>)
    }
    fn staged(&self, _offer_id: u64, _files: &[pb::FileEntry]) -> Option<Vec<PathBuf>> {
        self.staged.clone()
    }
}

struct Side {
    mock: Arc<MockClipboard>,
    fetcher: Arc<FakeFetcher>,
    sync: ClipboardSync,
    rx: UnboundedReceiver<ClipboardEvent>,
}

fn side_with(cfg: SyncConfig) -> Side {
    let mock = Arc::new(MockClipboard::default());
    let (tx, rx) = unbounded_channel();
    mock.start(tx).unwrap();
    let fetcher = Arc::new(FakeFetcher::default());
    let sync = ClipboardSync::new(mock.clone(), fetcher.clone(), cfg);
    Side {
        mock,
        fetcher,
        sync,
        rx,
    }
}

fn side() -> Side {
    side_with(SyncConfig::default())
}

impl Side {
    /// Feeds the pending backend event into the sync, like the glue's event loop does.
    fn pump(&mut self) -> Option<pb::ClipboardOffer> {
        let ev = self.rx.try_recv().ok()?;
        self.sync.on_local_change(ev)
    }
}

fn changed(formats: &[(&str, u64)], text: Option<&str>) -> ClipboardEvent {
    ClipboardEvent::Changed {
        formats: formats
            .iter()
            .map(|(m, s)| ClipFormatInfo {
                mime: (*m).into(),
                size: *s,
            })
            .collect(),
        sensitive: false,
        text: text.map(str::to_owned),
        files: vec![],
    }
}

fn text_event(t: &str) -> ClipboardEvent {
    changed(&[(mime::TEXT, t.len() as u64)], Some(t))
}

fn set_content(mock: &MockClipboard, items: &[(&str, &[u8])]) {
    *mock.content.lock() = items
        .iter()
        .map(|(m, d)| ((*m).to_owned(), d.to_vec()))
        .collect();
}

// ---------------------------------------------------------------- local -> peer

#[test]
fn small_text_is_sent_eagerly_inline() {
    let mut a = side();
    a.mock.user_copy_text("hallo welt");
    let offer = a.pump().expect("offer");
    assert_eq!(offer.inline_text, b"hallo welt");
    assert_eq!(offer.formats.len(), 1);
    assert_eq!(offer.formats[0].mime, mime::TEXT);
    assert_eq!(offer.formats[0].size, 10);
    assert!(offer.files.is_empty());
    assert!(!offer.sensitive);
    assert_eq!(a.sync.current_local_offer_id(), Some(offer.id));
    assert_eq!(a.sync.last_skip_reason(), None);
}

#[test]
fn unicode_text_roundtrips_byte_exact() {
    let text = "Grüße 日本語 😀 \u{202E}x\r\nZeile 2\0ende";
    let mut a = side();
    let b = side();
    a.mock.user_copy_text(text);
    let offer = a.pump().unwrap();
    assert_eq!(offer.inline_text, text.as_bytes());
    assert_eq!(offer.formats[0].size, text.len() as u64);
    b.sync.on_remote_offer(offer);
    assert_eq!(b.mock.read(mime::TEXT).unwrap().unwrap(), text.as_bytes());
}

#[test]
fn ping_pong_text_does_not_echo_back() {
    let mut a = side();
    let b = side();
    a.mock.user_copy_text("ping");
    let offer = a.pump().unwrap();
    b.sync.on_remote_offer(offer);
    assert_eq!(b.mock.read(mime::TEXT).unwrap().unwrap(), b"ping");

    // B's backend reports the write as a change; it must not travel back to A.
    assert!(b.sync.on_local_change(text_event("ping")).is_none());
    assert_eq!(
        b.sync.current_local_offer_id(),
        None,
        "peer content is not our offer"
    );
    assert_eq!(b.sync.last_skip_reason(), None, "an echo is not a skip");

    // A genuinely new copy on B right afterwards still goes out.
    let reply = b
        .sync
        .on_local_change(text_event("pong"))
        .expect("new text is synced");
    assert_eq!(reply.inline_text, b"pong");
    a.sync.on_remote_offer(reply);
    assert_eq!(a.mock.read(mime::TEXT).unwrap().unwrap(), b"pong");
    assert!(a.sync.on_local_change(text_event("pong")).is_none());
}

#[test]
fn echo_is_swallowed_repeatedly_within_the_window_and_forgotten_afterwards() {
    let a = side();
    let b = side();
    a.sync.on_local_change(text_event("x"));
    let offer = a.sync.on_local_change(text_event("once")).unwrap();
    b.sync.on_remote_offer(offer);
    // WM_CLIPBOARDUPDATE may fire more than once for one write.
    assert!(b.sync.on_local_change(text_event("once")).is_none());
    assert!(b.sync.on_local_change(text_event("once")).is_none());
    std::thread::sleep(Duration::from_millis(2100));
    assert!(
        b.sync.on_local_change(text_event("once")).is_some(),
        "window expired: a deliberate re-copy syncs"
    );
}

#[test]
fn echo_of_delayed_offer_is_not_resent() {
    let b = side();
    let offer = pb::ClipboardOffer {
        id: 9,
        formats: vec![
            pb::ClipFormat {
                mime: mime::HTML.into(),
                size: 40,
            },
            pb::ClipFormat {
                mime: mime::PNG.into(),
                size: 4000,
            },
        ],
        ..Default::default()
    };
    b.sync.on_remote_offer(offer);
    let echo = changed(&[(mime::PNG, 0), (mime::HTML, 0)], None);
    assert!(b.sync.on_local_change(echo).is_none());
}

#[test]
fn echo_of_large_text_without_event_text_is_not_resent() {
    let b = side();
    let big = 3 * 1024 * 1024;
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 3,
        formats: vec![pb::ClipFormat {
            mime: mime::TEXT.into(),
            size: big,
        }],
        ..Default::default()
    });
    assert!(
        b.sync
            .on_local_change(changed(&[(mime::TEXT, 0)], None))
            .is_none()
    );
}

#[test]
fn sensitive_content_never_leaves_and_invalidates_the_current_offer() {
    let a = side();
    let first = a.sync.on_local_change(text_event("normal")).unwrap();
    set_content(&a.mock, &[(mime::TEXT, b"normal")]);
    assert!(
        a.sync
            .serve_request(&pb::ClipboardRequest {
                id: first.id,
                mime: mime::TEXT.into()
            })
            .is_some()
    );

    set_content(&a.mock, &[(mime::TEXT, b"hunter2")]);
    let ev = ClipboardEvent::Changed {
        formats: vec![ClipFormatInfo {
            mime: mime::TEXT.into(),
            size: 7,
        }],
        sensitive: true,
        text: Some("hunter2".into()),
        files: vec![],
    };
    assert!(a.sync.on_local_change(ev).is_none());
    assert!(a.sync.last_skip_reason().unwrap().contains("Sensibler"));
    assert_eq!(a.sync.current_local_offer_id(), None);
    // The password must not be served even to a request for the previous offer id.
    assert!(
        a.sync
            .serve_request(&pb::ClipboardRequest {
                id: first.id,
                mime: mime::TEXT.into()
            })
            .is_none()
    );
}

#[test]
fn sensitive_check_can_be_disabled() {
    let a = side_with(SyncConfig {
        respect_sensitive: false,
        ..SyncConfig::default()
    });
    let ev = ClipboardEvent::Changed {
        formats: vec![ClipFormatInfo {
            mime: mime::TEXT.into(),
            size: 3,
        }],
        sensitive: true,
        text: Some("abc".into()),
        files: vec![],
    };
    assert!(a.sync.on_local_change(ev).is_some());
}

#[test]
fn remote_sensitive_offer_is_ignored() {
    let b = side();
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 1,
        formats: vec![pb::ClipFormat {
            mime: mime::TEXT.into(),
            size: 3,
        }],
        inline_text: b"abc".to_vec(),
        sensitive: true,
        ..Default::default()
    });
    assert!(b.mock.content.lock().is_empty());
}

#[test]
fn send_and_receive_toggles() {
    let a = side_with(SyncConfig {
        send: false,
        ..SyncConfig::default()
    });
    assert!(a.sync.on_local_change(text_event("x")).is_none());
    assert!(a.sync.last_skip_reason().is_some());
    a.sync.set_config(SyncConfig::default());
    assert!(a.sync.on_local_change(text_event("x")).is_some());
    let id = a.sync.current_local_offer_id().unwrap();
    a.sync.set_config(SyncConfig {
        send: false,
        ..SyncConfig::default()
    });
    assert_eq!(
        a.sync.current_local_offer_id(),
        None,
        "disabling send voids the offer"
    );
    set_content(&a.mock, &[(mime::TEXT, b"x")]);
    assert!(
        a.sync
            .serve_request(&pb::ClipboardRequest {
                id,
                mime: mime::TEXT.into()
            })
            .is_none()
    );

    let b = side_with(SyncConfig {
        receive: false,
        ..SyncConfig::default()
    });
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 5,
        formats: vec![pb::ClipFormat {
            mime: mime::TEXT.into(),
            size: 2,
        }],
        inline_text: b"hi".to_vec(),
        ..Default::default()
    });
    assert!(b.mock.content.lock().is_empty());
    assert!(b.mock.offered.lock().is_empty());
    assert!(b.sync.last_skip_reason().unwrap().contains("Empfang"));
}

#[test]
fn format_toggles_filter_local_offers() {
    let all = &[
        (mime::TEXT, 5),
        (mime::HTML, 30),
        (mime::RTF, 40),
        (mime::PNG, 5000),
        ("application/x-foo", 7),
    ];
    let ev = || changed(all, Some("hello"));

    let a = side();
    let o = a.sync.on_local_change(ev()).unwrap();
    let mimes: Vec<_> = o.formats.iter().map(|f| f.mime.as_str()).collect();
    assert_eq!(
        mimes,
        [mime::TEXT, mime::HTML, mime::RTF, mime::PNG],
        "unknown formats are dropped"
    );
    assert_eq!(o.inline_text, b"hello");

    let a = side_with(SyncConfig {
        sync_images: false,
        ..SyncConfig::default()
    });
    let o = a.sync.on_local_change(ev()).unwrap();
    assert!(o.formats.iter().all(|f| f.mime != mime::PNG));
    assert!(o.formats.iter().any(|f| f.mime == mime::HTML));

    let a = side_with(SyncConfig {
        sync_rich_text: false,
        ..SyncConfig::default()
    });
    let o = a.sync.on_local_change(ev()).unwrap();
    let mimes: Vec<_> = o.formats.iter().map(|f| f.mime.as_str()).collect();
    assert_eq!(mimes, [mime::TEXT, mime::PNG]);

    // Only an image copied, images disabled: nothing left, with a reason for the UI.
    let a = side_with(SyncConfig {
        sync_images: false,
        ..SyncConfig::default()
    });
    assert!(
        a.sync
            .on_local_change(changed(&[(mime::PNG, 100)], None))
            .is_none()
    );
    assert!(a.sync.last_skip_reason().unwrap().contains("deaktiviert"));

    // Nothing syncable at all.
    let a = side();
    assert!(
        a.sync
            .on_local_change(changed(&[("application/x-foo", 3)], None))
            .is_none()
    );
    assert!(
        a.sync
            .last_skip_reason()
            .unwrap()
            .contains("Kein synchronisierbares")
    );
}

#[test]
fn receive_side_applies_its_own_toggles() {
    let b = side_with(SyncConfig {
        sync_images: false,
        ..SyncConfig::default()
    });
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 8,
        formats: vec![
            pb::ClipFormat {
                mime: mime::PNG.into(),
                size: 10,
            },
            pb::ClipFormat {
                mime: mime::HTML.into(),
                size: 10,
            },
        ],
        ..Default::default()
    });
    let offered = b.mock.offered.lock().clone();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].mime, mime::HTML);
}

#[test]
fn size_limit_drops_oversized_content_with_reason() {
    let a = side_with(SyncConfig {
        max_size_bytes: 1000,
        ..SyncConfig::default()
    });
    let big = "x".repeat(1001);
    assert!(a.sync.on_local_change(text_event(&big)).is_none());
    assert!(a.sync.last_skip_reason().unwrap().contains("zu groß"));
    assert_eq!(a.sync.current_local_offer_id(), None);

    // Exactly at the limit is fine.
    let ok = "y".repeat(1000);
    assert!(a.sync.on_local_change(text_event(&ok)).is_some());
    assert_eq!(a.sync.last_skip_reason(), None, "success clears the reason");

    // An oversized image is dropped, the text next to it survives.
    let o = a
        .sync
        .on_local_change(changed(
            &[(mime::TEXT, 5), (mime::PNG, 5000)],
            Some("hello"),
        ))
        .unwrap();
    assert_eq!(o.formats.len(), 1);
    assert_eq!(o.formats[0].mime, mime::TEXT);

    // Receiving side enforces its own limit.
    let b = side_with(SyncConfig {
        max_size_bytes: 1000,
        ..SyncConfig::default()
    });
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 2,
        formats: vec![pb::ClipFormat {
            mime: mime::PNG.into(),
            size: 5000,
        }],
        ..Default::default()
    });
    assert!(b.mock.offered.lock().is_empty());
    assert!(b.sync.last_skip_reason().unwrap().contains("zu groß"));
}

#[test]
fn large_text_above_eager_limit_goes_delayed() {
    let mut a = side();
    let b = side();
    let big = "ä".repeat(600_000); // 1.2 MB > 1 MiB
    assert!(big.len() as u64 > SyncConfig::default().eager_limit_bytes);
    a.mock.user_copy_text(&big);
    let offer = a.pump().expect("big text is still offered");
    assert!(offer.inline_text.is_empty(), "not sent eagerly");
    assert_eq!(offer.formats[0].size, big.len() as u64);

    // Receiver registers delayed rendering instead of writing text immediately.
    b.sync.on_remote_offer(offer.clone());
    assert!(b.mock.content.lock().is_empty());
    assert_eq!(b.mock.offered.lock()[0].mime, mime::TEXT);

    // Paste on B -> fetch from A with a size scaled timeout -> A serves from its clipboard.
    let served = a
        .sync
        .serve_request(&pb::ClipboardRequest {
            id: offer.id,
            mime: mime::TEXT.into(),
        })
        .unwrap();
    b.fetcher.put(offer.id, mime::TEXT, &served);
    let pasted = b.mock.user_paste(mime::TEXT).expect("paste works");
    assert_eq!(pasted, big.as_bytes());
    let calls = b.fetcher.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, offer.id);
    assert!(
        calls[0].2 > Duration::from_secs(5),
        "timeout grows with size: {:?}",
        calls[0].2
    );
    assert!(calls[0].2 < Duration::from_secs(6));
}

#[test]
fn eager_limit_is_exclusive_and_configurable() {
    let a = side_with(SyncConfig {
        eager_limit_bytes: 10,
        ..SyncConfig::default()
    });
    assert_eq!(
        a.sync
            .on_local_change(text_event("123456789"))
            .unwrap()
            .inline_text,
        b"123456789"
    );
    assert!(
        a.sync
            .on_local_change(text_event("1234567890"))
            .unwrap()
            .inline_text
            .is_empty()
    );
    let off = side_with(SyncConfig {
        eager_limit_bytes: 0,
        ..SyncConfig::default()
    });
    assert!(
        off.sync
            .on_local_change(text_event("a"))
            .unwrap()
            .inline_text
            .is_empty()
    );
}

#[test]
fn small_text_without_event_text_is_read_from_the_backend() {
    let a = side();
    set_content(&a.mock, &[(mime::TEXT, b"from backend")]);
    let o = a
        .sync
        .on_local_change(changed(&[(mime::TEXT, 12)], None))
        .unwrap();
    assert_eq!(o.inline_text, b"from backend");
}

#[test]
fn rich_offer_with_inline_text_serves_plain_text_without_a_fetch() {
    let mut a = side();
    let b = side();
    set_content(
        &a.mock,
        &[(mime::TEXT, b"fett"), (mime::HTML, b"<b>fett</b>")],
    );
    let ev = changed(&[(mime::TEXT, 4), (mime::HTML, 11)], Some("fett"));
    let offer = a.sync.on_local_change(ev).unwrap();
    assert_eq!(offer.inline_text, b"fett");
    assert_eq!(offer.formats.len(), 2);
    let _ = a.pump();

    b.sync.on_remote_offer(offer.clone());
    assert_eq!(b.mock.user_paste(mime::TEXT).unwrap(), b"fett");
    assert!(
        b.fetcher.calls().is_empty(),
        "inline text needs no round trip"
    );
    b.fetcher.put(offer.id, mime::HTML, b"<b>fett</b>");
    assert_eq!(b.mock.user_paste(mime::HTML).unwrap(), b"<b>fett</b>");
    assert_eq!(b.fetcher.calls().len(), 1);
}

// ---------------------------------------------------------------- delayed rendering / requests

#[test]
fn delayed_image_paste_goes_through_the_fetcher() {
    let a = side();
    let b = side();
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
    set_content(&a.mock, &[(mime::PNG, &png)]);
    let offer = a
        .sync
        .on_local_change(changed(&[(mime::PNG, png.len() as u64)], None))
        .unwrap();
    assert!(offer.inline_text.is_empty());

    b.sync.on_remote_offer(offer.clone());
    assert!(
        b.mock.content.lock().is_empty(),
        "nothing is transferred before the paste"
    );
    let data = a
        .sync
        .serve_request(&pb::ClipboardRequest {
            id: offer.id,
            mime: mime::PNG.into(),
        })
        .unwrap();
    assert_eq!(data, png);
    b.fetcher.put(offer.id, mime::PNG, &data);
    assert_eq!(b.mock.user_paste(mime::PNG).unwrap(), png);
    let calls = b.fetcher.calls();
    assert_eq!((calls[0].0, calls[0].1.as_str()), (offer.id, mime::PNG));
    assert!(calls[0].2 >= Duration::from_secs(5));
}

#[test]
fn fetch_timeout_or_error_yields_an_empty_paste() {
    let b = side();
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 77,
        formats: vec![pb::ClipFormat {
            mime: mime::PNG.into(),
            size: 100,
        }],
        ..Default::default()
    });
    for err in [
        FetchError::Timeout,
        FetchError::Disconnected,
        FetchError::NotFound,
        FetchError::Other("x".into()),
    ] {
        *b.fetcher.fail.lock() = Some(err);
        assert_eq!(b.mock.user_paste(mime::PNG), None);
    }
    assert_eq!(b.fetcher.calls().len(), 4);
    // A format that was never offered is not even fetched.
    *b.fetcher.fail.lock() = None;
    assert_eq!(b.mock.user_paste(mime::RTF), None);
    assert_eq!(b.fetcher.calls().len(), 4);
}

#[test]
fn fetched_data_above_the_limit_is_rejected() {
    let b = side_with(SyncConfig {
        max_size_bytes: 10,
        ..SyncConfig::default()
    });
    b.sync.on_remote_offer(pb::ClipboardOffer {
        id: 4,
        formats: vec![pb::ClipFormat {
            mime: mime::PNG.into(),
            size: 5,
        }],
        ..Default::default()
    });
    b.fetcher.put(4, mime::PNG, &[0u8; 11]);
    assert_eq!(b.mock.user_paste(mime::PNG), None);
}

#[test]
fn newer_local_offer_invalidates_older_requests() {
    let a = side();
    set_content(&a.mock, &[(mime::PNG, b"one")]);
    let first = a
        .sync
        .on_local_change(changed(&[(mime::PNG, 3)], None))
        .unwrap();
    set_content(&a.mock, &[(mime::PNG, b"two"), (mime::HTML, b"<p/>")]);
    let second = a
        .sync
        .on_local_change(changed(&[(mime::PNG, 3), (mime::HTML, 4)], None))
        .unwrap();
    assert!(second.id > first.id);
    assert_eq!(a.sync.current_local_offer_id(), Some(second.id));

    let req = |id, m: &str| pb::ClipboardRequest { id, mime: m.into() };
    assert_eq!(
        a.sync.serve_request(&req(first.id, mime::PNG)),
        None,
        "stale offer"
    );
    assert_eq!(
        a.sync.serve_request(&req(second.id, mime::PNG)),
        Some(b"two".to_vec())
    );
    assert_eq!(
        a.sync.serve_request(&req(second.id, mime::HTML)),
        Some(b"<p/>".to_vec())
    );
    assert_eq!(
        a.sync.serve_request(&req(second.id, mime::RTF)),
        None,
        "not offered"
    );
    assert_eq!(
        a.sync.serve_request(&req(second.id + 1000, mime::PNG)),
        None,
        "unknown id"
    );
}

#[test]
fn receiving_a_remote_offer_voids_our_local_offer() {
    let a = side();
    let offer = a.sync.on_local_change(text_event("mine")).unwrap();
    set_content(&a.mock, &[(mime::TEXT, b"mine")]);
    a.sync.on_remote_offer(pb::ClipboardOffer {
        id: 5,
        formats: vec![pb::ClipFormat {
            mime: mime::TEXT.into(),
            size: 6,
        }],
        inline_text: b"theirs".to_vec(),
        ..Default::default()
    });
    assert_eq!(a.sync.current_local_offer_id(), None);
    assert_eq!(
        a.sync.serve_request(&pb::ClipboardRequest {
            id: offer.id,
            mime: mime::TEXT.into()
        }),
        None
    );
}

#[test]
fn serve_request_respects_size_limit_and_missing_data() {
    let a = side_with(SyncConfig {
        max_size_bytes: 8,
        ..SyncConfig::default()
    });
    // Declared small, but the clipboard now holds more than the limit.
    set_content(&a.mock, &[(mime::PNG, &[7u8; 9])]);
    let o = a
        .sync
        .on_local_change(changed(&[(mime::PNG, 4)], None))
        .unwrap();
    assert_eq!(
        a.sync.serve_request(&pb::ClipboardRequest {
            id: o.id,
            mime: mime::PNG.into()
        }),
        None
    );
    // Backend has no data for the format.
    set_content(&a.mock, &[]);
    assert_eq!(
        a.sync.serve_request(&pb::ClipboardRequest {
            id: o.id,
            mime: mime::PNG.into()
        }),
        None
    );
}

// ---------------------------------------------------------------- files

fn temp_tree() -> (tempfile::TempDir, Vec<PathBuf>) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("ordner/leer")).unwrap();
    std::fs::write(d.path().join("ordner/a.txt"), b"aaa").unwrap();
    std::fs::write(d.path().join("b.bin"), vec![1u8; 100]).unwrap();
    let paths = vec![d.path().join("ordner"), d.path().join("b.bin")];
    (d, paths)
}

#[test]
fn local_file_list_becomes_a_file_offer() {
    let (_d, paths) = temp_tree();
    let a = side();
    let ev = ClipboardEvent::Changed {
        formats: vec![ClipFormatInfo {
            mime: mime::URI_LIST.into(),
            size: 0,
        }],
        sensitive: false,
        text: None,
        files: paths,
    };
    let o = a.sync.on_local_change(ev).expect("file offer");
    assert!(
        o.formats.is_empty(),
        "local paths are not offered as uri-list"
    );
    assert!(o.inline_text.is_empty());
    let rels: Vec<_> = o
        .files
        .iter()
        .map(|f| (f.rel_path.as_str(), f.is_dir, f.size))
        .collect();
    assert_eq!(
        rels,
        [
            ("ordner", true, 0),
            ("ordner/a.txt", false, 3),
            ("ordner/leer", true, 0),
            ("b.bin", false, 100)
        ]
    );
    let planned: Vec<PlannedFile> = a.sync.local_files(o.id).unwrap();
    assert_eq!(planned.len(), 4);
    assert!(a.sync.local_files(o.id + 1).is_none());
    // Files are not served through the format request path.
    assert_eq!(
        a.sync.serve_request(&pb::ClipboardRequest {
            id: o.id,
            mime: mime::URI_LIST.into()
        }),
        None
    );
}

#[test]
fn file_lists_respect_the_toggle_and_unreadable_paths() {
    let (_d, paths) = temp_tree();
    let ev = |files: Vec<PathBuf>| ClipboardEvent::Changed {
        formats: vec![],
        sensitive: false,
        text: None,
        files,
    };
    let a = side_with(SyncConfig {
        sync_files: false,
        ..SyncConfig::default()
    });
    assert!(a.sync.on_local_change(ev(paths)).is_none());
    assert!(a.sync.last_skip_reason().unwrap().contains("deaktiviert"));

    let a = side();
    assert!(
        a.sync
            .on_local_change(ev(vec![PathBuf::from(r"C:\gibt\es\nicht.txt")]))
            .is_none()
    );
    assert!(a.sync.last_skip_reason().unwrap().contains("nicht lesbar"));
}

fn file_offer() -> pb::ClipboardOffer {
    pb::ClipboardOffer {
        id: 21,
        files: vec![
            pb::FileEntry {
                rel_path: "ordner".into(),
                size: 0,
                mtime_unix_ms: 5,
                is_dir: true,
            },
            pb::FileEntry {
                rel_path: "ordner/a.txt".into(),
                size: 3,
                mtime_unix_ms: 6,
                is_dir: false,
            },
        ],
        ..Default::default()
    }
}

#[test]
fn remote_file_offer_uses_virtual_files() {
    let b = side();
    let src = Arc::new(FakeFileSource {
        virtual_ok: true,
        ..Default::default()
    });
    b.sync.set_file_source(src.clone());
    b.sync.on_remote_offer(file_offer());
    let offered = b.mock.offered_files.lock().clone();
    assert_eq!(offered.len(), 2);
    assert_eq!(offered[1].rel_path, "ordner/a.txt");
    assert_eq!(offered[1].size, 3);
    assert_eq!(offered[1].mtime_unix_ms, 6);
    assert!(offered[0].is_dir);
    assert!(b.mock.file_provider.lock().is_some());
    assert_eq!(src.seen.lock().as_slice(), &[(21, 2)]);
    // The echo (file list on the clipboard) is not sent back.
    let echo = ClipboardEvent::Changed {
        formats: vec![],
        sensitive: false,
        text: None,
        files: vec![PathBuf::from(r"C:\irgendwo\ordner")],
    };
    assert!(b.sync.on_local_change(echo).is_none());
}

#[test]
fn remote_file_offer_falls_back_to_staged_files() {
    let b = side();
    let staged = vec![PathBuf::from(r"C:\Incoming\21\ordner")];
    b.sync.set_file_source(Arc::new(FakeFileSource {
        virtual_ok: false,
        staged: Some(staged.clone()),
        ..Default::default()
    }));
    b.sync.on_remote_offer(file_offer());
    assert!(b.mock.offered_files.lock().is_empty());
    assert_eq!(*b.mock.local_files.lock(), staged);
}

#[test]
fn remote_file_offer_rules() {
    // Disabled by config.
    let b = side_with(SyncConfig {
        sync_files: false,
        ..SyncConfig::default()
    });
    b.sync.set_file_source(Arc::new(FakeFileSource {
        virtual_ok: true,
        ..Default::default()
    }));
    b.sync.on_remote_offer(file_offer());
    assert!(b.mock.offered_files.lock().is_empty());

    // No file source plugged in.
    let b = side();
    b.sync.on_remote_offer(file_offer());
    assert!(b.mock.offered_files.lock().is_empty());
    assert!(
        b.sync
            .last_skip_reason()
            .unwrap()
            .contains("nicht verfügbar")
    );

    // Hostile paths are refused before anything reaches the clipboard.
    let b = side();
    b.sync.set_file_source(Arc::new(FakeFileSource {
        virtual_ok: true,
        ..Default::default()
    }));
    for evil in ["../x", "C:\\Windows\\x", "a/NUL", "x:y"] {
        let mut o = file_offer();
        o.files.push(pb::FileEntry {
            rel_path: evil.into(),
            size: 1,
            mtime_unix_ms: 0,
            is_dir: false,
        });
        b.sync.on_remote_offer(o);
        assert!(b.mock.offered_files.lock().is_empty(), "{evil}");
    }
    assert!(b.sync.last_skip_reason().unwrap().contains("Ungültige"));
}

// ---------------------------------------------------------------- concurrency

#[test]
fn concurrent_events_from_several_threads() {
    let s = side();
    let sync = Arc::new(s.sync);
    let ids = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for t in 0..8 {
        let sync = sync.clone();
        let ids = ids.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..200 {
                match (t + i) % 4 {
                    0 | 1 => {
                        if let Some(o) =
                            sync.on_local_change(text_event(&format!("thread {t} msg {i}")))
                        {
                            ids.lock().push(o.id);
                        }
                    }
                    2 => sync.on_remote_offer(pb::ClipboardOffer {
                        id: 1_000_000 + (t * 1000 + i) as u64,
                        formats: vec![pb::ClipFormat {
                            mime: mime::TEXT.into(),
                            size: 3,
                        }],
                        inline_text: format!("r{t}-{i}").into_bytes(),
                        ..Default::default()
                    }),
                    _ => {
                        let _ = sync.serve_request(&pb::ClipboardRequest {
                            id: 1,
                            mime: mime::TEXT.into(),
                        });
                        let _ = sync.current_local_offer_id();
                        let _ = sync.last_skip_reason();
                    }
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let mut ids = ids.lock().clone();
    let n = ids.len();
    assert!(n > 0);
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "offer ids are unique");
    // The final state is consistent: a fresh event still works and gets a higher id.
    let max = *ids.last().unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let o = sync.on_local_change(text_event("final")).unwrap();
    assert!(o.id > max);
    assert_eq!(sync.current_local_offer_id(), Some(o.id));
}
