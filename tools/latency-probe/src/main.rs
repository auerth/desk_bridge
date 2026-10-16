//! Latency measurements for Desk Bridge (plan section 14).
//!
//! ```text
//! latency-probe input [--pipe NAME] [--seconds 20]
//!     Polls the running agent. Move the mouse / type on the PC while the laptop is controlled;
//!     prints p50/p95/max of the one-way input latency measured by the receiving agent and the RTT.
//! latency-probe rtt [--pipe NAME]
//!     Prints RTT, clock offset and their history once per second.
//! latency-probe audio-emit [--count 10] [--interval-ms 1000]
//!     Plays short clicks on the default output and prints the local timestamp (µs) of each.
//! latency-probe audio-detect [--seconds 12] [--loopback] [--device ID]
//!     Records (default microphone, or loopback of the default output) and prints the local
//!     timestamps (µs) of detected click onsets.
//! latency-probe audio-compare EMIT.txt DETECT.txt [--offset-us N]
//!     Pairs clicks and prints the delay statistics. `--offset-us` is
//!     (detector clock − emitter clock) from `latency-probe rtt` when run on different machines.
//! ```
//! The Klick-Test of the plan: run `audio-emit` on the laptop (default output = what is captured and
//! sent to the headset), `audio-detect` on the PC while recording the headset path, then compare.

#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod audio;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    let code = rt.block_on(async {
        match cmd {
            "input" => ipc::input(&args[1..]).await,
            "rtt" => ipc::rtt(&args[1..]).await,
            "audio-emit" => audio::emit(&args[1..]).await,
            "audio-detect" => audio::detect(&args[1..]).await,
            "audio-compare" => audio::compare(&args[1..]),
            _ => {
                println!("{}", include_str!("help.txt"));
                0
            }
        }
    });
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("latency-probe supports Windows only");
}
