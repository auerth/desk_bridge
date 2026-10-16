//! Large streamed transfers. A counting allocator proves that the engine does not hold the file in
//! memory (peak heap growth stays far below the file size) and the throughput is reported.
//!
//! Throughput numbers: `cargo test -p deskbridge-files --release --test streaming -- --include-ignored --nocapture`.

use deskbridge_files::wire::{receive_file, send_reader};
use deskbridge_proto::pb;
use std::alloc::{GlobalAlloc, Layout, System};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::io::{AsyncRead, ReadBuf};

struct Counting;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let np = unsafe { System.realloc(p, l, new) };
        if !np.is_null() {
            if new >= l.size() {
                let now = CURRENT.fetch_add(new - l.size(), Ordering::Relaxed) + (new - l.size());
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        np
    }
}

#[global_allocator]
static A: Counting = Counting;

/// The tests share the global counters, so they must not overlap.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Deterministic pseudo-random content (splitmix64), generated on the fly.
struct PrngReader {
    state: u64,
    remaining: u64,
}

impl PrngReader {
    fn new(seed: u64, len: u64) -> Self {
        Self {
            state: seed,
            remaining: len,
        }
    }
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl AsyncRead for PrngReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let n = (buf.remaining() as u64).min(self.remaining) as usize;
        let out = buf.initialize_unfilled_to(n);
        for chunk in out.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        buf.advance(n);
        self.remaining -= n as u64;
        Poll::Ready(Ok(()))
    }
}

fn expected_hash(seed: u64, len: u64) -> [u8; 32] {
    let mut r = PrngReader::new(seed, len);
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        for c in buf[..n].chunks_mut(8) {
            let v = r.next().to_le_bytes();
            c.copy_from_slice(&v[..c.len()]);
        }
        h.update(&buf[..n]);
        left -= n as u64;
    }
    *h.finalize().as_bytes()
}

async fn stream_roundtrip(len: u64, label: &str) {
    let _guard = SERIAL.lock().await;
    let seed = 0xDE5C_B21D_u64;
    let expect = expected_hash(seed, len);
    let dst = tempfile::tempdir().unwrap();
    let entry = pb::FileEntry {
        rel_path: "gross.bin".into(),
        size: len,
        mtime_unix_ms: 1_700_000_000_000,
        is_dir: false,
    };

    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let started = Instant::now();

    let (mut tx, mut rx) = tokio::io::duplex(1 << 20);
    let e2 = entry.clone();
    let sender = tokio::spawn(async move {
        let mut sent = 0u64;
        let h = send_reader(
            &mut tx,
            1,
            0,
            &e2,
            PrngReader::new(seed, len),
            0,
            &mut |n| sent += n,
        )
        .await
        .unwrap();
        (h, sent)
    });
    let mut got_bytes = 0u64;
    let got = receive_file(&mut rx, dst.path(), &mut |n| got_bytes += n)
        .await
        .unwrap();
    let (sent_hash, sent) = sender.await.unwrap();

    let secs = started.elapsed().as_secs_f64();
    let peak_growth = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    assert_eq!(sent, len);
    assert_eq!(got_bytes, len);
    assert_eq!(sent_hash, expect);
    assert_eq!(got.blake3, expect);
    assert_eq!(std::fs::metadata(&got.path).unwrap().len(), len);
    println!(
        "[{label}] {} MiB in {secs:.2}s = {:.0} MB/s (incl. disk write), peak heap growth {} KiB",
        len >> 20,
        len as f64 / secs / 1e6,
        peak_growth >> 10
    );
    assert!(
        peak_growth < 16 << 20,
        "heap grew by {} KiB while streaming {} MiB: file is not streamed",
        peak_growth >> 10,
        len >> 20
    );
}

#[tokio::test]
async fn stream_64_mib_without_buffering_the_file() {
    stream_roundtrip(64 << 20, "64 MiB").await;
}

/// 1 GiB streamed through the engine (writes a 1 GiB file to the temp dir; ~2 s in debug).
#[tokio::test]
async fn stream_1_gib_without_buffering_the_file() {
    stream_roundtrip(1 << 30, "1 GiB").await;
}

/// Engine-only throughput: sender -> in-memory duplex -> a sink that parses the frames and
/// verifies the hash, no disk involved on either side.
#[tokio::test]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
async fn throughput_through_memory_duplex() {
    use tokio::io::AsyncReadExt;
    let _guard = SERIAL.lock().await;
    let len: u64 = 2 << 30;
    let entry = pb::FileEntry {
        rel_path: "x.bin".into(),
        size: len,
        mtime_unix_ms: 0,
        is_dir: false,
    };
    let (mut tx, mut rx) = tokio::io::duplex(4 << 20);
    let started = Instant::now();
    let sender = tokio::spawn(async move {
        // A zero-filled source isolates framing + hashing + copy cost from the PRNG.
        send_reader(
            &mut tx,
            1,
            0,
            &entry,
            tokio::io::repeat(0).take(len),
            0,
            &mut |_| {},
        )
        .await
        .unwrap()
    });
    let _hdr: pb::FileHeader = deskbridge_proto::frame::read_msg(&mut rx).await.unwrap();
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; deskbridge_files::CHUNK];
    let mut total = 0u64;
    loop {
        let mut l = [0u8; 4];
        rx.read_exact(&mut l).await.unwrap();
        let n = u32::from_le_bytes(l) as usize;
        if n == 0 {
            break;
        }
        rx.read_exact(&mut buf[..n]).await.unwrap();
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let trailer: pb::FileTrailer = deskbridge_proto::frame::read_msg(&mut rx).await.unwrap();
    let h = sender.await.unwrap();
    assert_eq!(
        trailer.blake3.as_slice(),
        hasher.finalize().as_bytes().as_slice()
    );
    assert_eq!(h.as_slice(), trailer.blake3.as_slice());
    let secs = started.elapsed().as_secs_f64();
    println!(
        "[duplex] {} MiB in {secs:.2}s = {:.2} GB/s",
        total >> 20,
        total as f64 / secs / 1e9
    );
}
