//! Headless peer for tests and demos. Runs a full engine on a mock platform.
//!
//! ```text
//! fake-peer --name Laptop --port 47831 --show-code          # waits for a pairing, prints the code
//! fake-peer --name PC --pc --pair 127.0.0.1:47831 123456     # pairs with it
//! fake-peer ... --connect 127.0.0.1:47831                    # adds a manual host for sessions
//! fake-peer ... --record events.jsonl                        # writes every injected input op
//! fake-peer ... --play events.jsonl                          # feeds captured events (PC role)
//! ```
//! Capture-event lines for `--play`: `{"t_ms":0,"ev":"move","x":2559,"y":720}`,
//! `{"t_ms":10,"ev":"key","hid":4,"down":true}`, `{"t_ms":20,"ev":"rel","dx":3,"dy":0}`.

use deskbridge_core::config::Config;
use deskbridge_core::engine::{self, EngineParams};
use deskbridge_core::ipc::UiRequest;
use deskbridge_core::paths::AppPaths;
use deskbridge_core::platform::Platform;
use deskbridge_platform::input::CaptureEvent;
use deskbridge_platform::mock::InjectedOp;
use std::io::Write;
use std::time::Duration;

struct Args {
    name: String,
    port: u16,
    pc: bool,
    show_code: bool,
    pair: Option<(String, String)>,
    connect: Vec<String>,
    record: Option<String>,
    play: Option<String>,
    width: u32,
    height: u32,
}

fn parse() -> Args {
    let mut a = Args {
        name: "fake-peer".into(),
        port: 0,
        pc: false,
        show_code: false,
        pair: None,
        connect: vec![],
        record: None,
        play: None,
        width: 1920,
        height: 1080,
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < v.len() {
        match v[i].as_str() {
            "--name" => {
                i += 1;
                a.name = v[i].clone();
            }
            "--port" => {
                i += 1;
                a.port = v[i].parse().unwrap_or(0);
            }
            "--pc" => a.pc = true,
            "--show-code" => a.show_code = true,
            "--pair" => {
                a.pair = Some((v[i + 1].clone(), v[i + 2].clone()));
                i += 2;
            }
            "--connect" => {
                i += 1;
                a.connect.push(v[i].clone());
            }
            "--record" => {
                i += 1;
                a.record = Some(v[i].clone());
            }
            "--play" => {
                i += 1;
                a.play = Some(v[i].clone());
            }
            "--size" => {
                let (w, h) = v[i + 1].split_once('x').unwrap_or(("1920", "1080"));
                a.width = w.parse().unwrap_or(1920);
                a.height = h.parse().unwrap_or(1080);
                i += 1;
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
        i += 1;
    }
    a
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse();
    let dir = std::env::temp_dir().join(format!("deskbridge-fake-{}-{}", args.name, std::process::id()));
    let (platform, mocks) = Platform::mock(args.width, args.height);
    let mut cfg = Config::default();
    cfg.device.name = args.name.clone();
    cfg.device.input_source = args.pc;
    cfg.device.headset_sink = args.pc;
    cfg.device.stream_source = !args.pc;
    let h = engine::start(EngineParams {
        paths: AppPaths::under(&dir),
        config: cfg,
        platform,
        bind: ([0, 0, 0, 0], args.port).into(),
        enable_discovery: false,
        identity: None,
    })
    .await?;
    println!("listening on {}", h.local_addr());

    if args.show_code {
        let r = h.request(UiRequest::StartPairing).await.map_err(anyhow::Error::msg)?;
        println!("pairing code: {}", r["code"].as_str().unwrap_or("?"));
    }
    if let Some((addr, code)) = &args.pair {
        h.request(UiRequest::PairWith { device_id: None, address: Some(addr.clone()), code: code.clone() })
            .await
            .map_err(anyhow::Error::msg)?;
    }
    for c in &args.connect {
        h.request(UiRequest::AddManualHost { host: c.clone() }).await.map_err(anyhow::Error::msg)?;
    }

    // recorder
    if let Some(path) = args.record.clone() {
        let inj = mocks.injector.clone();
        tokio::spawn(async move {
            let mut f = std::fs::File::create(path).expect("record file");
            let mut seen = 0usize;
            loop {
                let ops = inj.ops();
                for op in &ops[seen.min(ops.len())..] {
                    let line = match op {
                        InjectedOp::Key { hid, down } => format!("{{\"ev\":\"key\",\"hid\":{hid},\"down\":{down}}}"),
                        InjectedOp::MouseRel { dx, dy } => format!("{{\"ev\":\"rel\",\"dx\":{dx},\"dy\":{dy}}}"),
                        InjectedOp::MouseAbs { x, y } => format!("{{\"ev\":\"abs\",\"x\":{x},\"y\":{y}}}"),
                        InjectedOp::Button { button, down } => format!("{{\"ev\":\"btn\",\"b\":\"{button:?}\",\"down\":{down}}}"),
                        InjectedOp::Wheel { dx, dy } => format!("{{\"ev\":\"wheel\",\"dx\":{dx},\"dy\":{dy}}}"),
                    };
                    let _ = writeln!(f, "{line}");
                }
                seen = ops.len();
                let _ = f.flush();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
    }
    // player
    if let Some(path) = args.play.clone() {
        let cap = mocks.capture.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let start = std::time::Instant::now();
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                let t = v["t_ms"].as_u64().unwrap_or(0);
                if let Some(wait) = Duration::from_millis(t).checked_sub(start.elapsed()) {
                    tokio::time::sleep(wait).await;
                }
                let ev = match v["ev"].as_str() {
                    Some("move") => CaptureEvent::MouseMove { x: v["x"].as_i64().unwrap_or(0) as i32, y: v["y"].as_i64().unwrap_or(0) as i32 },
                    Some("rel") => CaptureEvent::MouseRel { dx: v["dx"].as_i64().unwrap_or(0) as i32, dy: v["dy"].as_i64().unwrap_or(0) as i32 },
                    Some("key") => CaptureEvent::Key { hid: v["hid"].as_u64().unwrap_or(0) as u16, down: v["down"].as_bool().unwrap_or(true) },
                    _ => continue,
                };
                cap.feed(ev);
            }
            println!("playback finished");
        });
    }

    // status line every 2 s until Ctrl+C
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let s = h.state().await;
                println!("state={:?} peers={} rtt_us={:?} location={}", s.connection.state, s.peers.len(), s.connection.rtt_us, s.location);
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    h.stop();
    Ok(())
}
