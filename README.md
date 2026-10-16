# Desk Bridge

**Ein Schreibtisch. Zwei Rechner. Null Umschalten.**

One desk, two computers, zero switching. Install Desk Bridge on both Windows machines, pair them
once with a six-digit code, and your mouse, keyboard, clipboard, files and headset work across both.
Everything stays on your LAN: encrypted end to end, no cloud, no account, no telemetry.

> By [softwelop](https://softwelop.com). Licensed under MIT OR Apache-2.0.

## What it does

| | Feature |
|---|---|
| F1 | **Share mouse and keyboard.** Push the mouse over the screen edge and you control the laptop; it behaves like one more monitor. |
| F2 | **Laptop sound in your headset**, mixed with the PC sound. |
| F3 | **Headset microphone for the laptop** as "Desk Bridge Mikrofon" for Teams, Zoom, Discord (depends on a signed virtual audio driver, see [ADR 0003](docs/adr/0003-virtual-audio-driver.md)). |
| F4 | **Shared clipboard**: text, rich text, images, files. |
| F5 | **Send files** with copy/paste in Explorer, drag and drop, or "Send to". |
| F6 | **Focus mode**: the laptop full screen (or half) on your ultrawide in native 5120x1440 at 120 Hz through a virtual display. |
| F7 | **A simple, good-looking UI** in softwelop branding: install, pair, go. |

One installer does everything: driver, firewall rule, autostart, no manual prerequisites.

## Status

Early development, following [`docs/PLAN.md`](docs/PLAN.md).

| Milestone | State |
|---|---|
| M0 Foundation (workspace, UI shell, installer helper, CI) | in progress |
| M1 Connection (identity, pairing, QUIC session, reconnect) | in progress |
| M2 Mouse, keyboard, text clipboard | planned |
| M3 Audio and microphone | planned |
| M4 Clipboard and files | planned |
| M5 Focus mode | planned |
| M6 Robustness, M7 release | planned |

Platform: Windows 10 22H2+ and Windows 11 (x64). macOS and Linux later.
There is no release yet; see [CHANGELOG.md](CHANGELOG.md).

## Quick start (developers)

```powershell
git clone https://github.com/softwelop/desk-bridge.git
cd desk-bridge
cd app; npm ci; npm run build; cd ..       # the Tauri crate embeds app/dist
cargo test --workspace
.\scripts\dev-two-instances.ps1 -Clean      # two agents on this PC for development
```

Build the installer: `.\scripts\stage-installer.ps1 -NoFfmpeg`, then `cd app; npm run tauri build -- --bundles nsis`.
Full instructions (cmake for libopus, mock mode, deploy to a laptop): [docs/development.md](docs/development.md).

## Documentation

* [Plan](docs/PLAN.md) - vision, requirements, milestones (German)
* [Architecture](docs/architecture.md) - processes, crates, threading, state machines
* [Protocol](docs/protocol.md) - QUIC channels, framing, pairing
* [Testing](docs/testing.md) - automated tests and the hardware checklists
* [Decisions (ADRs)](docs/adr/README.md)
* [Installer](installer/README.md)

## Contributing and security

Contributions are welcome, see [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). Please report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Bundled
third-party components keep their own licences (FFmpeg LGPL build as DLLs, Montserrat under the
SIL OFL, drivers under the licences in `drivers/*/`).
