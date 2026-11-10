# ADR 0009 - PC webcam as a virtual camera on the laptop

* Status: **Proposed** (implemented; needs the hardware acceptance test below)
* Date: 2026-11-10
* Deciders: Thorben (owner)
* Related: ADR 0003 (virtual microphone: the same idea for audio, blocked there by driver
  signing), ADR 0005 (video pipeline); `crates/camera`, `crates/vcam`,
  `crates/platform-windows/src/camera`, `crates/core/src/engine/camera.rs`

## Zusammenfassung für Thorben (Deutsch)

Die Webcam am PC erscheint auf dem Laptop als Kamera **„Desk Bridge Kamera“**. Teams, Zoom, der
Browser und die Kamera-App sehen sie wie jede andere Webcam. Anders als beim Mikrofon braucht das
**keinen Treiber und keine Signatur**: Windows 11 kann seit Build 22000 virtuelle Kameras
(`MFCreateVirtualCamera`). Die Kamera ist eine normale DLL (`deskbridge_vcam.dll`), die der
Windows-Dienst „Kamera-FrameServer“ lädt.

Wie beim Mikro gilt: **Die Webcam am PC geht nur an, solange eine App auf dem Laptop die Kamera
zeigt.** Der PC zeigt dann „Laptop nutzt deine Kamera (Teams)“. Ohne Verbindung, im Pausen-Modus
oder ohne Webcam am PC zeigt die Kamera ein Platzhalterbild (Kamera-Symbol auf Markenblau, Punkt
orange = PC-Kamera startet, grau = nicht verfügbar).

Einschränkung: nur **Windows 11** auf dem Laptop. Die Registrierung braucht einmal
Administratorrechte; das erledigt der Agent selbst, wenn er (wie üblich) als Administrator läuft.

## Context

Owner request (2026-11-10): "webcam auch übertragen an laptop … wie mikro". The user sits at the
PC (webcam, headset); calls run on the laptop. The microphone path (ADR 0003) needs a signed
kernel driver; a camera does not.

## Decision

1. **Virtual camera = Media Foundation media source** in `crates/vcam` (Rust `cdylib`, COM class
   `{5B3F8C21-7A4E-4D19-B6C2-9E1F04A7D385}` under `HKLM\SOFTWARE\Classes\CLSID`, registered with
   `MFCreateVirtualCamera(SoftwareCameraSource, System, AllUsers)`). The Frame Server (svchost,
   `LOCAL SERVICE`, session 0) loads it: class factory → `IMFActivate` → `IMFMediaSourceEx` +
   `IKsControl` (answers "not found") + one `IMFMediaStream2`. It offers NV12 1280x720 (default),
   1920x1080 and 640x360 at 30 fps, BT.709 limited range, system-memory 2D buffers; a worker
   thread answers each `RequestSample` paced to the chosen frame rate.
   `MFCreateVirtualCamera` is resolved at run time (Windows 10 lacks the export).
2. **Agent ↔ camera: one named file mapping** `Global\DeskBridgeCamera.v1` (`crates/camera`):
   a 4 KiB header (heartbeats, latest published frame, the size the app chose, a log ring) and
   three NV12 slots of 1920x1080. The agent writes, the camera reads; a slot's sequence number
   detects torn reads. DACL: SYSTEM, administrators, `LOCAL SERVICE`, interactive users. Either
   side creates it (`SeCreateGlobalPrivilege`: elevated agent or the Frame Server). The camera
   has no log file of its own; its lines go into the log ring and the agent copies them into its
   log (target `vcam`).
3. **"In use" = the camera's heartbeat.** While an app streams, the camera beats
   `reader_alive_ms` with every sample and states size and rate; stale after 1.5 s. The agent
   turns that into `CameraInUse { width, height, fps, process }` (the app name comes from the
   Windows privacy bookkeeping `CapabilityAccessManager\ConsentStore\webcam`).
4. **Transport:** caps `camera.source` (headset device) / `camera.sink` (stream source with the
   virtual camera); control `CameraRoute`, `CameraInUse`, `CameraStatus`, `CameraKeyframe`;
   pictures on **one** uni stream (kind 4) per sender run, `write_frame` framing, H.264. One
   stream keeps the pictures in order; when the network or the decoder falls behind, pictures are
   dropped until the next key frame (sender) and a key frame is requested (receiver).
5. **Codecs:** PC: Media Foundation source reader (shared through the Frame Server, so PC apps
   can use the webcam at the same time) → BGRA → FFmpeg hardware encoder in system memory
   (`create_camera_encoder`, ~2.5 Mbit/s at 720p30). Laptop: FFmpeg **software** H.264 decoder
   (`create_camera_decoder`; keeps the GPU for focus mode) → BGRA → NV12 at the app's size.
6. **Device switching like the microphone:** the laptop enables the camera while the partner
   forwards a webcam and disables it 30 s after the partner went away, so apps fall back to the
   laptop's own camera but a short reconnect does not end the picture in a call.
7. **Registration:** an elevated agent on the stream source registers (or repairs) it at start
   (`ensure_virtual_camera`); `deskbridge-agent --register-camera` / `--unregister-camera` do it
   by hand; the uninstaller removes it. The installer ships `deskbridge_vcam.dll` next to the
   agent.

## Consequences

* No driver, no signature, no reboot. Windows 10 laptops show the feature as unavailable.
* The camera always exists once registered (enabled/disabled by the agent); while the agent is
  not running it shows the placeholder.
* Any interactive user process could write pictures into the mapping. That is no worse than any
  other user-mode virtual camera; the DACL keeps out other services and remote users.
* The picture is converted three times (camera format → BGRA → H.264 → BGRA → NV12). Cheap at
  webcam sizes (a few ms per picture), and it reuses the existing encoder/decoder.

## Acceptance test (hardware, owner)

1. Laptop (Windows 11): install, start Desk Bridge as administrator once; Windows camera settings
   list "Desk Bridge Kamera".
2. Without a connection the Camera app shows the placeholder (grey dot).
3. Connected: open the Camera app on the laptop → the PC's webcam light goes on within ~2 s, the
   picture appears; the PC shows "Laptop nutzt deine Kamera (…)". Close the app → the webcam
   light goes off within ~2 s.
4. Teams call on the laptop with "Desk Bridge Kamera": picture fluent, the other side confirms.
5. Pause the camera on the PC → placeholder; unpause → picture again.
6. Disconnect for > 30 s → the camera disappears from Teams' list; reconnect → it comes back.

Done on the PC (2026-11-10): `camera_probe vcam 5` (synthetic colours through the real Frame
Server, 30.2 fps, colours correct), `camera_probe loop 10 <png>` (real webcam → virtual camera →
Frame Server → reader, 30.1 fps, picture correct), `hw_roundtrip` webcam test (h264_nvenc →
software decoder, colours within ±4). Open: the steps above on two devices.

Pre-test on one machine (no network): `camera_probe register <dll>` once as administrator, then
`camera_probe vcam 5` (synthetic colours through the real Frame Server) and `camera_probe forward
60` (own webcam into "Desk Bridge Kamera", look at it in the Camera app).
