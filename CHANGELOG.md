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

[Unreleased]: https://github.com/softwelop/desk-bridge/commits/main
