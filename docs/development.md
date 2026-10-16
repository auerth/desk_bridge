# Development guide

## Prerequisites (Windows 10 22H2 / Windows 11)

| Tool | Version | Notes |
|---|---|---|
| Rust | stable >= 1.85 (edition 2024) | `rust-toolchain.toml` pins `stable` with rustfmt + clippy; install via rustup |
| MSVC build tools | VS 2022 Build Tools, "Desktop development with C++" | needed by Rust and by `libopus` |
| CMake | any recent | needed to build `libopus`, see below |
| Node.js | 24 | for `app/` (Vite, Tauri CLI is an npm devDependency) |
| Git | - | `.gitattributes` forces LF; `*.ps1`, `*.nsh`, `*.nsi` are CRLF |

You do **not** need NSIS, WiX, devcon or a WebView2 installer: the Tauri bundler downloads what it
needs on first use.

### cmake / PATH note (libopus)

`audiopus_sys` builds the Opus library from source and needs `cmake` on `PATH`. GitHub's
`windows-latest` runners have it. On a development PC install it with `winget install Kitware.CMake`
or `pip install --user cmake`. The pip variant puts it into a directory that is *not* on `PATH` by
default (for Microsoft Store Python: `%LOCALAPPDATA%\Packages\PythonSoftwareFoundation.Python.3.13_*\LocalCache\local-packages\Python313\Scripts`).
The helper scripts dot-source `scripts/dev-env.ps1`, which finds it and prepends it to `PATH`; in your
own shell run `. .\scripts\dev-env.ps1` first. `.cargo/config.toml` sets
`CMAKE_POLICY_VERSION_MINIMUM=3.5` so CMake 4.x can configure libopus. If a build fails in a `*-sys`
crate with "is `cmake` not installed?", this is the cause.

## Build and test

```powershell
cargo build --workspace                 # debug
cargo test --workspace                  # unit + integration tests (mock platform, 127.0.0.1)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check                        # licences, bans, sources (deny.toml)
cargo audit                             # RustSec advisories

# a single crate
cargo test -p deskbridge-net
cargo test -p deskbridge-setup
```

Static CRT is configured in `.cargo/config.toml` (`+crt-static`), so binaries run on a machine
without the Visual C++ redistributable. Do not remove it.

Compile times: `[profile.dev.package."*"] opt-level = 1` keeps dependencies fast enough in debug
builds; release uses thin LTO.

## The UI (`app/`)

```powershell
cd app
npm ci
npm run dev            # Vite dev server on http://localhost:1420 (browser, uses the mock backend)
npm run build          # type check + production bundle into app/dist
npm run tauri dev      # full Tauri app (needs the Rust toolchain)
npm run tauri build -- --bundles nsis
```

**Mock mode:** outside Tauri (plain browser) or with `?mock` in the URL the UI uses
`app/src/api/mock.ts` instead of the agent pipe, so every screen can be developed and screenshotted
without a running agent. The Rust side is tested the same way with `deskbridge_platform::mock`.

`cargo build --workspace` compiles the Tauri crate, which embeds `app/dist`; run `npm run build`
once before the first workspace build (CI does the same).

## Running two instances on one PC

```powershell
.\scripts\dev-two-instances.ps1 -Clean            # agents A (47820) and B (47821); Ctrl+C stops both
.\scripts\dev-two-instances.ps1 -WithUi           # also start two UI processes
```

Each instance gets its own state through three environment variables that the agent honours:

| Variable | A | B | Meaning |
|---|---|---|---|
| `DESKBRIDGE_DATA_ROOT` | `%TEMP%\deskbridge-dev\a` | `...\b` | root for `config\` and `data\` (identity, peers, logs) |
| `DESKBRIDGE_PORT` | `47820` | `47821` | UDP port of the QUIC listener |
| `DESKBRIDGE_PIPE` | `\\.\pipe\deskbridge-ui-a` | `...-ui-b` | UI IPC pipe |

Pair them via manual address (`127.0.0.1:47821`); mDNS between two processes of one host is not
reliable. Because both agents can inject input into the same desktop, do not enable the
mouse/keyboard feature on both at once unless you want a feedback loop; use the mock platform for
input experiments.

To see what the installer layer would do without changing the machine:

```powershell
cargo run -p deskbridge-setup -- install --dry-run
cargo run -p deskbridge-setup -- status
```

## Deploying to the laptop

```powershell
.\scripts\deploy-laptop.ps1 -Target \\LAPTOP\deskbridge -Run          # build (release) + robocopy /MIR + restart
.\scripts\deploy-laptop.ps1 -Destination D:\test -Config Debug -NoBuild
```

First time on the laptop, once, elevated, from the deployed folder: `.\deskbridge-setup.exe install`
(firewall rule + autostart task). Later deploys only replace the files; user data is never in the
deploy folder. The script refuses to mirror into a non-empty folder without its marker file.

## Building the installer locally

```powershell
.\scripts\stage-installer.ps1 -NoFfmpeg      # release build of agent/service/setup -> installer/stage/
cd app; npm run tauri build -- --bundles nsis
```

Output: `target\release\bundle\nsis\Desk Bridge_<version>_x64-setup.exe`. Details, the hook flow and
the `tauri.conf.json` resource mapping are in [`installer/README.md`](../installer/README.md).
FFmpeg (M5a): put the LGPL **shared** build into `vendor/ffmpeg/bin`, run
`scripts/check-ffmpeg-license.ps1`, then stage without `-NoFfmpeg`.

## Logs and data locations

| What | Where |
|---|---|
| Config, peers, identity | `%APPDATA%\softwelop\DeskBridge` |
| Logs (agent, UI), `Incoming`, `topology.json`, `lid-backup.json` | `%LOCALAPPDATA%\softwelop\DeskBridge` (`logs\` rolling, 7 days) |
| Installer helper log | `%LOCALAPPDATA%\softwelop\DeskBridge\logs\setup.log` |
| Installed-items manifest | `<install dir>\installed.json` |

Set `RUST_LOG=debug` (or `deskbridge_net=trace`) for more output.

## Conventions

* Code, comments and commit messages in English; UI strings in German (i18n-ready), English in M7.
* The product name appears only in `crates/core/src/brand.rs` and `app/src/brand.ts`
  (`crates/setup/src/brand.rs` mirrors the installer-relevant constants; a unit test keeps it in sync).
* No Windows types outside `crates/platform-windows` (plan 17). New OS access = new trait + mock first.
* Libraries use `thiserror`, binaries `anyhow`. No `unwrap()` on I/O or network results.
* A new dependency that cannot be shipped inside the installer needs an ADR and sign-off
  (plan 12.1). Check licences with `cargo deny check`.
* Deviations from `docs/PLAN.md` need an ADR (`docs/adr/`), see its README.
* Keep commits small; one branch and one PR per milestone (plan section 0). Update `CHANGELOG.md`.
