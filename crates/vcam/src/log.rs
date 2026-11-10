//! The shared memory to the agent, opened once per process, and logging into its log ring.

use deskbridge_camera::shm::SharedCamera;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static SHARED: OnceLock<SharedCamera> = OnceLock::new();
static LAST_TRY: Mutex<Option<Instant>> = Mutex::new(None);

/// The mapping, created here if the agent has not created it yet (the Frame Server may create
/// global objects). A failed attempt is retried at most once per second.
pub(crate) fn shared() -> Option<&'static SharedCamera> {
    if let Some(s) = SHARED.get() {
        return Some(s);
    }
    {
        let mut last = LAST_TRY.lock().ok()?;
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return None;
        }
        *last = Some(Instant::now());
    }
    match SharedCamera::open_or_create() {
        Ok(s) => {
            let _ = SHARED.set(s);
            SHARED.get()
        }
        Err(_) => None,
    }
}

pub(crate) fn log(line: &str) {
    if let Some(s) = shared() {
        s.ring()
            .log(&format!("[pid {}] {line}", std::process::id()));
    }
}

macro_rules! vlog {
    ($($t:tt)*) => { $crate::log::log(&format!($($t)*)) };
}
pub(crate) use vlog;
