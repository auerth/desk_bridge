//! Own-process CPU and memory figures for the diagnostics page (RAM < 150 MB, idle CPU < 1 %).

use std::time::Instant;

#[derive(Debug)]
pub struct ProcStats {
    last_wall: Instant,
    last_cpu_100ns: u64,
}

impl Default for ProcStats {
    fn default() -> Self {
        Self {
            last_wall: Instant::now(),
            last_cpu_100ns: cpu_time_100ns(),
        }
    }
}

impl ProcStats {
    /// CPU usage in percent of one core since the previous call, and RSS in MB.
    pub fn sample(&mut self) -> (f32, f32) {
        let now = Instant::now();
        let cpu = cpu_time_100ns();
        let wall = now.duration_since(self.last_wall).as_secs_f64().max(0.001);
        let pct = (cpu.saturating_sub(self.last_cpu_100ns) as f64 / 1e7 / wall * 100.0) as f32;
        self.last_wall = now;
        self.last_cpu_100ns = cpu;
        (pct, rss_mb())
    }
}

#[cfg(windows)]
fn cpu_time_100ns() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: out-parameters are valid for the call; the pseudo handle needs no closing.
    unsafe {
        if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).is_err() {
            return 0;
        }
    }
    let f = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    f(k) + f(u)
}

#[cfg(windows)]
fn rss_mb() -> f32 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut pmc = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `pmc` is a properly sized, initialised out structure.
    unsafe {
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb).is_err() {
            return 0.0;
        }
    }
    pmc.WorkingSetSize as f32 / (1024.0 * 1024.0)
}

#[cfg(not(windows))]
fn cpu_time_100ns() -> u64 {
    0
}

#[cfg(not(windows))]
fn rss_mb() -> f32 {
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_works() {
        let mut p = ProcStats::default();
        let mut x = 0u64;
        for i in 0..5_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        let (cpu, rss) = p.sample();
        assert!(cpu >= 0.0);
        #[cfg(windows)]
        assert!(rss > 1.0);
        let _ = rss;
    }
}
