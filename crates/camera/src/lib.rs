//! The picture path between the Desk Bridge agent and the "Desk Bridge Kamera" virtual camera.
//!
//! The virtual camera (`deskbridge-vcam.dll`) is a Media Foundation media source that Windows'
//! *Frame Server* service loads (as `LOCAL SERVICE`, in session 0). It cannot talk to the agent
//! over the network or a pipe it would have to find, so both share one named file mapping,
//! [`MAPPING_NAME`]:
//!
//! ```text
//! +-------------------+ 0
//! | Header            |  magic, version, heartbeats, the latest published frame,
//! |                   |  what the camera app asked for (size, fps)
//! +-------------------+ HEADER_BYTES
//! | slot 0: NV12      |  up to MAX_WIDTH x MAX_HEIGHT
//! | slot 1: NV12      |
//! | slot 2: NV12      |
//! +-------------------+ TOTAL_BYTES
//! ```
//!
//! * The **agent** (writer) decodes the partner's webcam stream and writes each picture into the
//!   next slot, then publishes its sequence number. Three slots mean a reader copying the latest
//!   picture is only overwritten if the writer gets two pictures ahead during that copy (~1 ms).
//!   Every slot also carries the sequence it holds, so a torn copy is detected and dropped.
//! * The **virtual camera** (reader) beats [`Header::reader_alive_ms`] while an app streams from
//!   it and states the size the app chose. The agent turns "the beat is fresh" into `CameraInUse`
//!   for the partner, exactly like the microphone: the PC only opens its webcam while an app on
//!   the laptop shows the camera.
//!
//! Heartbeats use `GetTickCount64` (milliseconds since boot, the same in every process).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub mod nv12;
#[cfg(windows)]
pub mod shm;

/// COM class of the media source in `deskbridge_vcam.dll` (registered under HKLM, the Frame
/// Server runs as `LOCAL SERVICE` and does not see per-user registrations).
pub const VCAM_CLSID: u128 = 0x5b3f_8c21_7a4e_4d19_b6c2_9e1f_04a7_d385;
/// Name of the camera in Teams, Zoom and the Windows camera settings.
pub const VCAM_FRIENDLY_NAME: &str = "Desk Bridge Kamera";
/// File name of the media source DLL next to the agent.
pub const VCAM_DLL: &str = "deskbridge_vcam.dll";

/// Name of the file mapping (global namespace: the Frame Server runs in session 0).
pub const MAPPING_NAME: &str = "Global\\DeskBridgeCamera.v1";
/// `"DBCM"`.
pub const MAGIC: u32 = 0x4D43_4244;
pub const VERSION: u32 = 1;
pub const SLOTS: usize = 3;
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const HEADER_BYTES: usize = 4096;
/// One NV12 picture of the maximum size, rounded up to a page.
pub const SLOT_BYTES: usize = (nv12::size(MAX_WIDTH, MAX_HEIGHT)).div_ceil(4096) * 4096;
pub const TOTAL_BYTES: usize = HEADER_BYTES + SLOTS * SLOT_BYTES;

/// The camera counts as "in use" while its reader beat is younger than this.
pub const READER_TIMEOUT_MS: u64 = 1_500;
/// The camera shows the partner's picture while the writer beat is younger than this; after that
/// it shows the placeholder (agent gone or the partner stopped sending).
pub const WRITER_TIMEOUT_MS: u64 = 2_000;

/// Sizes the virtual camera offers, the first is the default. All 16:9 like the usual webcams.
pub const OFFERED_SIZES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (640, 360)];
pub const OFFERED_FPS: u32 = 30;

/// What the agent currently has for the camera (shown by the placeholder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum WriterState {
    /// No agent or no partner connected.
    Idle = 0,
    /// Connected; the partner's webcam is starting.
    Waiting = 1,
    /// Pictures are flowing.
    Streaming = 2,
    /// The partner has no webcam, or it is switched off there.
    Unavailable = 3,
}

impl WriterState {
    #[must_use]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Waiting,
            2 => Self::Streaming,
            3 => Self::Unavailable,
            _ => Self::Idle,
        }
    }
}

#[repr(C)]
pub struct Slot {
    /// Sequence number of the picture in the slot, 0 while it is being written.
    pub seq: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Capture time on the writer's clock (µs), informational.
    pub ts_us: AtomicU64,
}

#[repr(C)]
pub struct Header {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    /// Writer heartbeat (`GetTickCount64`), refreshed with every picture and at least every
    /// 500 ms while the agent runs.
    pub writer_alive_ms: AtomicU64,
    /// [`WriterState`].
    pub writer_state: AtomicU32,
    pub _pad0: u32,
    /// Sequence number of the latest complete picture (0 = none yet).
    pub published: AtomicU64,
    pub slots: [Slot; SLOTS],
    /// Reader heartbeat while an app streams from the virtual camera (0 = stopped).
    pub reader_alive_ms: AtomicU64,
    /// Size and rate of the media type the app selected.
    pub reader_width: AtomicU32,
    pub reader_height: AtomicU32,
    pub reader_fps: AtomicU32,
    /// Pictures the reader delivered (diagnostics).
    pub reader_frames: AtomicU32,
    /// Pictures the reader delivered without a fresh picture from the writer (placeholder or a
    /// repeated picture).
    pub reader_repeats: AtomicU32,
    /// Spin lock of the log area (the virtual camera logs from several threads).
    pub log_lock: AtomicU32,
    /// Bytes ever written into the log area (it is a ring of [`LOG_BYTES`]).
    pub log_written: AtomicU64,
}

/// The virtual camera runs inside the Frame Server, where it has no log file of its own: it
/// writes lines into this ring and the agent copies them into its log.
pub const LOG_OFFSET: usize = 2048;
pub const LOG_BYTES: usize = HEADER_BYTES - LOG_OFFSET;

const _: () = assert!(std::mem::size_of::<Header>() <= LOG_OFFSET);

/// A picture that was read from the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub ts_us: u64,
}

/// What the camera app asked for while it streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderRequest {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Typed access to a mapped region of [`TOTAL_BYTES`] bytes.
pub struct Ring {
    base: *mut u8,
}

// SAFETY: all shared fields are atomics; slot data is only written by the single writer and
// guarded by the slot sequence numbers.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    /// # Safety
    /// `base` must point to at least [`TOTAL_BYTES`] bytes that stay mapped (and 8-byte aligned)
    /// for the lifetime of the `Ring`, zero-initialised or written by another `Ring`.
    #[must_use]
    pub unsafe fn new(base: *mut u8) -> Self {
        let r = Self { base };
        let h = r.header();
        if h.magic.load(Ordering::Acquire) != MAGIC {
            h.version.store(VERSION, Ordering::Relaxed);
            h.magic.store(MAGIC, Ordering::Release);
        }
        r
    }

    #[must_use]
    pub fn header(&self) -> &Header {
        // SAFETY: `new`'s contract; the header is at offset 0 and fits.
        unsafe { &*self.base.cast::<Header>() }
    }

    /// Whether the other side speaks the same layout.
    #[must_use]
    pub fn compatible(&self) -> bool {
        let h = self.header();
        h.magic.load(Ordering::Acquire) == MAGIC && h.version.load(Ordering::Acquire) == VERSION
    }

    fn slot_data(&self, i: usize) -> *mut u8 {
        // SAFETY: inside the mapping (`i < SLOTS`).
        unsafe { self.base.add(HEADER_BYTES + i * SLOT_BYTES) }
    }

    // ----------------------------------------------------------------------------- writer side

    /// Writes one NV12 picture: `fill` gets the slot (exactly `nv12::size(w, h)` bytes, stride =
    /// width) and must fill all of it. Pictures larger than the slot are refused.
    pub fn write_frame(
        &self,
        width: u32,
        height: u32,
        ts_us: u64,
        now_ms: u64,
        fill: impl FnOnce(&mut [u8]),
    ) -> bool {
        let len = nv12::size(width, height);
        if width == 0
            || height == 0
            || len > SLOT_BYTES
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            return false;
        }
        let h = self.header();
        let next = h.published.load(Ordering::Acquire).wrapping_add(1).max(1);
        let i = (next % SLOTS as u64) as usize;
        let slot = &h.slots[i];
        slot.seq.store(0, Ordering::Release);
        std::sync::atomic::fence(Ordering::SeqCst);
        // SAFETY: the single writer owns the slot while its seq is 0.
        let data = unsafe { std::slice::from_raw_parts_mut(self.slot_data(i), len) };
        fill(data);
        slot.width.store(width, Ordering::Relaxed);
        slot.height.store(height, Ordering::Relaxed);
        slot.ts_us.store(ts_us, Ordering::Relaxed);
        slot.seq.store(next, Ordering::Release);
        h.published.store(next, Ordering::Release);
        h.writer_alive_ms.store(now_ms, Ordering::Release);
        true
    }

    pub fn set_writer_state(&self, state: WriterState, now_ms: u64) {
        let h = self.header();
        h.writer_state.store(state as u32, Ordering::Release);
        h.writer_alive_ms.store(now_ms, Ordering::Release);
    }

    /// The app on this machine that streams from the virtual camera, if one does.
    #[must_use]
    pub fn reader_request(&self, now_ms: u64) -> Option<ReaderRequest> {
        let h = self.header();
        let alive = h.reader_alive_ms.load(Ordering::Acquire);
        if alive == 0 || now_ms.saturating_sub(alive) > READER_TIMEOUT_MS {
            return None;
        }
        Some(ReaderRequest {
            width: h.reader_width.load(Ordering::Relaxed),
            height: h.reader_height.load(Ordering::Relaxed),
            fps: h.reader_fps.load(Ordering::Relaxed),
        })
    }

    // ----------------------------------------------------------------------------- reader side

    pub fn reader_beat(&self, req: Option<ReaderRequest>, now_ms: u64) {
        let h = self.header();
        match req {
            Some(r) => {
                h.reader_width.store(r.width, Ordering::Relaxed);
                h.reader_height.store(r.height, Ordering::Relaxed);
                h.reader_fps.store(r.fps, Ordering::Relaxed);
                h.reader_alive_ms.store(now_ms.max(1), Ordering::Release);
            }
            None => h.reader_alive_ms.store(0, Ordering::Release),
        }
    }

    /// The writer's state, [`WriterState::Idle`] once its heartbeat is stale.
    #[must_use]
    pub fn writer_state(&self, now_ms: u64) -> WriterState {
        let h = self.header();
        let alive = h.writer_alive_ms.load(Ordering::Acquire);
        if alive == 0 || now_ms.saturating_sub(alive) > WRITER_TIMEOUT_MS {
            return WriterState::Idle;
        }
        WriterState::from_u32(h.writer_state.load(Ordering::Acquire))
    }

    /// Appends one line to the log ring (cut to the ring size).
    pub fn log(&self, line: &str) {
        let h = self.header();
        let mut bytes: Vec<u8> = line.bytes().take(LOG_BYTES - 1).collect();
        bytes.push(b'\n');
        let mut spins = 0u32;
        while h
            .log_lock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins += 1;
            if spins > 10_000 {
                return; // a crashed holder: drop the line rather than hang
            }
            std::hint::spin_loop();
        }
        let start = h.log_written.load(Ordering::Relaxed);
        for (i, b) in bytes.iter().enumerate() {
            let at = ((start + i as u64) % LOG_BYTES as u64) as usize;
            // SAFETY: inside the header page, guarded by the lock.
            unsafe { self.base.add(LOG_OFFSET + at).write_volatile(*b) };
        }
        h.log_written
            .store(start + bytes.len() as u64, Ordering::Release);
        h.log_lock.store(0, Ordering::Release);
    }

    /// Log text written since position `from` (what is still in the ring) and the new position.
    #[must_use]
    pub fn log_since(&self, from: u64) -> (String, u64) {
        let h = self.header();
        let end = h.log_written.load(Ordering::Acquire);
        if end <= from {
            return (String::new(), end.min(from));
        }
        let start = from.max(end.saturating_sub(LOG_BYTES as u64));
        let bytes: Vec<u8> = (start..end)
            .map(|p| {
                let at = (p % LOG_BYTES as u64) as usize;
                // SAFETY: inside the header page.
                unsafe { self.base.add(LOG_OFFSET + at).read_volatile() }
            })
            .collect();
        (String::from_utf8_lossy(&bytes).into_owned(), end)
    }

    /// Copies the latest picture into `out` if it is newer than `last_seq`. `None`: nothing new
    /// (or the copy was torn twice in a row, which needs a writer two pictures ahead).
    pub fn read_latest(&self, last_seq: u64, out: &mut Vec<u8>) -> Option<FrameInfo> {
        let h = self.header();
        for _ in 0..2 {
            let seq = h.published.load(Ordering::Acquire);
            if seq == 0 || seq == last_seq {
                return None;
            }
            let i = (seq % SLOTS as u64) as usize;
            let slot = &h.slots[i];
            if slot.seq.load(Ordering::Acquire) != seq {
                continue;
            }
            let width = slot.width.load(Ordering::Relaxed);
            let height = slot.height.load(Ordering::Relaxed);
            let ts_us = slot.ts_us.load(Ordering::Relaxed);
            let len = nv12::size(width, height);
            if len == 0 || len > SLOT_BYTES {
                return None;
            }
            out.resize(len, 0);
            // SAFETY: inside the slot; a concurrent rewrite is detected below.
            unsafe { std::ptr::copy_nonoverlapping(self.slot_data(i), out.as_mut_ptr(), len) };
            std::sync::atomic::fence(Ordering::SeqCst);
            if slot.seq.load(Ordering::Acquire) == seq {
                return Some(FrameInfo {
                    seq,
                    width,
                    height,
                    ts_us,
                });
            }
        }
        None
    }
}

/// Milliseconds since boot, the same clock in every process (`GetTickCount64`).
#[must_use]
pub fn tick_ms() -> u64 {
    #[cfg(windows)]
    {
        // SAFETY: no preconditions.
        unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
    }
    #[cfg(not(windows))]
    {
        use std::sync::OnceLock;
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis() as u64
            + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> Vec<u64> {
        vec![0u64; TOTAL_BYTES / 8]
    }

    #[test]
    fn write_then_read() {
        let mut mem = region();
        // SAFETY: the vector is large enough and outlives the rings.
        let w = unsafe { Ring::new(mem.as_mut_ptr().cast()) };
        let r = unsafe { Ring::new(mem.as_mut_ptr().cast()) };
        assert!(r.compatible());
        let mut buf = Vec::new();
        assert_eq!(r.read_latest(0, &mut buf), None, "nothing published yet");
        assert!(w.write_frame(64, 36, 7, 100, |d| d.fill(42)));
        let f = r.read_latest(0, &mut buf).expect("a picture");
        assert_eq!((f.width, f.height, f.ts_us, f.seq), (64, 36, 7, 1));
        assert_eq!(buf.len(), nv12::size(64, 36));
        assert!(buf.iter().all(|&b| b == 42));
        assert_eq!(r.read_latest(f.seq, &mut buf), None, "nothing newer");
        for n in 0..5u8 {
            assert!(w.write_frame(32, 18, 0, 100, |d| d.fill(n)));
        }
        let f = r.read_latest(1, &mut buf).expect("latest");
        assert_eq!(f.seq, 6);
        assert!(buf.iter().all(|&b| b == 4));
    }

    #[test]
    fn refuses_bad_sizes() {
        let mut mem = region();
        let w = unsafe { Ring::new(mem.as_mut_ptr().cast()) };
        assert!(!w.write_frame(0, 10, 0, 1, |_| {}));
        assert!(!w.write_frame(33, 18, 0, 1, |_| {}));
        assert!(!w.write_frame(3840, 2160, 0, 1, |_| {}));
    }

    #[test]
    fn log_ring() {
        let mut mem = region();
        let ring = unsafe { Ring::new(mem.as_mut_ptr().cast()) };
        assert_eq!(ring.log_since(0).0, "");
        ring.log("hello");
        let (t, p) = ring.log_since(0);
        assert_eq!((t.as_str(), p), ("hello\n", 6));
        ring.log("world");
        assert_eq!(ring.log_since(p).0, "world\n");
        for i in 0..500 {
            ring.log(&format!("line {i}"));
        }
        let (t, _) = ring.log_since(0);
        assert!(t.len() <= LOG_BYTES);
        assert!(t.ends_with("line 499\n"));
    }

    #[test]
    fn heartbeats() {
        let mut mem = region();
        let ring = unsafe { Ring::new(mem.as_mut_ptr().cast()) };
        assert_eq!(ring.writer_state(10_000), WriterState::Idle);
        ring.set_writer_state(WriterState::Streaming, 10_000);
        assert_eq!(ring.writer_state(11_000), WriterState::Streaming);
        assert_eq!(
            ring.writer_state(10_000 + WRITER_TIMEOUT_MS + 1),
            WriterState::Idle
        );

        assert_eq!(ring.reader_request(5_000), None);
        let req = ReaderRequest {
            width: 1280,
            height: 720,
            fps: 30,
        };
        ring.reader_beat(Some(req), 5_000);
        assert_eq!(ring.reader_request(6_000), Some(req));
        assert_eq!(ring.reader_request(5_000 + READER_TIMEOUT_MS + 1), None);
        ring.reader_beat(None, 6_000);
        assert_eq!(ring.reader_request(6_000), None);
    }
}
