# ADR 0007: Showing the secure desktop in focus mode (UAC, lock, logon)

Status: implemented (hardware sign-off pending)
Date: 2026-10-22
Supersedes/extends: [ADR 0006](0006-service-and-secure-desktop.md)

## Context

Focus mode streams the source machine's screen to the operator over the existing video
pipeline. When Windows raises the **secure desktop** on the source — the UAC consent dialog,
the lock screen, or the logon screen — the operator sees the "interruption" placeholder and
cannot see the prompt, even though they can already click it.

This is the standard behaviour every remote-support tool has to solve (TeamViewer, AnyDesk,
RustDesk, Chrome Remote Desktop, Windows Quick Assist). It is a legitimate, consent-based
remote-administration feature: the service is installed by the machine owner, runs signed, and
only mirrors a desktop the owner is already operating.

Two halves make it work. **Input is already done.** The SYSTEM helper
(`deskbridge-service.exe --helper`, `crates/service/src/helper.rs`) starts on
`winsta0\Winlogon`, follows the current input desktop with `SetThreadDesktop`
(`crates/service/src/desktop.rs`, `ThreadDesktop::follow`), and injects with `SendInput`. The
agent routes injection to the helper pipe whenever the secure desktop is active
(`crates/agent/src/secure.rs`, `SecureDesktopInjector`). That path is accepted and shipping.

**Capture is the missing half.** The agent's DXGI Desktop Duplication
(`crates/platform-windows/src/video/capture.rs`) runs in the user's session and gets
`E_ACCESSDENIED` from `DuplicateOutput` on the secure desktop; today that is turned into the
German "lock screen or UAC" placeholder in `interruption_reason`. Only a process running as
SYSTEM in the console session, with its thread attached to the input desktop, can open a
duplication of the secure desktop. That process already exists: it is the helper.

## Problem statement

Capture of the secure desktop must happen in the helper (SYSTEM), but the video encoder and the
network stream live in the agent (user session). The image therefore has to cross the
process/session boundary, and it has to do so without a per-frame CPU copy at 2560x1440, or the
focus-mode latency budget blows up.

## Decision

Mirror the input design for capture. Keep the duplication in the helper; hand the agent a
**GPU-shared texture**, not pixels over a pipe. The pipe only carries small control and
metadata messages. The agent's existing encoder reads the shared texture with its own D3D11
device, exactly as it reads a locally captured frame today.

### Component responsibilities

- **Helper (SYSTEM, `crates/service/src/helper.rs`)** — on request, attach the capture thread
  to the current input desktop (it already does this for input), open an
  `IDXGIOutputDuplication` for the target output, and maintain a small ring of D3D11 textures
  created with `D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX` + `..._NTHANDLE`. Each acquired desktop
  frame is copied into the next ring texture under its keyed mutex. The helper never encodes and
  never touches the network.
- **Agent (`crates/agent/src/secure.rs` + capture backend)** — when the user-session
  duplication returns `E_ACCESSDENIED`, stop treating it as a dead end: ask the helper to start
  secure capture, receive the shared-handle metadata, open the texture by handle on the agent's
  existing D3D11 device (matched by adapter **LUID**), and feed it into the same
  `CaptureOutput::Frame` path the normal capturer uses. Fall back to the placeholder only if the
  helper cannot capture.
- **Transport** — reuse the existing length-prefixed helper protocol
  (`crates/service/src/protocol.rs`). Add request/response variants for: begin secure capture
  (returns output rect + adapter LUID), poll for the next shared handle + frame metadata
  (generation counter, present time, dirty flag), the pointer position/shape on the secure
  desktop (the pipeline already keeps the cursor separate from the image), and end capture. This
  is the `SecureFrameRequest` / `SecurePointer` shape that was sketched in
  `crates/platform/src/video.rs` (see the abandoned stash) — the data model there is the right
  direction; the transport should be these typed pipe messages, not a second ad-hoc protocol.

### Why GPU-shared, not pixels over the pipe

`MAX_PAYLOAD` on the helper pipe is 256 bytes and the protocol is tuned for fire-and-forget
input. A 1440p BGRA frame is ~14 MB; copying it to system memory, framing it, and re-uploading
it in the agent would add a full read-back + upload per frame and defeat focus mode's latency.
A shared keyed-mutex texture is opened once per generation and read in place. The agent already
owns a D3D11 device and a texture pool (`capture.rs`, `Pool`), so the consuming side is a small
addition, not a new pipeline.

### Lifecycle and edge cases

- **Desktop switches.** The helper's `ThreadDesktop::follow` already reports when the input
  desktop changes. On a change, re-create the duplication (new secure desktop = new output
  association) and bump the generation counter so the agent re-opens the handle.
- **Access lost.** `DXGI_ERROR_ACCESS_LOST` fires on every secure-desktop transition; the
  existing re-open loop in `capture.rs` is the model. The helper owns the retry; the agent sees
  a generation bump.
- **Still image.** The secure desktop produces no "new frame" events while nothing moves, like
  the existing `reset` case. Carry the same `reset`/wait-timeout handling the normal capturer
  uses so the first paint and post-input repaints are not missed.
- **Helper absent / older install.** Installs from before this feature have no capture path;
  the agent must detect that from the helper `Pong` protocol version and keep showing today's
  placeholder. Bump `PROTOCOL_VERSION`.
- **Teardown.** When the input desktop returns to the user's desktop, the agent resumes its own
  session-local duplication and tells the helper to drop the shared textures.

### Security notes

- The shared textures carry an image of the secure desktop. Keep them inside the local
  machine: created by the helper, opened only by the agent on the same box over the named pipe
  whose ACL already restricts it (ADR 0006). Nothing new leaves the LAN that focus mode did not
  already send; the secure-desktop frames ride the same end-to-end-encrypted QUIC stream as every
  other focus-mode frame.
- No change to who can trigger capture: the operator is already paired and already driving the
  machine. This only lets them *see* the prompt they can already click.

## Consequences

- Closes the last hardware-sign-off gap in M6 ("UAC / lock screen") for the picture; input was
  already there.
- Adds a capture responsibility to the helper, which until now only injected. The helper still
  never encodes or networks, so its attack surface stays a local named pipe.
- The agent capture backend gains a second frame source behind the same `CaptureOutput` enum, so
  the encoder, ABR, and viewer are untouched.

## Implementation

Built as designed, with these concrete choices:

- **Handles by name.** A raw NT handle value is process-local and cannot cross the pipe. The helper
  creates every ring texture with `SHARED_NTHANDLE | SHARED_KEYEDMUTEX`, publishes it with
  `IDXGIResource1::CreateSharedHandle(name)` and only the name travels
  (`Response::SecureFrame.name`). The agent opens it with `ID3D11Device1::OpenSharedResourceByName`
  on a device on the helper's adapter (LUID from `SecureStarted`). Names are
  `DeskBridgeSecure-<helper pid>-<epoch>-<slot>`, checked to `[A-Za-z0-9_-]` on decode; the DACL of
  the objects is SYSTEM + the interactive user (the pipe's SID).
- **Keyed mutex.** The helper acquires key 0, copies, releases with key 1; the agent acquires key 1,
  copies into a pool texture, releases with key 0. A picture nobody read (key still 1) is taken
  back by the helper before the slot is reused, so a slow agent never blocks it. `AcquireSync`
  reports a timeout as a *success* HRESULT, so the code calls it through the vtable.
- **Protocol 2** (`crates/service/src/protocol.rs`, `PROTOCOL_VERSION = 2`): `SecureStart {x,y,w,h}`
  (the rectangle of the output the agent was capturing, so the helper picks the same screen),
  `SecurePoll {generation, wait_ms}`, `SecureStop`; answers `SecureStarted {luid,x,y,w,h}`,
  `SecureFrame {generation,name,w,h,pointer_x,pointer_y,pointer_visible,image}` and
  `SecureNoFrame`. `generation` counts every picture and pointer change; `image` tells whether the
  picture itself changed since the polled generation. The agent pings first and keeps the
  placeholder if the helper speaks a protocol older than 2.
- **Helper threads** (`crates/service/src/secure_capture.rs`): a controller starts one *session
  thread* per input-desktop episode. The session attaches to the input desktop, duplicates the
  output and pumps; when the input desktop changes or access is lost it ends and a fresh thread
  (new epoch, new textures) takes over, because `SetThreadDesktop` fails once a thread owns DXGI
  objects. Capture connections are separate from the input connection (a poll waits), run on
  blocking threads, and a dropped capture connection or ten seconds without a poll stops the
  capture without releasing the input connection's keys (`Handler::on_capture_disconnect`).
- **Agent** (`crates/platform-windows/src/video/capture.rs`, `run_secure`): the re-open loop
  starts the helper capture on `E_ACCESSDENIED`, copies each new picture into a pool texture and
  sends it as a normal `CaptureOutput::Frame`; the pointer goes out as `CaptureOutput::Cursor`
  (position and visibility, no shape yet, so the viewer draws its own cursor). Every 400 ms it
  tries the normal duplication; once that works the helper is stopped and the local capture resumes.
  The helper link (`HelperFrameSource`, `crates/agent/src/secure.rs`) is injected through
  `WindowsVideoBackend::with_secure_source`, because `platform-windows` cannot depend on the
  service crate.
- **Not done:** the pointer *shape* of the secure desktop, HDR outputs (the ring is BGRA8; an HDR
  output falls back to the placeholder).

## Status of the earlier attempt

A first cut lived in `git stash` ("unfinished: secure-desktop picture protocol"). Its
`SecureFrameRequest`/`SecurePointer` data model matches this ADR and is worth lifting; its
transport grew a parallel protocol in `protocol.rs` and was left unfinished. Implement against
this ADR rather than un-stashing wholesale.
