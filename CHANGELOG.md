# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it reaches 1.0.

## [Unreleased]

## [0.1.1] - 2026-10-20

Fixes the devices silently losing each other when Windows re-classifies a network, makes
"Restart agent" work and makes updating over a running installation reliable. Install it on both
devices.

### Added

- **Public network detection**: the agent checks the Windows network category at start, after
  network changes and every 10 s. On a network classified as "Public" (Windows Firewall blocks
  Desk Bridge there) the app shows a hint with the network name and the button "Mark network as
  private". The click switches it to Private (directly when the agent is elevated, otherwise
  through the Desk Bridge service). Before, this button only answered "not available yet".
- The partner is told as well: a device on a Public network announces that over mDNS, and the
  other device shows "The other device is on a Public network" instead of silently retrying.
- Failed connection attempts are written to the agent log (once per distinct error), so an
  unreachable partner is diagnosable from the default log.

### Fixed

- **Restart agent** (Settings) works: the agent shuts down cleanly and starts a fresh instance
  that waits until the old process has released the port, the pipe and the single-instance lock.
  When the Desk Bridge service supervises the agent it adopts the new instance (no double start).
- **Installer over a running Desk Bridge**: the installer now closes the UI first. Before, the
  UI restarted the agent while the "close Desk Bridge?" dialog was open, so the installer could
  not replace `deskbridge-agent.exe` and had to be started a second time.
- The firewall rule also opens the **Domain** profile (company LANs are classified as domain
  networks). Still limited to `deskbridge-agent.exe`, UDP 47820 + mDNS and the local subnet; the
  Public profile stays closed.

## [0.1.0] - 2026-10-16

First public release. One installer for both Windows machines; pair them once with a six-digit
code. Everything stays on your LAN: end-to-end encrypted, no cloud, no account, no telemetry.

### Highlights

- **Mouse and keyboard across both computers**: push the cursor over the screen edge and control
  the laptop like one more monitor; arrange the screens, corner lock, hotkeys, Ctrl+Alt+Del and
  the lock screen through the Desk Bridge service.
- **Laptop sound in your headset**: low-latency Opus/PCM with an adaptive jitter buffer and clock
  drift compensation. The sound of *every* laptop output is captured and mixed, so Teams, Zoom
  & co. are heard even when they play on the communications device or an output picked in their
  own settings. The laptop speakers can stay muted meanwhile and are restored afterwards.
- **Headset microphone for the laptop** through the free, Microsoft-signed
  [VB-CABLE](https://vb-audio.com/Cable/) driver (install it on the laptop yourself). While the
  headset mic is switched on, "CABLE Output" is enabled and the default recording device; when the
  mic is off, the connection drops or Desk Bridge is not running, it is disabled and apps fall back
  to the laptop microphone. Audio is only sent while an app actually uses the microphone.
- **Shared clipboard** for text, rich text, images and files, and **file transfers** with copy/paste
  in Explorer, drag and drop or "Send to"; resumable, verified with BLAKE3, clearable history.
- **Focus mode**: the laptop full screen, split or as a window on the PC monitor through a
  virtual display, hardware video encoding/decoding, absolute pointer mapping and automatic
  rollback of the display layout.
- **Update hint**: Desk Bridge checks the GitHub releases (30 s after start, then daily, or on
  demand in Settings) and links to the release page when a newer version is out. Nothing is
  downloaded or installed automatically.
- English and German UI in light and dark theme, tray, on-screen hints, diagnostics export.

### Added

- `proto`: protobuf schema `deskbridge.v1`, length framing, audio/video binary headers, ALPNs
  `deskbridge/1` and `deskbridge-pair/1`, protocol versioning with capability negotiation.
- `net`: device identity (UUIDv7 + self-signed Ed25519 certificate, DPAPI-protected), rustls
  certificate pinning, SPAKE2 pairing (2 min validity, lock-out after 3 failures), QUIC with
  control and input streams and datagrams, mDNS discovery, manual addresses.
- `core`: engine actor, versioned configuration, connection state machine with heartbeat and
  reconnect backoff, NDJSON UI protocol over a named pipe, mock platform for tests.
- `input`: shared layout in normalised units, edge routing with dwell delay, required modifier,
  corner lock and double bump, injection only between Enter and Leave, lock-key sync,
  `release_all` on every exit path; low-level hooks + Raw Input capture and `SendInput` injection.
- `audio`: Opus (static libopus) and PCM with seamless switching, adaptive jitter buffer with
  concealment, Kaiser-sinc drift resampler, silence suppression; WASAPI loopback (mixed over all
  real outputs), microphone capture and event-driven low-latency render, hot-plug handling,
  default-device switching, microphone session detection.
- Virtual microphone on VB-CABLE: device-clocked playback into "CABLE Input", automatic
  enable/disable of "CABLE Output" via the service (also while the agent is not running), default
  device claim and restore (crash-safe), microphone playout statistics from the laptop in the PC
  log and the microphone card.
- `clipboard` and `files`: sync policy (eager text, delayed rendering, loop avoidance, size and
  privacy rules), CF_HTML/RTF/PNG/DIB conversion, virtual files through a custom `IDataObject`,
  chunked transfers with resume and path hardening, transfer history with "clear history".
- `video` and `display`: DXGI capture, FFmpeg (LGPL, shared DLLs) encode/decode, D3D11 presenter,
  adaptive bitrate, focus plans (full/split/window), virtual display control, topology save and
  crash recovery.
- `service`: SYSTEM service with agent supervisor, secure-desktop input helper, Ctrl+Alt+Del
  (`SendSAS`), virtual-microphone guard.
- `setup` and NSIS installer: firewall rule (UDP 47820 + mDNS, private profile), elevated logon
  task, "Send to" shortcut, driver installation, service, setup log in ProgramData; user data
  survives upgrades.
- Partner update: hand an installer to the paired device over the connection (SHA-256 verified,
  the partner confirms first).
- Release check against `api.github.com/repos/auerth/desk_bridge/releases/latest` through WinHTTP
  (system proxy settings respected).
- Diagnostics ZIP (without keys), crash reports, CPU/RAM and latency metrics, third-party licence
  report (`THIRD_PARTY_LICENSES.html`).
- Documentation: architecture, protocol, testing checklists, development guide, ADRs 0001-0006.

### Known limitations

- Windows 10 22H2+ / Windows 11 x64 only.
- The microphone feature needs VB-CABLE installed by the user on the laptop; in Teams pick
  "CABLE Output" once. Use 48 kHz for both "CABLE Input" and "CABLE Output" (different rates on
  the two sides are a known cause of crackling).
- The installer is not code-signed yet, so Windows SmartScreen may warn on first start.
- Install the same version on both devices.
- Several hardware scenarios are not signed off yet, see [docs/testing.md](docs/testing.md).

[Unreleased]: https://github.com/auerth/desk_bridge/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/auerth/desk_bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/auerth/desk_bridge/releases/tag/v0.1.0
