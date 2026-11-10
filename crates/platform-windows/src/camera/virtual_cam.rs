//! Laptop side: pictures into "Desk Bridge Kamera" and "an app streams from it" out of it.

use super::register;
use deskbridge_camera::shm::SharedCamera;
use deskbridge_camera::{MAX_HEIGHT, MAX_WIDTH, WriterState, nv12, tick_ms};
use deskbridge_platform::camera::{CameraFeedState, CameraUsage, VirtualCamera};
use deskbridge_platform::video::{Frame, FrameData};
use deskbridge_platform::{Result, UnboundedSender};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Inner {
    shm: Mutex<Option<SharedCamera>>,
    last_open: Mutex<Option<Instant>>,
    feed: AtomicU32,
    watchers: Mutex<Vec<UnboundedSender<CameraUsage>>>,
    last_usage: Mutex<Option<CameraUsage>>,
    log_pos: Mutex<Option<u64>>,
}

/// `VirtualCamera` on Windows 11.
#[derive(Clone, Default)]
pub struct WinVirtualCamera {
    inner: Arc<Inner>,
    started: Arc<std::sync::OnceLock<()>>,
}

fn writer_state(s: CameraFeedState) -> WriterState {
    match s {
        CameraFeedState::Idle => WriterState::Idle,
        CameraFeedState::Waiting => WriterState::Waiting,
        CameraFeedState::Streaming => WriterState::Streaming,
        CameraFeedState::Unavailable => WriterState::Unavailable,
    }
}

impl Inner {
    /// The mapping, opened (or created) on demand; retried at most every 2 s.
    fn with_ring<R>(&self, f: impl FnOnce(&deskbridge_camera::Ring) -> R) -> Option<R> {
        let mut g = self.shm.lock();
        if g.is_none() {
            let mut last = self.last_open.lock();
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                return None;
            }
            *last = Some(Instant::now());
            match SharedCamera::open_or_create() {
                Ok(s) => {
                    tracing::info!("virtual camera: shared memory open");
                    *g = Some(s);
                }
                Err(e) => {
                    tracing::debug!("virtual camera: shared memory not available yet: {e}");
                    return None;
                }
            }
        }
        g.as_ref().map(|s| f(s.ring()))
    }

    fn feed(&self) -> WriterState {
        WriterState::from_u32(self.feed.load(Ordering::Relaxed))
    }

    /// Heartbeat, the virtual camera's log lines and usage changes, every 250 ms.
    fn tick(&self) {
        let now = tick_ms();
        let feed = self.feed();
        let polled = self.with_ring(|ring| {
            // Streaming beats with every picture; the other states beat here.
            if feed != WriterState::Streaming {
                ring.set_writer_state(feed, now);
            } else {
                ring.header()
                    .writer_state
                    .store(feed as u32, Ordering::Release);
            }
            let mut pos = self.log_pos.lock();
            let from = pos.unwrap_or_else(|| {
                ring.header()
                    .log_written
                    .load(Ordering::Acquire)
                    .saturating_sub(1024)
            });
            let (text, end) = ring.log_since(from);
            *pos = Some(end);
            for line in text.lines().filter(|l| !l.is_empty()) {
                tracing::info!(target: "vcam", "{line}");
            }
            ring.reader_request(now)
        });
        let req = polled.flatten();
        let usage = CameraUsage {
            in_use: req.is_some(),
            width: req.map_or(0, |r| r.width),
            height: req.map_or(0, |r| r.height),
            fps: req.map_or(0, |r| r.fps),
            process: if req.is_some() {
                super::usage::camera_app()
            } else {
                None
            },
        };
        let mut last = self.last_usage.lock();
        if last.as_ref() != Some(&usage) {
            *last = Some(usage.clone());
            drop(last);
            self.watchers
                .lock()
                .retain(|w| w.send(usage.clone()).is_ok());
        }
    }
}

impl WinVirtualCamera {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_thread(&self) {
        let inner = Arc::downgrade(&self.inner);
        self.started.get_or_init(move || {
            let _ = std::thread::Builder::new()
                .name("vcam-writer".into())
                .spawn(move || {
                    while let Some(i) = inner.upgrade() {
                        i.tick();
                        drop(i);
                        std::thread::sleep(Duration::from_millis(250));
                    }
                });
        });
    }
}

impl VirtualCamera for WinVirtualCamera {
    fn available(&self) -> bool {
        register::os_supported() && register::registered_dll().is_some_and(|p| p.is_file())
    }

    fn unavailable_reason(&self) -> Option<String> {
        if !register::os_supported() {
            return Some("Die virtuelle Kamera braucht Windows 11".into());
        }
        match register::registered_dll() {
            None => Some("Die virtuelle Kamera ist nicht eingerichtet (Desk Bridge einmal als Administrator starten oder neu installieren)".into()),
            Some(p) if !p.is_file() => Some(format!("Kamera-Modul fehlt: {}", p.display())),
            Some(_) => None,
        }
    }

    fn set_enabled(&self, on: bool) -> Result<()> {
        let cam = register::VirtualCameraDevice::open(true)?;
        if on { cam.start() } else { cam.stop() }
    }

    fn set_feed_state(&self, state: CameraFeedState) {
        self.ensure_thread();
        let s = writer_state(state);
        if self.inner.feed.swap(s as u32, Ordering::Relaxed) != s as u32 {
            tracing::info!(?state, "virtual camera feed");
            self.inner
                .with_ring(|ring| ring.set_writer_state(s, tick_ms()));
        }
    }

    fn write_frame(&self, frame: &Frame) {
        let FrameData::Bgra { data, stride } = &frame.data else {
            return;
        };
        let (sw, sh, stride) = (frame.width, frame.height, *stride);
        // Pictures mean "streaming": the heartbeat thread must not put the placeholder state back
        // in between them (the engine sets the next state once they stop).
        self.ensure_thread();
        self.inner
            .feed
            .store(WriterState::Streaming as u32, Ordering::Relaxed);
        self.inner.with_ring(|ring| {
            let now = tick_ms();
            // The size the app chose; without an app, the picture's own size (kept in range).
            let (w, h) = match ring.reader_request(now) {
                Some(r) if r.width > 0 && r.height > 0 => (r.width, r.height),
                _ => (sw, sh),
            };
            let (w, h) = (w.min(MAX_WIDTH) & !1, h.min(MAX_HEIGHT) & !1);
            if w == 0 || h == 0 {
                return;
            }
            ring.header()
                .writer_state
                .store(WriterState::Streaming as u32, Ordering::Release);
            ring.write_frame(w, h, frame.ts_us, now, |dst| {
                nv12::from_bgra(data, sw, sh, stride, dst, w, h)
            });
        });
    }

    fn usage(&self) -> CameraUsage {
        self.ensure_thread();
        self.inner.last_usage.lock().clone().unwrap_or(CameraUsage {
            in_use: false,
            width: 0,
            height: 0,
            fps: 0,
            process: None,
        })
    }

    fn watch_usage(&self, tx: UnboundedSender<CameraUsage>) -> Result<()> {
        self.ensure_thread();
        if let Some(u) = self.inner.last_usage.lock().clone() {
            let _ = tx.send(u);
        }
        self.inner.watchers.lock().push(tx);
        Ok(())
    }
}
