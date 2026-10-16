//! Manual probe for the Windows input capture.
//!
//! ```text
//! cargo run -p deskbridge-platform-windows --example input_probe              # print events for 10 s
//! cargo run -p deskbridge-platform-windows --example input_probe -- --selftest # inject F24 + 1px moves itself
//! cargo run -p deskbridge-platform-windows --example input_probe -- --grab     # grab ALL input for 5 s (!)
//! ```
//! The default mode only listens (nothing is swallowed). `--grab` blocks keyboard and mouse for
//! 5 seconds and hides the cursor; it ends by itself, and the cursor is restored on exit.
//! `--selftest` injects *unmarked* input (so the capture does not filter it): F24 and a 1 px
//! mouse nudge, and verifies hotkey swallowing and mouse grabbing.

use std::time::{Duration, Instant};

use deskbridge_platform::hotkey::Hotkey;
use deskbridge_platform::input::{CaptureEvent, GrabMode, InputCapture};
use deskbridge_platform::{UnboundedReceiver, unbounded_channel};
use deskbridge_platform_windows::input::{
    WinInputCapture, cursor_position, init_dpi_awareness, is_elevated, restore_system_cursors,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};

fn drain(rx: &mut UnboundedReceiver<CaptureEvent>, label: &str, ms: u64) -> Vec<CaptureEvent> {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut out = Vec::new();
    while Instant::now() < end {
        while let Ok(ev) = rx.try_recv() {
            println!("[{label}] {ev:?}");
            out.push(ev);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

/// Unmarked (dwExtraInfo = 0) F24 press via scancode 0x76: practically no application reacts to it.
fn inject_f24(down: bool) {
    let flags = if down {
        KEYEVENTF_SCANCODE
    } else {
        KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP
    };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: 0x76,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: a single fully initialised INPUT.
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

fn inject_mouse_move(dx: i32, dy: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: a single fully initialised INPUT.
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

fn selftest(cap: &WinInputCapture, rx: &mut UnboundedReceiver<CaptureEvent>) {
    let mut ok = true;
    let mut check = |name: &str, cond: bool| {
        println!("{} {name}", if cond { "PASS" } else { "FAIL" });
        ok &= cond;
    };

    // 1. Plain key + mouse capture without grab.
    inject_f24(true);
    inject_f24(false);
    let p0 = cursor_position().unwrap_or_default();
    let dir = if p0.0 > 4 { -1 } else { 1 };
    inject_mouse_move(dir, 0);
    let evs = drain(rx, "plain", 300);
    check(
        "F24 down reported",
        evs.contains(&CaptureEvent::Key {
            hid: 0x73,
            down: true,
        }),
    );
    check(
        "F24 up reported",
        evs.contains(&CaptureEvent::Key {
            hid: 0x73,
            down: false,
        }),
    );
    check(
        "raw mouse delta reported",
        evs.iter()
            .any(|e| matches!(e, CaptureEvent::MouseRel { .. })),
    );
    check(
        "absolute mouse move reported",
        evs.iter()
            .any(|e| matches!(e, CaptureEvent::MouseMove { .. })),
    );
    let p1 = cursor_position().unwrap_or_default();
    inject_mouse_move(-dir, 0);
    drain(rx, "restore", 100);

    // 2. Hotkey F24 is swallowed and reported as Hotkey(42).
    cap.set_hotkeys(vec![Hotkey {
        id: 42,
        mods: 0,
        key: 0x73,
    }]);
    std::thread::sleep(Duration::from_millis(50));
    inject_f24(true);
    inject_f24(false);
    let evs = drain(rx, "hotkey", 300);
    check("hotkey fired", evs.contains(&CaptureEvent::Hotkey(42)));
    check(
        "hotkey key not forwarded",
        !evs.iter()
            .any(|e| matches!(e, CaptureEvent::Key { hid: 0x73, .. })),
    );

    // 3. Grab ALL: the injected move must not move the cursor, but raw deltas are still reported.
    cap.set_grab(GrabMode::All);
    let before = cursor_position().unwrap_or_default();
    inject_mouse_move(dir, 0);
    let evs = drain(rx, "grab", 300);
    let after = cursor_position().unwrap_or_default();
    cap.set_grab(GrabMode::None);
    check("grab keeps the cursor still", before == after);
    check(
        "grab still reports raw deltas",
        evs.iter()
            .any(|e| matches!(e, CaptureEvent::MouseRel { .. })),
    );
    check(
        "grab suppresses absolute moves",
        !evs.iter()
            .any(|e| matches!(e, CaptureEvent::MouseMove { .. })),
    );
    println!("(cursor before/after plain nudge: {p0:?} -> {p1:?})");
    println!("{}", if ok { "SELFTEST OK" } else { "SELFTEST FAILED" });
}

fn main() {
    init_dpi_awareness();
    let args: Vec<String> = std::env::args().collect();
    let cap = WinInputCapture::new();
    let (tx, mut rx) = unbounded_channel();
    // Recover a cursor that a previous crashed run might have left hidden.
    restore_system_cursors();
    println!("elevated: {}", is_elevated());
    cap.start(tx).expect("start capture");

    if args.iter().any(|a| a == "--selftest") {
        selftest(&cap, &mut rx);
    } else if args.iter().any(|a| a == "--grab") {
        println!("GRABBING all input and hiding the cursor for 5 s ...");
        cap.set_grab(GrabMode::All);
        cap.set_cursor_visible(false);
        drain(&mut rx, "grab", 5000);
        cap.set_cursor_visible(true);
        cap.set_grab(GrabMode::None);
    } else {
        println!(
            "listening for 10 s (type, move the mouse, scroll, press AltGr+Q, Pause, media keys) ..."
        );
        drain(&mut rx, "evt", 10_000);
    }
    cap.stop();
}
