# ADR 0008 - Control at the Windows logon screen (agent before sign-in)

* Status: **Proposed** (implemented; needs the manual acceptance test below)
* Date: 2026-10-27
* Deciders: Thorben (owner)
* Related: ADR 0006 (service, secure-desktop helper; this closes its risk "the logon screen after
  a reboot cannot be driven"), ADR 0007 (picture of the secure desktop); `crates/agent/src/logon.rs`,
  `crates/service/src/supervisor.rs`, `EngineParams::logon_screen`

## Context

New requirement (owner, 2026-10-27): as soon as Windows has booted, before anybody signs in, the
device must already be controllable from the partner, so the password can be typed with the PC's
keyboard.

Until now that was impossible: the agent is a user process (scheduled task at logon, or the
service's `CreateProcessAsUser` with the user's token). Before the sign-in there is no user, no
`%APPDATA%` profile with identity and pairing, and no agent. Lock screen and UAC already work
because the user's agent keeps running there (ADR 0006).

Constraints:

* The logon screen is on `winsta0\Winlogon` of the console session; only SYSTEM can inject there
  (the helper of ADR 0006 already does).
* The partner pins the device's certificate (mTLS): an agent before the sign-in must present the
  **same identity**, otherwise the PC rejects it. The private key is protected with *user* DPAPI,
  which SYSTEM cannot decrypt.
* Exactly one process may hold UDP 47820 and the partner: at the sign-in the user's agent must take
  over without fighting the logon agent (single-instance mutex, port, `exit 0 = do not restart`).

## Decision

1. **Logon-screen agent**: the same `deskbridge-agent.exe`, started by the service with
   `--logon-screen` as SYSTEM in the console session on `winsta0\Winlogon` (the helper's token and
   desktop). The engine runs with `EngineParams::logon_screen = true`:
   * input target only (`Config::restrict_to_logon_screen`: no roles, so no capture, headset or
     focus source; no clipboard, audio, microphone, update checks or partner installs);
   * `Hello` capabilities `input`, `logs`, `session.logon-screen` only; control messages other
     than `SendSas` and `LogsRequest` are dropped; no display/audio recovery at start; the config
     is never written;
   * no UI pipe; own mutex `Global\softwelop-deskbridge-logon-agent`, so the user's agent never
     thinks it is already running; logs to `%ProgramData%\softwelop\DeskBridge\logs\logon-agent.log.*`
     (part of the partner's log view);
   * input reaches the logon screen through the helper (`SecureDesktopInjector`, unchanged),
     Ctrl+Alt+Del through the service (`SendSAS`), which matters on domain PCs with "Strg+Alt+Entf
     drücken".
2. **Profile copy**: the user's agent keeps a copy of `identity.bin`, `peers.toml` and
   `config.toml` in `%ProgramData%\softwelop\DeskBridge\logon\config` (checked every 20 s, written
   only when an input changed). The identity is re-encrypted with **machine-scope DPAPI**; the
   directory is created with `O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` (owner Administrators,
   SYSTEM and Administrators only, nothing inherited from `%ProgramData%`). A directory with a
   foreign owner is deleted and recreated. Only a **target device** keeps a copy (the laptop): on
   the input source (`device.input_source`, the PC) there would be nothing to receive. Setting
   `general.logon_screen` (default **on**, UI on target devices: *Einstellungen → Schon am
   Anmeldebildschirm steuern*) off deletes the copy.
3. **Trust check** in the logon agent: root, config directory and the three files must be owned
   by SYSTEM or Administrators, otherwise it refuses (exit 0). A standard user cannot pre-create
   the directory or plant a `peers.toml` to make a SYSTEM process accept their device.
4. **Service**: `decide_logon_agent` (pure, tested): run while there is a console session, nobody
   is signed in, the profile copy exists and no user's agent holds the mutex (fast user switching).
   It is terminated on the sign-in (session-change event, same tick, *before* the user's agent is
   adopted or started) and the service waits for its exit so the port is free. Backoff as for the
   agent; exit code 0 suppresses it until the console session or its user changes. Stopping the
   service also stops it (nobody else would end it at the sign-in). The **helper now runs as soon as
   a console session exists**, not only with a signed-in user; without a user its pipe is
   SYSTEM-only.
5. **Self-stop**: the logon agent also ends itself within a second when a user is signed in at the
   console, in case the service could not stop it.
6. **Partner side**: a peer with `session.logon-screen` gets no clipboard, audio or file streams;
   the UI shows "<Name> ist am Anmeldebildschirm" (`ConnectionDto::peer_logon_screen`).
7. Uninstall deletes `%ProgramData%\softwelop\DeskBridge\logon` (it contains the device key).

## Consequences and risks

| Risk | Impact | Mitigation / status |
|---|---|---|
| Device key in a second place (machine DPAPI: every *process that can read the file* can decrypt it) | key theft by an administrator | the file is readable by SYSTEM/Administrators only; an administrator could read the user's key anyway (or become SYSTEM). Switch the setting off to remove the copy |
| A paired partner can type at the logon screen | intended; same trust as for the lock screen today | setting to switch it off; only the pinned partner gets in |
| Copy is stale (paired/renamed while the agent ran without admin rights) | logon agent connects with old data or not at all | the copy needs the elevated agent (the elevation warning already asks for it); failures are logged once ("logon-screen profile not updated") |
| Wi-Fi before the sign-in | without a network the partner cannot reach the laptop | Windows connects to Wi-Fi profiles saved "for all users" (the default) before the sign-in; per-user Wi-Fi or VPN-only networks do not work. LAN is unaffected |
| Port race at the sign-in | user's agent fails to bind | logon agent ended (and awaited) in the same supervisor tick as the logon event, before the scheduled task's agent usually starts; a failed agent is restarted by the supervisor after its grace period |
| No picture of the logon screen in focus mode | the PC shows nothing while the laptop sits at the logon screen | input works with the laptop's own screen; the picture is ADR 0007 |
| Credential providers rejecting synthetic input (Windows Hello, some smart-card providers) | password may not be typable | as for the lock screen (ADR 0006): to be verified on the laptop |

## Manual acceptance test (owner, laptop)

1. Install on both devices, sign in once on the laptop (the user's agent writes the copy: log line
   `logon-screen profile result=Written`, folder `C:\ProgramData\softwelop\DeskBridge\logon` exists
   and is not readable for a standard user).
2. Restart the laptop, do not touch it. Within a few seconds after the logon screen appears the PC
   shows "Laptop ist am Anmeldebildschirm" (service log: `logon-screen agent started`).
3. Move the mouse over to the laptop, dismiss the lock curtain, type the password with the PC's
   keyboard (on a domain PC first Ctrl+Alt+End for Ctrl+Alt+Del), press Enter.
4. After the sign-in: service log `stopping logon-screen agent: user signed in`, then the user's
   agent connects; clipboard and laptop sound work again.
5. Sign out on the laptop: the logon agent comes back. Switch the setting off, restart: nothing at
   the logon screen, the `logon` folder is gone.
