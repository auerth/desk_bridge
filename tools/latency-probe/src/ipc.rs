//! `input` and `rtt`: read metrics from the running agent over the UI pipe.

use deskbridge_core::ipc::AppState;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn pipe_path(args: &[String]) -> String {
    match arg(args, "--pipe").or_else(|| std::env::var("DESKBRIDGE_PIPE").ok()) {
        Some(p) if p.starts_with("\\\\") => p,
        Some(p) => format!("\\\\.\\pipe\\{p}"),
        None => deskbridge_core::brand::UI_PIPE_NAME.to_string(),
    }
}

async fn get_state(args: &[String]) -> anyhow::Result<AppState> {
    let client = ClientOptions::new().open(pipe_path(args))?;
    let (rd, mut wr) = tokio::io::split(client);
    wr.write_all(b"{\"id\":1,\"req\":{\"type\":\"get_state\"}}\n").await?;
    let mut lines = BufReader::new(rd).lines();
    while let Some(line) = lines.next_line().await? {
        let v: serde_json::Value = serde_json::from_str(&line)?;
        if v.get("id").and_then(serde_json::Value::as_u64) == Some(1) {
            return Ok(serde_json::from_value(v["data"].clone())?);
        }
    }
    anyhow::bail!("agent closed the pipe")
}

pub async fn input(args: &[String]) -> i32 {
    let secs: u64 = arg(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(20);
    println!("Measuring for {secs} s – move the mouse and type so that the other device receives input…");
    for _ in 0..secs {
        match get_state(args).await {
            Ok(s) => {
                let m = &s.metrics;
                println!(
                    "input latency p50={} p95={} max={}   rtt={}   state={:?}",
                    fmt_us(m.input_latency_p50_us),
                    fmt_us(m.input_latency_p95_us),
                    fmt_us(m.input_latency_max_us),
                    fmt_us(s.connection.rtt_us),
                    s.connection.state
                );
            }
            Err(e) => {
                eprintln!("agent not reachable: {e}");
                return 1;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!("Targets (wired): p95 < 3 ms, (wifi): p95 < 15 ms.");
    0
}

pub async fn rtt(args: &[String]) -> i32 {
    loop {
        match get_state(args).await {
            Ok(s) => println!(
                "rtt={} clock_offset_us={:?} state={:?}",
                fmt_us(s.connection.rtt_us),
                s.connection.clock_offset_us,
                s.connection.state
            ),
            Err(e) => {
                eprintln!("agent not reachable: {e}");
                return 1;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn fmt_us(v: Option<u32>) -> String {
    v.map_or_else(|| "-".to_string(), |u| format!("{:.2} ms", f64::from(u) / 1000.0))
}
