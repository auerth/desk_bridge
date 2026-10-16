# ADR 0001 - Technology stack

* Status: **Accepted**
* Date: 2026-10-16
* Deciders: Thorben (softwelop)
* Source: `docs/PLAN.md` section 5 (this ADR freezes it as the baseline for M0)

## Context

Desk Bridge shares mouse, keyboard, clipboard, files, audio and (later) a streamed desktop between
two Windows computers on one LAN. The core has hard latency requirements (< 3 ms added input
latency on wired LAN, < 30 ms glass-to-glass for 5120x1440@120), must run elevated, must be
installable by non-experts with a single installer and no manual prerequisites, and should be
portable to macOS and Linux later. It is developed by one person with AI assistance, so the stack
must be mainstream and well supported. Budget for code-signing certificates is zero.

## Decision

| Area | Choice | Main reason |
|---|---|---|
| Core language | Rust stable, edition 2024 | no GC pauses, safe, `windows-rs`, portable later |
| UI | Tauri 2 + React 19 + TypeScript + Vite; Radix UI primitives, Zustand, Motion, Lucide | small binaries, full control over the softwelop branding |
| Async runtime | tokio | standard |
| Transport | QUIC (`quinn` + `rustls`), one UDP port (47820), ALPN `deskbridge/1` | TLS 1.3 built in, independent streams (no head-of-line blocking), unreliable datagrams for audio |
| Discovery | mDNS/DNS-SD (`mdns-sd`), manual IP fallback | zero configuration on a LAN |
| Pairing | SPAKE2 (`spake2`) with a 6-digit code + pinned self-signed certificates (`rcgen`) | no MITM without manual fingerprint comparison |
| Control protocol | Protobuf (`prost`), `u32 LE` length framing | schema evolution across app versions |
| Media headers | small custom little-endian binary headers | minimal per-packet overhead |
| Windows APIs | `windows` crate (windows-rs) | official bindings |
| Audio I/O / codec | WASAPI (event driven, loopback), Opus, optional raw PCM, `rubato` for drift | low latency, control over buffers |
| Video | FFmpeg **LGPL shared build shipped as DLLs** (NVENC/QSV/AMF encode, D3D11VA decode) | one API for all GPU vendors; no GPL (x264 excluded) |
| Capture / render | DXGI Desktop Duplication (fallback Windows.Graphics.Capture); native `winit` window + D3D11 flip swap chain | lowest latency; WebView is too slow for video |
| Virtual display | existing signed IddCx driver, shipped unmodified (see ADR 0004) | custom drivers need EV signing |
| Virtual audio | only a ready-made signed, redistributable driver (see ADR 0003) | no budget for attestation signing |
| Installer | Tauri bundler -> NSIS with hooks + own helper `deskbridge-setup.exe` (SetupAPI, firewall COM, Task Scheduler COM) | no third-party tools for the user; WebView2 offline installer embedded |
| Runtime deps | static CRT (`+crt-static`), WebView2 embedded, everything else bundled | "one installer, zero manual steps" |
| Fonts | Montserrat only (SIL OFL) | licence-safe, brand-conform |
| Updates | `tauri-plugin-updater` + GitHub Releases, minisign-signed `latest.json` | no own infrastructure |
| Logging / config / errors | `tracing` (+ rolling files), TOML (`serde`, `directories`), `thiserror` in libraries / `anyhow` in binaries | standard |
| CI | GitHub Actions `windows-latest`: fmt, clippy `-D warnings`, tests, `cargo deny`, `cargo audit`, Tauri build | one place for all gates |
| Licence | MIT OR Apache-2.0; `cargo deny` allow-list, no GPL in the core | open-source ready from day one |

Process model (agent / UI / service) and the repo layout are described in
[`docs/architecture.md`](../architecture.md); everything OS-specific sits behind traits in
`crates/platform` so macOS/Linux can be added without touching the logic crates.

## Consequences

Positive

* Latency-critical paths are native Rust with no managed runtime; the UI can crash without
  interrupting input or audio.
* One encrypted UDP port, no extra services; firewall setup is a single rule.
* Protocol can evolve between versions (protobuf field tags, capability negotiation).
* The installer has no external prerequisites; users never see devcon, bcdedit or test signing.

Negative / risks

* Rust + Windows driver/UI interop has a learning curve; kernel-mode work is deliberately avoided.
* FFmpeg must be an LGPL build shipped as DLLs and checked in CI (`scripts/check-ffmpeg-license.ps1`);
  accidentally linking a GPL build would taint the project.
* Without an EV certificate there is no own audio driver; F3 (virtual microphone) depends on finding
  a signed third-party driver (ADR 0003) and may be deferred.
* Unsigned binaries trigger a one-time SmartScreen prompt for internet downloads until SignPath
  Foundation signing is set up for the open-source release (M7).
* Tauri's single-instance plugin and WebView2 add a dependency on a Microsoft runtime (embedded,
  installed only if missing).

## Alternatives considered

C#/.NET (GC, Windows-centric), C++ (slower to develop, less safe), Electron (size, RAM), WinUI 3
(Windows only), egui/Slint (harder to brand), TCP + custom TLS (head-of-line blocking), WebRTC
(overhead, hard to tune), `cpal` (too abstract for loopback and latency tuning), x264 / OpenH264
(GPL / patent licence), vendor GPU SDKs one by one (3x the effort), writing our own IddCx or audio
driver now (EV certificate required).
