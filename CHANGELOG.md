# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it reaches 1.0.

## [Unreleased]

## [0.1.4] - 2026-10-21

Install it on both devices; the fix itself works on the laptop. After installing it on the PC,
"<Partner> aktualisieren" updates the laptop over the connection.

### Fixed

- **Teams calls were silent in the headset**: while the laptop's sound is forwarded, Desk Bridge
  kept the laptop speakers quiet by muting them. On the laptop's Realtek output the mute also
  silences what Desk Bridge records, so a call arrived silent at the PC (turning the volume down
  instead of muting kept it working). The speakers are now kept quiet by turning their volume to 0
  with the mute switched off; volume and mute are put back exactly as they were when forwarding
  stops (also after a crash). If the laptop was muted before, it is unmuted at volume 0 while
  forwarding, so its sound reaches the headset.

## [0.1.3] - 2026-10-21

Install it on both devices.

### Added

- **Update the other device from this one**: when the two devices run different versions, the
  newer one shows "<Partner> aktualisieren" (version banner and Settings -> Updates). Its installer
  is transferred over the connection and installed on the partner right away, without anyone at
  the other device; the connection drops briefly and comes back with the new version, and the
  result is shown on both devices. From the older device, "Auf <version> aktualisieren" fetches
  the partner's version the same way. Only newer versions are installed, never older ones.
- New setting "Partnergerät darf Updates hier installieren" (on by default); off, the partner's
  update is shown as a question as before.
- The installer keeps a copy of itself next to the agent (`installer\DeskBridge-Setup-<version>.exe`,
  about 250 MB) so the device can pass its version on; received update packages that are no longer
  needed are deleted at start. A partner that still runs 0.1.2 asks its user once.

### Fixed

Focus mode froze after a while (the picture stood still, sometimes the PC window stayed open after
ending it, and once the laptop lost the connection). There were several independent causes:

- **The PC ran out of graphics memory**: every bitrate change of the laptop made the PC's video
  decoder re-create its picture pool, and the colour converter kept each old pool alive (about
  120 MB at 2560x1440). After a few minutes the graphics card was full: the picture froze, and the
  next focus start blocked the whole agent, so the laptop lost the connection. The converter now
  only keeps what belongs to the current pool; measured on an RTX 3070 Ti, graphics memory stays
  flat across bitrate changes. The laptop's encoder had the same problem whenever its screen
  capture was re-created.
- **The laptop stopped capturing**: Windows interrupts the screen capture when the displays change
  (`DXGI_ERROR_ACCESS_LOST`) and renumbers them. The capture re-opened the old display number,
  which no longer existed, and never got a picture again ("Die Bildschirmaufnahme am Laptop liefert
  keine Bilder", or a frozen picture during use). It now finds the display again by its name (or
  its size), keeps its graphics device and re-opens within a fraction of a second; if that keeps
  failing it tries a new device now and then.
- **A capture that stops without any error is revived**: the laptop kept sending keep-alives, so
  the PC never noticed a silent capture. The laptop now notices when keys, clicks or the wheel
  produce no new picture for 3 s and rebuilds the capture with a new graphics device; if that does
  not help either, focus mode restarts itself on both sides (at most three times a minute). A still
  desktop without input is left alone.
- **A stuck viewer on the PC is replaced**: if the PC's decode/display thread hangs (in the decoder
  or the graphics driver), the PC notices within 3 s, closes the window, takes a new graphics
  device and restarts focus mode; the log names the step it hung in. A hardware decoder that
  rejects every frame is rebuilt after three failed frames (and asks for a keyframe); if the rebuilt
  one fails too, focus mode restarts with a fresh device. After three restarts within a minute it
  stops with a message and the laptop is restored.
- **Keyframes overtaken by later frames**: every video frame travels on its own QUIC stream and the
  PC read them in parallel, so a large keyframe was regularly overtaken by the small frames behind
  it and dropped as "late". The PC now puts frames back into order (waiting at most 80 ms for an
  overtaken one), and the laptop sends older frames with higher priority.
- **Split-mode window staying open on the PC** after focus mode ended: it is now closed directly,
  also when the viewer thread hangs.
- Opening the focus window and video decoder no longer happens on the agent's main loop: a stalled
  graphics driver can only make that focus start fail (with a message); the connection stays up.
- If the capture does have to create a new graphics device, the video encoder re-opens itself on
  it instead of rejecting every frame.
- A focus start that arrives while the laptop is still restoring the previous layout is queued and
  started right after, instead of failing.
- When the capture is interrupted for longer (lock screen, UAC prompt on the laptop), the picture
  shows why instead of just standing still.
- Partner logs (Settings -> Diagnostics) arrived empty for the log file that was still being
  written: its size was read from the stale directory entry.

### Diagnostics

- The PC log shows the laptop's capture and encode rate, how old its last picture is and any
  capture problem; the viewer logs every 5 s how many frames it dropped and why; decode errors and
  capture re-open failures are logged with their text.

## [0.1.2] - 2026-10-20

Fixes the laptop staying black after focus mode, lets you read the partner's logs from your own
device and turns an update into a single run without questions. Install it on both devices (the
laptop fix only takes effect on the laptop).

### Added

- **Logs of the other device**: Settings → Diagnostics → "Fetch logs" pulls the newest log files of
  the paired device over the connection (agent, UI, setup and service logs plus a system-info
  block; no keys, no config secrets) and shows them in a viewer with file selection, filter,
  highlighted warnings/errors, copy and "open folder". They are also saved locally under
  `%LOCALAPPDATA%\softwelop\DeskBridge\peer-logs\`. Needs 0.1.2 on both devices.

### Fixed

- **Laptop black after focus mode**: when the display layout could not be restored (typically
  because the laptop was locked meanwhile: Windows refuses layout changes on the lock screen), the
  laptop stayed on the invisible virtual display with its own panel switched off. Now
  - the real displays are switched back on in that case, if necessary by the Desk Bridge service
    on the lock screen, and only then the virtual display is removed,
  - the exact layout is restored again every 3 s until it works (e.g. right after unlocking),
  - a new focus mode waits until the old layout is back instead of saving the broken one.
- **Updating**: running the installer over an existing installation no longer shows welcome,
  licence, "uninstall before installing?", folder and start-menu pages, and no longer uninstalls
  the old version first (that removed service, task, firewall rule and display driver just to set
  them up again and asked whether to delete the user data). It shows only the progress bar and
  starts Desk Bridge again. A first installation keeps the normal assistant.

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

[Unreleased]: https://github.com/auerth/desk_bridge/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/auerth/desk_bridge/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/auerth/desk_bridge/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/auerth/desk_bridge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/auerth/desk_bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/auerth/desk_bridge/releases/tag/v0.1.0
