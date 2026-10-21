# ADR 0002: Laptop sound via WASAPI loopback (M3a) – mute behaviour

- Status: Accepted (needs hardware confirmation on the laptop)
- Date: 2026-10-16

## Zusammenfassung (Deutsch)

Der Laptop-Ton wird in M3a per WASAPI-Loopback auf dem Standard-Ausgabegerät abgegriffen und
zum Headset am PC geschickt. Loopback greift den Mix der Audio-Engine **vor** Lautstärke und
Stumm-Schaltung des Endpunkts ab – aber nicht bei jedem Treiber: Am Laptop (Realtek) macht
**Stumm** auch den Loopback stumm (Teams-Anruf kam am PC nicht an), die **Lautstärke** nicht
(1 % ging). Desk Bridge hält die Laptop-Lautsprecher deshalb über **Lautstärke 0 ohne Stumm**
leise und stellt Lautstärke und Stumm danach wieder her (Nachtrag 2026-10-21).

## Context

Plan 8.3/M3a: *"Zu prüfen: ob Stummschalten oder Volume 0 auch den Loopback stumm macht (je nach
Treiber). Falls ja, ist als Workaround ein Dummy-Ausgabegerät nötig."* The virtual speaker
(M3b) would remove the question, but no redistributable, Microsoft-signed driver with a data
path exists (see ADR 0003).

## Findings

* WASAPI loopback (`AUDCLNT_STREAMFLAGS_LOOPBACK`) captures the *engine mix* of a render
  endpoint before the endpoint's hardware/software volume and mute are applied. This is why
  recorders such as OBS capture sound while the speakers are muted. (Observed on the
  development PC: loopback delivers a normal signal for the unmuted default device; the mute
  case was deliberately *not* tested there, because system settings must not be changed.)
* Drivers that implement master volume as an **APO in the render path** (some OEM audio stacks)
  can attenuate *before* the loopback tap. On those machines volume 0 also silences the capture.
* Loopback produces no packets while nothing is rendering. `WasapiCapture` therefore emits
  synthetic zero frames at the expected 10 ms cadence after 20 ms of silence, so the sender's
  silence detection and keep-alives keep working.
* A loopback stream on "the default device" does not follow default-device changes; the engine
  restarts capture when `DefaultChanged` arrives (headset side) / on the next route request.

## Decision

1. Use loopback on the **default render device** of the stream source (laptop), **mixed with the
   loopback of every other active real output** (amended 2026-10-16: Teams & co. play on the
   default *communications* device or on an output picked in their own settings, so a call was
   silent in the headset). Virtual cables are excluded ("CABLE Input" carries the headset mic and
   would echo). The mix runs on a QPC clock with a per-output FIFO (15 ms prebuffer, backlog
   trimmed above 50 ms), so an idle default device does not stall the others. "Mute the laptop"
   mutes all forwarded outputs; the capture is reopened when outputs appear, vanish or the
   default changes.
2. The UI offers "Laptop-Lautsprecher stumm lassen" (Desk Bridge keeps the laptop quiet and keeps
   streaming). **Amended 2026-10-21 (hardware result):** on the laptop's Realtek output the
   endpoint *mute* silences the loopback (a Teams call arrived silent in the headset), the master
   *volume* does not (volume 1 % worked). The PC's Steam Streaming Speakers keep the loopback in
   both cases, so this is driver behaviour. Desk Bridge therefore never mutes: it sets the
   forwarded outputs to volume 0 with the mute off and restores volume and mute afterwards (crash
   backup `speaker-mute.json` carries both). If a driver attenuates before the tap (test above fails), the documented
   workaround is to select a *different* render device as default whose volume is left at 100 %
   and which is not audible (HDMI/DisplayPort audio of a dummy monitor, or a virtual cable if the
   owner approves ADR 0003's conditional recommendation).
3. No automatic workaround logic is implemented until the laptop test shows it is needed.

## Consequences

* F2 works without any driver (zero installation steps beyond Desk Bridge itself).
* The laptop may play sound locally unless the user mutes it; this is expected until M3b.
* Open item for the owner: the 1-minute mute test on the real laptop (see `docs/testing.md`,
  M3a).
