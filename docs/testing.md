# Testing

Two layers: **automated** tests that run everywhere (also in CI, no hardware, no admin) and
**manual hardware checklists** per milestone that Thorben runs on the real PC + laptop pair. A
milestone is done when its automated tests are green *and* its checklist is ticked (plan section 15).

Test setup used in the checklists: **PC** = control device (mouse, keyboard, wireless headset with USB
dongle, 5120x1440@120 monitor), **Laptop** = target device. Both on wired LAN unless a step says
otherwise. Build/deploy: `scripts/deploy-laptop.ps1 -Target \\LAPTOP\deskbridge -Run`.

## 1. Automated tests

Run everything: `cargo test --workspace`. CI additionally runs `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check` and `cargo audit`.

| Area | Where | What it covers |
|---|---|---|
| Protocol | `crates/proto` | framing round trip and partial reads, oversize frames, unknown-field tolerance (old decoder, new bytes), audio/video header encode/decode, version compatibility |
| Identity, pairing, transport | `crates/net` (+ `tests/pair_and_session.rs`) | cert generation and fingerprint, DPAPI-protected storage (via `SecretProtector`), SPAKE2 pairing incl. wrong code, expiry, 3-strike lockout, MITM detection, pinned session, unknown peer rejection, hello/version mismatch |
| Layout and routing | `crates/input` | edge adjacency, relative-position mapping across different resolution/DPI, corner blocking, switch delay, hotkey detection, router state machine, receiver key tracking and `release_all` |
| Engine integration | `crates/core` | two engines with mock platforms over `127.0.0.1`: pairing, reconnect/backoff, edge switch, **connection loss while a key is held** (must call `release_all`), clipboard text, config migration, UI protocol (NDJSON) |
| Audio | `crates/audio` | PCM/Opus round trip, jitter buffer with simulated clocks and loss, drift controller, level meters |
| Files / clipboard | `crates/files`, `crates/clipboard` | chunking, resume, BLAKE3, format mapping, loop guard |
| Installer helper | `crates/setup` | CLI parsing, scheduled-task XML, `installed.json`, INF parsing, driver catalogue validation, lid-backup restore commands, firewall `netsh` arguments, brand constants in sync with `core` |
| UI | `app/` | `npm run build` (type check + bundle); component tests as they are added |
| Network chaos | planned (M3+) | transport wrapper with artificial loss/jitter/latency |

Safe dry run of the installer helper on any machine (changes nothing):

```powershell
.\target\debug\deskbridge-setup.exe install --dry-run
.\target\debug\deskbridge-setup.exe uninstall --dry-run --purge-user-data
.\target\debug\deskbridge-setup.exe status          # read-only JSON
.\target\debug\deskbridge-setup.exe check-network   # read-only JSON
```

Latency tools (M2+): `scripts/measure-input-latency.ps1`, `scripts/measure-audio-latency.ps1`
(front-ends for `tools/latency-probe`). `tools/fake-peer` is a headless peer for recording and
replaying events.

## 2. Manual checklists

Tick a box only when the **expected result** was observed. Write failures into the issue tracker
with the log from `%LOCALAPPDATA%\softwelop\DeskBridge\logs\`.

### M0 - Foundation (installer, shell)

- [ ] Fresh Windows 10 22H2 VM (no VC++ redistributable, no WebView2): run `DeskBridge-Setup-x.y.z.exe`. Expected: installs without any other prompt or download; app starts with tray icon and own icon.
- [ ] Windows 11 laptop: same. Expected: same; Start menu entry present.
- [ ] Look at the UI in light and dark theme (switch Windows theme). Expected: branded, readable, Montserrat everywhere, Mica background, custom title bar.
- [ ] Tray menu opens; second start of the app. Expected: no second instance, the existing window is focused.
- [ ] `deskbridge-setup.exe status` (elevated). Expected: firewall rule, scheduled task `DeskBridge Agent` and SendTo shortcut `present: true`.
- [ ] Reboot or log off/on. Expected: agent starts at logon without a UAC prompt (Task Manager shows `deskbridge-agent.exe` as elevated).
- [ ] Install the same version over itself, then an older over newer is refused/handled as designed. Expected: user data (pairing, settings) untouched.
- [ ] Uninstall, answer "No" to deleting user data. Expected: firewall rule, task, SendTo entry and install folder gone; `%APPDATA%\softwelop\DeskBridge` still there.
- [ ] Uninstall again after reinstall, answer "Yes". Expected: user data folders gone as well.
- [ ] CI is green and the installer artefact is attached to the run.

### M1 - Connection

- [ ] Pair PC and laptop with the 6-digit code, timing yourself without reading docs. Expected: **< 30 s**.
- [ ] Enter a wrong code three times. Expected: pairing is refused, then locked for 60 s.
- [ ] Let a code expire (> 2 min). Expected: "Code abgelaufen".
- [ ] Start Desk Bridge on a third, unpaired PC and try to connect to the laptop by IP. Expected: connection rejected, nothing visible happens on the laptop.
- [ ] Network profile on the laptop is "Public". Expected: UI hint with "Netzwerk als privat markieren"; clicking it asks for confirmation, then connection works.
- [ ] Put the laptop to sleep and wake it. Expected: reconnect **< 5 s** after the network is back.
- [ ] Unplug and replug the LAN cable (both sides in turn). Expected: state goes Degraded/Reconnecting and recovers by itself.
- [ ] Router/switch restart. Expected: automatic recovery.
- [ ] Restart only one side. Expected: automatic recovery without re-pairing.
- [ ] Unpair on one side. Expected: the other side shows the device as removed and refuses to reconnect.
- [ ] Leave connected for 24 h. Expected: no drop (or only automatic recoveries), RAM stable, agent CPU < 1 %.

### M2 - Mouse, keyboard, text clipboard

Switching and layout

- [ ] Arrange screens in the editor, move the cursor across each edge (left/right/top/bottom), with different offsets. Expected: cursor appears at the matching position on the other device.
- [ ] Corners with "corner blocking" on/off. Expected: no accidental switch in blocked corners; start menu / close buttons usable.
- [ ] Switch delay 0 / 250 / 500 ms and "only with Ctrl". Expected: behaves as configured.
- [ ] Hotkeys: jump (`Ctrl+Alt+Shift+Space`), cursor lock (`Scroll Lock`), emergency (`Ctrl+Alt+Shift+Backspace`). Expected: each works from both states; emergency always returns everything to the PC.
- [ ] OSD pill on switch. Expected: appears ~600 ms with the target name.

Typing and input quality

- [ ] German characters on the laptop while remote: `@ € { } [ ] \ | ~ ä ö ü ß Ä Ö Ü`, AltGr combinations. Expected: all correct.
- [ ] Dead keys: `^ ´ \`` followed by a vowel. Expected: correct composed characters.
- [ ] Win key shortcuts (`Win+E`, `Win+D`, `Alt+Tab`, `Ctrl+Esc`, `Win+L` on the target while remote). Expected: they act on the laptop, not the PC.
- [ ] Media keys, mouse side buttons (back/forward), precision scrolling, horizontal scroll. Expected: work on the laptop.
- [ ] Caps Lock on at the PC, then switch. Expected: toggle state synchronised on the laptop.
- [ ] Elevated window on the laptop (Task Manager, installer). Expected: input works (agent runs elevated).
- [ ] Full-screen app/game window on the laptop (windowed borderless). Expected: input works; note anti-cheat games are out of scope.

Robustness (acceptance test)

- [ ] Hold Shift on the PC while remote, then pull the LAN cable; repeat **10 times**. Expected: **no stuck keys** on either machine, cursor returns to PC.
- [ ] Kill `deskbridge-agent.exe` on the PC while remote and holding a key. Expected: laptop releases the key; PC input works normally again.
- [ ] Kill the agent on the laptop while remote. Expected: PC returns to local control within a few seconds.
- [ ] Lock the PC (`Win+L`) while remote. Expected: defined behaviour (back to local), nothing stuck.

Clipboard (text)

- [ ] Copy text on PC, paste on laptop and the reverse; plain text, Unicode/emoji, multi-line. Expected: identical content both directions.
- [ ] Copy rapidly 20 times in a row. Expected: no loops or flicker (loop guard).

Measurements

- [ ] `scripts/measure-input-latency.ps1 -Peer <laptop> -Count 5000`. Expected: added latency **p95 < 3 ms** (wired LAN); also record Wi-Fi 5 GHz (target < 15 ms).
- [ ] Acceptance: use the setup for **one working week without the KVM** for mouse and keyboard. Expected: no blocking problems.

### M3 - Audio and microphone

M3a: laptop sound to headset

- [ ] Play music on the laptop. Expected: audible in the PC headset (pick the right dongle device "Game"/"Chat"); mixed with PC audio.
- [ ] Expected: laptop speakers stay silent (or the behaviour is documented in the ADR with the workaround).
- [ ] Laptop volume 0 and muted. Expected: documented behaviour (loopback may go silent).
- [ ] 1 hour of music. Expected: no crackle, no growing delay (drift control).
- [ ] Switch codec Auto / PCM / Opus. Expected: seamless, no click.
- [ ] Turn the headset off and on, unplug/replug the dongle during playback. Expected: stream pauses and **resumes automatically**; no crash; no silent switch to another device.
- [ ] `scripts/measure-audio-latency.ps1 -Peer <laptop>` (click test). Expected: **< 30 ms** wired, **< 60 ms** Wi-Fi (without the headset's own latency).
- [ ] CPU while streaming. Expected: **< 3 %**.
- [ ] Per-direction mute and level meters in the UI. Expected: respond correctly.

M3b: virtual driver and microphone (only if ADR 0003 selects a driver; otherwise the UI shows "noch nicht verfügbar")

- [ ] Fresh laptop with **Secure Boot on**, no test mode: run the installer. Expected: driver installs **without any Windows prompt**, devices "Desk Bridge ..." appear.
- [ ] Teams test call on the laptop using the PC headset microphone. Expected: other party confirms good quality.
- [ ] Mic end-to-end latency. Expected: **< 30 ms** wired.
- [ ] Open the mic in an app on the laptop, then close it. Expected: PC tray shows "Laptop nutzt dein Mikro (App)" only while open; **audio is transmitted only while in use**.
- [ ] Global mute hotkey (`Ctrl+Alt+Shift+M`). Expected: mic muted, OSD shown.
- [ ] 8 hours of music. Expected: no crackle or drift.
- [ ] "Als Standardgerät setzen" (playback + communications). Expected: only with explicit consent; reverting works.
- [ ] Uninstall. Expected: driver package and devices removed (check Device Manager and `pnputil /enum-drivers`).

### M4 - Clipboard complete and files

- [ ] Copy rich text (Word/Browser), HTML, RTF to the other side. Expected: formatting kept.
- [ ] `Win+Shift+S` screenshot on the laptop, paste into Paint/Teams on the PC. Expected: image arrives.
- [ ] Copy a password from the password manager. Expected: **not** synchronised.
- [ ] Copy a 2 GB file in Explorer on the PC, paste in Explorer on the laptop. Expected: Explorer progress dialog, throughput **>= 80 %** of link speed, hash matches (compare BLAKE3/SHA-256).
- [ ] Copy a folder with 1 000 small files. Expected: complete, timestamps preserved.
- [ ] Pull the cable mid-transfer, reconnect, repeat the paste. Expected: transfer resumes.
- [ ] "Senden an -> Desk Bridge - Laptop senden" in Explorer. Expected: files arrive in `Downloads\Desk Bridge`.
- [ ] Drop zone in the UI; history lists the last transfers with "Im Ordner zeigen".
- [ ] Per-direction clipboard switches, size limit. Expected: respected.

### M5 - Focus mode

- [ ] Spike first: capture -> NVENC -> network -> D3D11VA at 5120x1440@120; record encode/decode time per frame and the Optimus adapter question. Expected: numbers in the ADR before building further.
- [ ] Hotkey `Ctrl+Alt+Shift+F`: laptop appears full screen on the Ultrawide at **5120x1440@120**. Expected: stable 120 fps while moving windows, text in browser and IDE legible.
- [ ] Glass-to-glass latency with the timestamp overlay. Expected: **p95 < 30 ms** wired.
- [ ] Toggle on/off **20 times**. Expected: each toggle **< 2 s**; afterwards the laptop's display topology is exactly as before (all 20).
- [ ] Kill `deskbridge-agent.exe` on the laptop during focus mode, start it again. Expected: topology restored automatically.
- [ ] Close the laptop lid in focus mode (with "do nothing on lid close" accepted). Expected: stream continues; open lid restores.
- [ ] Split mode (`Ctrl+Alt+Shift+D`): maximise windows on the PC. Expected: they stay in their half (AppBar); mouse crosses the monitor centre seamlessly in both directions.
- [ ] 4:4:4 option and AV1 option. Expected: work or are greyed out with an explanation.
- [ ] Watch a 1 h video in the streamed desktop. Expected: no stutter or freeze.
- [ ] Machine without hardware encoder (VM). Expected: focus mode disabled with a clear hint.
- [ ] Uninstall with the virtual display installed. Expected: device and driver removed, no ghost monitor.

### M6 - Robustness

- [ ] UAC prompt on the laptop. Expected: operable from the PC keyboard/mouse.
- [ ] Lock screen and sign-in screen of the laptop. Expected: operable from the PC; `Ctrl+Alt+End` sends Ctrl+Alt+Del.
- [ ] Kill the agent. Expected: service restarts it and **first releases all keys**.
- [ ] Sleep/wake of each device repeatedly, Wi-Fi <-> cable switch, router restart. Expected: automatic recovery each time.
- [ ] Different app versions on the two devices. Expected: UI shows the version mismatch; incompatible major shows "Bitte beide Geräte aktualisieren".
- [ ] Update from x to x+1 via the updater on both devices. Expected: no re-pairing.
- [ ] Diagnostics export. Expected: ZIP with logs, config (no keys), system info.
- [ ] Idle CPU **< 1 %**, agent + UI RAM **< 150 MB**.
- [ ] 8 h soak (music, typing, idle). Expected: no hang, drift or stuck key.
- [ ] Two weeks of daily use without restarting the app.

### Installer regression (before every release)

- [ ] `deskbridge-setup.exe install --dry-run` output matches expectations; real install is idempotent (run `install` twice).
- [ ] `check-network` reports the right category; `make-private` only after confirming in the UI.
- [ ] Uninstall restores the lid-close action (if the agent changed it) - verify with `powercfg /query SCHEME_CURRENT SUB_BUTTONS LIDACTION`.
- [ ] FFmpeg DLLs: `scripts/check-ffmpeg-license.ps1` passes (no `--enable-gpl`, no `--enable-nonfree`).
