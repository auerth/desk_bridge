# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it reaches 1.0.

## [Unreleased]

### Added - M0 Foundation

- Cargo workspace (Rust stable, edition 2024) with crates `proto`, `net`, `platform`,
  `platform-windows`, `input`, `audio`, `clipboard`, `files`, `video`, `display`, `core`, `agent`,
  `service`, `setup` and tools `latency-probe`, `fake-peer`; `rustfmt`, `clippy` and `.editorconfig`
  configuration, static CRT (`+crt-static`), `cargo deny` licence allow-list, MIT OR Apache-2.0.
- Tauri 2 UI shell (React 19, TypeScript, Vite): softwelop brand tokens, Montserrat (OFL), light and
  dark theme, navigation with empty screens, tray, single instance, custom title bar, Mica, mock
  backend for development in a plain browser.
- `deskbridge-setup.exe` installer helper: firewall rule (UDP 47820 + mDNS, Private profile,
  program-scoped, COM with `netsh` fallback), elevated logon task `DeskBridge Agent`, SendTo
  shortcut, driver store + root-enumerated device installation from a driver catalogue
  (`driver install|uninstall|inspect`), service management stub for M6, lid-close action restore,
  network category check (`check-network`, `make-private`), read-only `status`, global `--dry-run`,
  `installed.json` manifest, setup log.
- NSIS hooks (`installer/hooks.nsh`): stop running processes before upgrade, run `install` after
  copying files, `uninstall` with optional user-data deletion; user data survives upgrades.
- `scripts/stage-installer.ps1`, `deploy-laptop.ps1`, `dev-two-instances.ps1`,
  `check-ffmpeg-license.ps1`, placeholders for `measure-input-latency.ps1` and
  `measure-audio-latency.ps1`.
- GitHub Actions: CI (fmt, clippy `-D warnings`, tests, `cargo deny`, `cargo audit`, web build,
  NSIS installer artefact, FFmpeg licence check), release workflow with `latest.json` for the Tauri
  updater, Dependabot (cargo, npm, actions).
- Documentation: architecture, protocol, testing checklists, development guide, ADR 0001 (tech
  stack), ADR 0003 (virtual audio driver, proposed), ADR 0004 (virtual display driver, proposed),
  README, CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, issue and PR templates.

### Added - M1 Connection

- `proto`: protobuf schema `deskbridge.v1`, `u32 LE` length framing, audio/video binary headers,
  ALPNs `deskbridge/1` and `deskbridge-pair/1`, protocol versioning with capability negotiation.
- `net`: device identity (UUIDv7 + self-signed Ed25519 certificate, DPAPI-protected), `peers.toml`,
  rustls certificate pinning, SPAKE2 pairing with a six-digit code (2 min validity, 3 failures lock
  for 60 s), QUIC transport with control and input streams, datagrams, mDNS announce/browse.
- `core`: engine actor, configuration with schema version, connection state machine with
  heartbeat (degraded after 3 s, reconnect after 6 s) and exponential backoff, UI protocol over a
  named pipe (NDJSON), mock platform for tests.

### Added - M2 Mouse, keyboard, text clipboard

- `input`: shared layout in 96-dpi-normalised units with relative-position edge mapping, source
  router (local/remote/absolute region, dwell delay, required modifier, corner lock, double bump,
  held-modifier replay), target receiver (injection only between Enter and Leave, return-edge
  detection with `EdgeHit`, lock-key sync, `release_all` on Leave/disconnect/Emergency), hotkey
  actions with conflict detection.
- `platform-windows`: low-level hooks + Raw Input capture with stuck-key-safe grab, `SendInput`
  injector (scancodes, extended keys, AltGr, XButtons, wheel), German 105-key HID keymap, screens
  with per-monitor DPI and change watching, DPAPI secrets, power/lid/network events.
- `agent`: elevated agent binary with named-pipe UI server (user-only ACL), single instance,
  `send <paths>` and `--stop` subcommands, environment overrides for two-instance development.
- Input latency measurement (Ping/Pong clock offset) shown in diagnostics; `latency-probe`.

### Added - M3 Audio

- `audio`: Opus (static libopus) and PCM paths with seamless auto-switching, adaptive jitter buffer
  with PLC, drift compensation (own Kaiser-sinc resampler), silence suppression; simulated-network
  tests including +-200 ppm drift, loss, reordering and a 10-minute soak.
- `platform-windows`: WASAPI loopback/microphone capture, event-driven render with `IAudioClient3`,
  device enumeration and hot-plug (`IMMNotificationClient`), `IPolicyConfig` default-device setting,
  mic-session enumeration.
- Engine: laptop sound to the headset with pause/resume when the headset disappears, microphone
  only while the laptop uses it, mute hotkeys, test tone. ADR 0002 (loopback and mute),
  ADR 0003 (virtual audio driver spike: no driver meets all criteria, F3 deferred, conditional
  proposal for the owner).

### Added - M4 Clipboard and files

- `clipboard`: sync policy (eager text, delayed rendering, loop avoidance, size and privacy rules);
  `files`: chunked transfers with BLAKE3, resume, path hardening, history.
- `platform-windows`: clipboard listener with CF_HTML/RTF/PNG/DIB conversion, `CF_HDROP`, sensitive
  flags, virtual files through a custom `IDataObject` (Explorer's own copy dialog).
- Send files from the UI/SendTo; partner update offers with SHA-256 verification.

### Added - M5 Focus mode

- `video`: capability negotiation, adaptive bitrate, IDR throttling and keep-alives, late-frame
  filter, FFmpeg (LGPL, shared) encode/decode, DXGI capture, D3D11 presenter (flip model, tearing,
  AppBar split); `display`: focus plan (full/split), virtual display control, topology save/restore
  with crash recovery; ADR 0004 (virtual display driver), ADR 0005 (video spike).
- Engine: full, split and window focus mode with absolute pointer mapping, automatic rollback on
  every failure and disconnect, lid automation request.

### Added - M6 Robustness

- `service`: SYSTEM service with agent supervisor, secure-desktop input helper, Ctrl+Alt+Del
  (`SendSAS`); ADR 0006.
- Diagnostics export (ZIP without keys), crash reports, process CPU/RAM metrics, third-party licence
  report (`THIRD_PARTY_LICENSES.html`), `cargo deny` clean with documented exceptions.

### Added - M7 Release preparation

- English and German UI texts, README screenshots, contribution and security documents.

[Unreleased]: https://github.com/softwelop/desk-bridge/commits/main
