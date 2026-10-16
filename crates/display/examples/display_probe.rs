//! Display probe for focus mode.
//!
//! Read-only commands (safe on any machine):
//!   display_probe list
//!   display_probe plan --mode 5120x1440@120 [--split 50|67|33] [--side left|right] [--even]
//!
//! Commands that CHANGE the display configuration or the driver state. They run only with
//! `--really-do-it` and after typing `yes` at a prompt (needs an elevated shell):
//!   display_probe enable   --mode 5120x1440@120 [--adapter NAME] --really-do-it
//!   display_probe disable  --really-do-it
//!   display_probe focus    --mode 5120x1440@120 [--keep-internal] [--topology FILE] --really-do-it
//!       full session: save topology -> enable -> switch -> wait for Enter -> restore
//!   display_probe recover  [--topology FILE] --really-do-it
//!       finish a crashed focus session (restore saved topology, disable the virtual display)

use deskbridge_display::plan::{
    FocusLayout, PlanOptions, Side, SplitShare, parse_mode, plan_focus,
};

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  display_probe list\n  display_probe plan --mode WxH@HZ [--split 50|67|33] [--side left|right] [--even]\n  \
         display_probe enable|disable|focus|recover ... --really-do-it   (changes the system!)"
    );
    std::process::exit(2);
}

fn cmd_plan(args: &[String]) {
    let mode = arg_value(args, "--mode")
        .as_deref()
        .and_then(parse_mode)
        .unwrap_or_else(|| usage());
    let layout = match arg_value(args, "--split") {
        None => FocusLayout::Full,
        Some(p) => {
            let share = p
                .parse::<u32>()
                .ok()
                .and_then(SplitShare::from_percent)
                .unwrap_or_else(|| {
                    eprintln!("--split must be 50, 67 or 33");
                    std::process::exit(2);
                });
            let side = match arg_value(args, "--side").as_deref() {
                None | Some("left") => Side::Left,
                Some("right") => Side::Right,
                _ => usage(),
            };
            FocusLayout::Split { side, share }
        }
    };
    let opts = PlanOptions {
        even_widths: has(args, "--even"),
    };
    match plan_focus(mode, layout, opts) {
        Ok(p) => {
            println!(
                "PC monitor mode : {}x{}@{}",
                p.pc_mode.width, p.pc_mode.height, p.pc_mode.hz
            );
            println!("layout          : {:?}", p.layout);
            println!(
                "virtual mode    : {}x{}@{}",
                p.virtual_mode.width, p.virtual_mode.height, p.virtual_mode.hz
            );
            let r = p.viewer_region;
            println!(
                "viewer region   : x={} y={} w={} h={} (relative to the monitor's top-left)",
                r.x, r.y, r.w, r.h
            );
            if let Some(r) = p.pc_region {
                println!("PC desktop part : x={} y={} w={} h={}", r.x, r.y, r.w, r.h);
            }
            println!("driver modes ({}):", p.driver_modes.len());
            for m in &p.driver_modes {
                println!("  {}x{}@{}", m.width, m.height, m.hz);
            }
        }
        Err(e) => {
            eprintln!("cannot plan: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
mod win {
    use std::io::Write;
    use std::sync::Arc;

    use deskbridge_display::plan::parse_mode;
    use deskbridge_display::session::{FocusDisplaySession, FocusOptions, recover_on_startup};
    use deskbridge_platform::display::DisplayManager;
    use deskbridge_platform_windows::display::WinDisplayManager;

    use super::{arg_value, has, usage};

    pub fn list() {
        let m = WinDisplayManager::new();
        print!("{}", m.report().render());
    }

    fn confirm(what: &str) {
        println!("This will CHANGE the display configuration / driver state: {what}");
        print!("Type 'yes' to continue: ");
        let _ = std::io::stdout().flush();
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        if s.trim() != "yes" {
            println!("aborted, nothing changed");
            std::process::exit(1);
        }
    }

    fn topology_path(args: &[String]) -> std::path::PathBuf {
        arg_value(args, "--topology").map_or_else(
            || {
                std::env::temp_dir()
                    .join("deskbridge-display-probe")
                    .join("topology.json")
            },
            Into::into,
        )
    }

    pub fn change(cmd: &str, args: &[String]) {
        if !has(args, "--really-do-it") {
            eprintln!("'{cmd}' changes the system; add --really-do-it to run it");
            std::process::exit(2);
        }
        let dm: Arc<dyn DisplayManager> = Arc::new(WinDisplayManager::new());
        match cmd {
            "enable" => {
                let mode = arg_value(args, "--mode")
                    .as_deref()
                    .and_then(parse_mode)
                    .unwrap_or_else(|| usage());
                confirm("enable the virtual display (the topology is NOT saved by this command)");
                let id = dm.enable_virtual(mode, arg_value(args, "--adapter").as_deref());
                println!("enable_virtual -> {id:?}");
            }
            "disable" => {
                confirm("disable the virtual display device");
                println!("disable_virtual -> {:?}", dm.disable_virtual());
            }
            "focus" => {
                let mode = arg_value(args, "--mode")
                    .as_deref()
                    .and_then(parse_mode)
                    .unwrap_or_else(|| usage());
                let path = topology_path(args);
                confirm("switch to the virtual display and back (topology file below)");
                println!("topology file: {}", path.display());
                let session = FocusDisplaySession::new(dm, path);
                let opts = FocusOptions {
                    disable_internal: !has(args, "--keep-internal"),
                    render_adapter_hint: arg_value(args, "--adapter"),
                };
                match session.begin(mode, &opts) {
                    Ok(id) => {
                        println!("focus display ready (screen {id}). Press Enter to restore.");
                        let mut s = String::new();
                        let _ = std::io::stdin().read_line(&mut s);
                        println!("end -> {:?}", session.end());
                    }
                    Err(e) => println!("begin failed (rolled back): {e}"),
                }
            }
            "recover" => {
                let path = topology_path(args);
                confirm("finish a crashed focus session");
                println!("recover -> {:?}", recover_on_startup(&path, &dm));
            }
            _ => usage(),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        usage();
    };
    match cmd {
        "plan" => cmd_plan(&args),
        #[cfg(windows)]
        "list" => win::list(),
        #[cfg(windows)]
        "enable" | "disable" | "focus" | "recover" => win::change(cmd, &args),
        _ => usage(),
    }
}
