# ADR 0003 – Virtual audio driver / virtual microphone for F3 (M3b)

* Status: **Proposed**
* Date: 2026-10-16
* Scope: plan sections 8.4, 8.9, 12.1, 16, 18. Companion spike: ADR 0004 (virtual display).
* Files: `drivers/audio/README.md` (no binaries bundled, see Decision)

## Zusammenfassung für Thorben (Deutsch)

**Ergebnis des Spikes: Es gibt keinen Treiber, der die drei Muss-Kriterien aus Plan 8.9 wörtlich erfüllt. Es gibt aber genau einen realistischen Kandidaten mit Einschränkungen:
VB-CABLE (VB-Audio, Pack45).**

* **Der einzige quelloffene Kandidat mit Mikrofon (VirtualDrivers/Virtual-Audio-Driver, MIT) scheidet aus:** Das „Signed“-Release 25.7.14 ist nur von der *SignPath Foundation* signiert, nicht von Microsoft.
  Ich habe das mit `signtool verify /kp` geprüft: Die Kernel-Signaturprüfung schlägt fehl. Unter Secure Boot lädt Windows ihn nicht (Fehlercode 52; die Entwickler schreiben selbst, sie hingen „in der Attestation-Phase bei Microsoft“,
  und nur Test-Signing funktioniere). Außerdem hat er **keinen Datenpfad** zu einer eigenen App (das Mikro liefert nur Stille; der Lautsprecher verwirft die Daten; Shared Memory gibt es nur als kostenpflichtige Sonderanfertigung).
* **VB-CABLE ist von Microsoft signiert** (Katalog „Microsoft Windows Hardware Compatibility Publisher“, `signtool /kp` erfolgreich) und läuft damit mit Secure Boot. Es ist ein reines „Kabel“: Wir schreiben den Headset-Ton in „CABLE Input“,
  Teams & Co. lesen „CABLE Output“. Das reicht für F3 technisch aus. Einschränkungen: (1) **Lizenz** ist Donationware, kein Open Source, keine Änderung erlaubt (wir ändern aber nichts); die Webseite erlaubt ausdrücklich, VB-CABLE
  mit einer eigenen Anwendung und stiller Installation mitzuliefern (mit Herkunfts- und Spendenhinweis), die `readme.txt` im Paket sagt dagegen, Einbau in fremde Installer sei ohne Zustimmung des Autors nicht erlaubt. **Das muss VB-Audio schriftlich klären.**
  (2) Gerätenamen sind fest („CABLE Output (VB-Audio Virtual Cable)“), nicht „Desk Bridge Mikrofon“. (3) „Mikro in Benutzung“ muss über WASAPI-Sessions statt über den Treiber erkannt werden (Plan 8.9 erlaubt das als Ersatz).
* **Empfehlung:** *Bedingtes Ja* zu VB-CABLE für M3b, **Binärdateien aber noch nicht ins Repo kopieren** (Lizenz nicht eindeutig). Wenn du die Abweichung von Kriterium 2/3 akzeptierst **und** VB-Audio schriftlich zustimmt, wird VB-CABLE im Installer
  per Download+Hash-Pin mitgeliefert. Andernfalls gilt die Plan-Regel: **F3 zurückgestellt** (F2 Laptop-Ton → Headset bleibt unberührt).
* Beobachtungsliste: `NezucloudResearch/acxmic` (MIT, KMDF/ACX, mit echtem Datenpfad per `WriteFile`, aber Stand heute **ohne** Microsoft-Signatur, Repo erst vom 2026-10-08) und das Virtual-Audio-Driver-Projekt, falls dessen Attestation durchkommt.

## Context

F3 (plan 8.4) needs a virtual microphone on the laptop that Teams & co. can select, fed from the headset microphone on the PC. A virtual render endpoint ("speaker") is optional (F2 runs on WASAPI loopback without a driver).
Plan 8.9 fixes the evaluation criteria (in this order): (1) signed by Microsoft (attestation/WHQL), installable without test signing and with Secure Boot on – **must**; (2) licence allows redistribution and modification (MIT/BSD/Apache/MS-PL) – **must**;
(3) render and capture endpoint with a data path to our own app (shared memory/IOCTL), not just a "cable" – **must**; (4) reports capture-stream open/close (`MicInUse`) – should; (5) endpoint names configurable – should; (6) maintained, Win 10/11 x64 – should.
Rule: a ready-made driver that meets 1–3 is bundled **unchanged**; otherwise F3 is deferred. There is no budget for an EV certificate, so we cannot sign our own driver.

Technical background (Microsoft Learn): "Starting with Windows 10, version 1607, Windows will not load any new kernel-mode drivers which are not signed by the Dev Portal." Audio endpoint drivers (WaveRT/ACX miniports) are kernel-mode, so there is no user-mode escape hatch as there is for the display driver.

## Options considered

Legend: ✔ met, ✘ not met, ◐ partly/conditional, ? not verified. Criteria numbers = plan 8.9.

| Candidate | 1 MS-signed, Secure Boot | 2 Licence | 3 Data path to our app | 4 Stream open/close | 5 Names | 6 Maintained / OS | Verdict |
|---|---|---|---|---|---|---|---|
| **VB-CABLE Pack45** (VB-Audio) | ✔ catalog signed by *Microsoft Windows Hardware Compatibility Publisher*; `signtool /kp` OK | ◐ proprietary donationware; redistribution with an app + silent install allowed on website, readme says otherwise; no modification, no source | ◐ cable semantics: app plays into "CABLE Input", apps record "CABLE Output" (enough for our one-way mic) | ✘ in driver; ✔ substitute via WASAPI session enumeration on the capture endpoint | ✘ fixed `CABLE Input/Output` + "VB-Audio Virtual Cable" | ✔ Pack45 (INF 2024-10-07, zip refreshed 2025-12-05), Win 10/11 x64 + ARM64, vendor active | **conditional GO** |
| **VirtualDrivers Virtual-Audio-Driver** 25.7.14 "Signed" | ✘ SignPath Foundation (GlobalSign) only; `/kp` fails; users get Code 52 | ✔ MIT (+ MS-PL for sysvad parts) | ✘ none: capture side writes silence, render data discarded, no IOCTL, no shared memory (source reviewed) | ✘ | ✔ in INF source, but any change breaks signature | ◐ code until 2026-03, last release 2025-07-14; INF limited to build ≥22000 (#23) | **rejected** |
| NezucloudResearch **acxmic** | ✘ no Microsoft signature (README: "this repository does not contain one yet") | ✔ MIT (+ MS-PL ACX samples) | ✔ `\\.\PadDisplayMic` `WriteFile` s16 mono 48 kHz, `IOCTL_PADMIC_GET_STATUS` | ✔ `StreamsRunning` counter | ◐ fixed "PadDisplay Virtual Microphone" in INF | ◐ new (2026-10-08), 0 stars, tested only in Hyper-V test-signing | **watch list** |
| lswr1000 fork of Virtual-Audio-Driver | ✘ test signing required (README) | ✔ MIT | ✘ same driver | ✘ | – | – | rejected |
| Scream 4.0 | ✘ cross-signed; does not install under Secure Boot on fresh Win 10/11 (README), test mode on Win 11 | ✔ MS-PL | ◐ network sender, render only, **no capture endpoint** | ✘ | ✘ | ✘ last release 2022-09 | rejected |
| VAC (Eugene Muzychenko) | ? (commercial, not downloaded) | ✘ commercial ($30 home / $50 business); trial voice reminder; "custom versions" with own device names and unique key, companion app required | ◐ cable | ? | ✔ in custom version | ✔ 4.7x | rejected for now; **fallback if budget exists** (ask for quote) |
| Voicemeeter / Banana (VB-Audio) | ✔ same vendor, MS-signed (not downloaded) | ◐ may be distributed "AS IS" under donationware rules | ◐ cable | ✘ | ✘ | ✔ | not needed; bigger footprint than VB-CABLE |
| Steam Streaming Speakers / Microphone (Valve) | ✔ (Valve-signed, ships with Steam) | ✘ proprietary; installed by Steam; no redistribution right found | ◐ | ? | ✘ | ✔ | rejected (cannot be bundled) |
| Microsoft SysVAD / ACX samples | ✘ source only, needs own attestation signing (EV cert) | ✔ MS-PL | ✔ (build your own) | ✔ | ✔ | ✔ | not possible without EV cert (explicitly postponed in plan 8.9) |
| BlackHole | macOS only | GPL-3 | – | – | – | – | not applicable on Windows; keep in mind for the macOS port |
| Apollo / Sunshine / Moonlight | ship no audio driver of their own (Apollo calls the Steam drivers) | – | – | – | – | – | no candidate |
| Synchronous Audio Router, other test-signed projects | ✘ test signing | – | – | – | – | – | not considered |

## Decision / Recommendation

**Recommendation (1), conditional: use VB-CABLE Pack45 as the F3 virtual microphone, unmodified, subject to the preconditions below. Do not copy its binaries into the repository yet; document and pin them instead.**
If any precondition fails: **(2) F3 deferred**, exactly as plan 8.9 prescribes ("noch nicht verfügbar" in the UI, everything else unaffected).

Pinned artefact (verified 2026-10-16):

| Item | Value |
|---|---|
| Source | https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip (page: https://vb-audio.com/Cable/ ; "Pack45, OCT 2024, XP to WIN11 32/64 bits/Arm64") |
| ZIP SHA-256 | `b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb` (1 318 877 bytes, `Last-Modified: Fri, 05 Dec 2025`) |
| Driver files (x64, Win 10+) | `vbMmeCable64_win10.inf` (`DriverVer = 10/07/2024,3.3.1.7`), `vbaudio_cable64_win10.sys`, `vbaudio_cable64_win10.cat` |
| INF SHA-256 | `61c857be74831cc299d9be62f8d49d137f14063454fc54f859d5bc9b4b813daf` |
| SYS SHA-256 | `f01344602472f1b527de5ee98f18987c03db48fea444e457d061e937f1d531d5` |
| CAT SHA-256 | `0a921ebadbe39cf3fa7c14ff37d1ca22565a9651244bd3ac177dc810bc99072e` |
| Vendor installer (alternative path) | `VBCABLE_Setup_x64.exe` `734c35dfa6d98f48782a451633ceb471166ec70d60482fd89a1123d0ee3c4f41` (EV-signed by *BUREL VINCENT Entrepreneur individuel*) |
| hwid / device | `VBAudioVACWDM` (root-enumerated; INF `[Strings] HardwareId`), class MEDIA `{4d36e96c-e325-11ce-bfc1-08002be10318}`, service `VBAudioVACMME`, manufacturer "VB-Audio Software", device "VB-Audio Virtual Cable" |
| Endpoints | render: **CABLE Input** (VB-Audio Virtual Cable), capture: **CABLE Output** (VB-Audio Virtual Cable); default format 48 kHz, 24 bit, stereo (OEM format in INF); 16-channel variant "CABLE In 16 Ch" |
| Licence text location | `readme.txt` inside the ZIP (section LICENSE) and https://vb-audio.com/Services/licensing.htm (section "VB-CABLE Distribution with other product") |

F3 data flow with a cable: PC agent -> Opus over QUIC -> laptop agent decodes, drift-compensates, **opens a WASAPI shared-mode render stream on "CABLE Input"** and writes the headset audio; Teams/Zoom/Meet record **"CABLE Output"**. The agent must never capture "CABLE Output" itself. "Set as default device" (`IPolicyConfig`, plan 8.4) applies to the capture endpoint "CABLE Output".

`MicInUse` substitute: `IMMDevice::Activate(IAudioSessionManager2)` on the capture endpoint "CABLE Output", `RegisterSessionNotification` + `IAudioSessionControl::RegisterAudioSessionNotification` (state Active/Inactive) and `IAudioSessionControl2::GetProcessId` for the "Laptop uses your mic (Teams)" hint. Known gap: apps that open the endpoint in WASAPI *exclusive* mode do not show up as shared sessions (unverified for this driver; test in M3b).

**Install recipe for `deskbridge-setup` (to be validated on a clean Win 11 VM, Secure Boot on, no test mode):**
1. Detect an existing VB-CABLE (`SetupDiGetClassDevs` for hwid `VBAudioVACWDM`). If present: do nothing, reuse it, never uninstall it (not ours). Record whether we installed it.
2. Download-and-pin at **build time** (not committed): fetch the ZIP, check SHA-256 above, embed `vbMmeCable64_win10.inf/.sys/.cat` (and `readme.txt` as licence text) in the installer.
3. Primary path (plan 12.2): `DiInstallDriver(vbMmeCable64_win10.inf)` + `SetupDiCreateDeviceInfo`/`SetupDiCallClassInstaller(DIF_REGISTERDEVICE)` with hwid `VBAudioVACWDM` + `UpdateDriverForPlugAndPlayDevices`. Fallback path: run the vendor's own `VBCABLE_Setup_x64.exe -i -h` (community-documented switches, not in vendor docs; forum reports a Windows driver prompt still appears, not reproduced by us). The vendor's licensing page explicitly names "VB-CABLE silent installation" as allowed.
4. Wait for the endpoints to appear (`IMMNotificationClient::OnDeviceAdded`); a reboot is advised by the vendor readme – **verify whether it is really needed** (expected: no).
5. Uninstall: only if we installed it: `DiUninstallDriver` / remove device node.

Preconditions before bundling:
1. **Thorben's OK** for a non-OSS, donationware component (plan 12.1 table gets a row "VB-CABLE | VB-Audio donationware, redistribution per vb-audio.com | M3b"; plan 12.1 rule "new dependency needs ADR + OK").
2. **Written confirmation from VB-Audio** (https://vb-audio.com/Services/contact.htm) that an open-source (MIT OR Apache-2.0) application may embed the unmodified Pack45 files in its installer, silently install them, and host that installer on GitHub Releases. Reason: the licensing page allows it under the "donationware model is applicable" condition, but `readme.txt` in the package says copying/distributing the package as-is is fine while integrating it into another software's installation procedure needs the author's agreement. Two texts, two answers.
3. Compliance work: installer and app UI must mention "VB-CABLE by VB-Audio, www.vb-cable.com, donationware – all participations are welcome" and let users see/identify it as VB-Audio's product (so: do **not** rename the endpoints). For "professional" distribution to organisations where users cannot see or pay for it, VB-Audio asks for paid licences (volume price on their page: 3.61 EUR/unit at 100); decide how to word this for company users of Desk Bridge.
4. A clean-machine test (Secure Boot on) of the install recipe and of the WASAPI session-based `MicInUse`; measure the added latency of the cable (not known; budget 10–30 ms).

## Consequences

* Plan 8.9 criteria 2 and 3 are only partly satisfied by this choice; Thorben has to accept that deviation explicitly (or choose "F3 deferred").
* Plan 8.4 text "Desk Bridge Mikrofon" becomes "CABLE Output (VB-Audio Virtual Cable)" in other apps. The Desk Bridge UI can say "Desk Bridge speaks into: VB-CABLE".
* The mic is only as private as WASAPI sessions allow: any app can record "CABLE Output" at any time; our agent only *writes* when the PC side reports that someone listens (session detection) – the privacy design in 8.4 still works, but the signal comes from session monitoring, not from the driver.
* If the user already has VB-CABLE, VoiceMeeter, or other VB devices, they share the same driver/endpoints; we must coexist (reuse, never uninstall).
* The repository stays free of third-party binaries for audio (clean OSS licensing), at the cost of a build-time download with hash pinning.
* If VB-Audio declines: F3 deferred (plan 8.9), re-check list below.

## Re-check later (if F3 is deferred, or to improve on VB-CABLE)

* `NezucloudResearch/acxmic` (https://github.com/NezucloudResearch/acxmic): the closest technical fit (data path, stream counter, MIT). Needs a Microsoft attestation signature; watch for a signed release or a co-signing/sponsoring arrangement (EV certificate + Partner Center). 
* `VirtualDrivers/Virtual-Audio-Driver`: issue #15 comments say the project is "stuck in the attestation phase at Microsoft"; check releases for an attestation-signed build; shared-memory build is offered only as a paid custom build (contact@mikethetech.com), signing state unknown.
* VAC custom version (own device names, unique key): ask for a quote; WHQL status to be confirmed by the vendor.
* Sponsoring of signing costs after the OSS release (plan 8.9), or an EV certificate if the project pays off; then own SysVAD/ACX driver becomes possible.
* Windows platform changes (new in-box virtual audio devices, signing policy changes).

## Evidence

All checks read-only on 2026-10-16; files downloaded into `…\scratchpad\research\{vbcable,vad-audio-25.7.14,vdd-25.7.23}\` (never executed). Windows SDK `signtool.exe` 10.0.26100 used only with `verify`.

**VB-CABLE Pack45** – `Get-AuthenticodeSignature`:
```
vbaudio_cable64_win10.cat : Valid
  Signer  : CN=Microsoft Windows Hardware Compatibility Publisher, O=Microsoft Corporation, L=Redmond, S=Washington, C=US
  Issuer  : CN=Microsoft Windows Third Party Component CA 2014
  Validity: 2024-01-11 .. 2025-01-10, timestamp: CN=Microsoft Time-Stamp Service
vbaudio_cable64_win10.sys : Valid
  Signer  : CN=BUREL VINCENT Entrepreneur individuel … (GlobalSign GCC R45 EV CodeSigning CA 2020), valid 2023-04-03 .. 2025-06-21, GlobalSign RFC3161 timestamp
VBCABLE_Setup_x64.exe     : Valid, same EV signer
```
```
> signtool verify /kp /c vbaudio_cable64_win10.cat vbaudio_cable64_win10.sys
Successfully verified: …vbaudio_cable64_win10.sys        (exit 0)   <- kernel-mode signing policy satisfied
> signtool verify /pa /c vbaudio_cable64_win10.cat vbaudio_cable64_win10.sys
Successfully verified: …vbaudio_cable64_win10.sys        (exit 0)
```
Legacy files in the pack (xp/vista/win7/2003) carry 2013-2015 VeriSign cross-signatures; irrelevant for Win 10/11.

**VirtualDrivers Virtual-Audio-Driver** 25.7.14 "Signed" (https://github.com/VirtualDrivers/Virtual-Audio-Driver/releases/tag/25.7.14), asset `Virtual.Audio.Driver.Signed.-.25.7.14.zip`, SHA-256 `dd10560994de65a7e587fb8b93c0d7e9838292d9c3566a0976c2786d727292bd`:
```
VirtualAudioDriver.sys  sha256 646317270884b1052219a1f97c9d455f07dc7ff92beaecf9b79bcbbb855dde7a   Valid
virtualaudiodriver.cat  sha256 f741cf334fadc0d3d6532b40500375d940dc39be3e0d7c4b9701818ed848f9dd   Valid
  Signer: CN=SignPath Foundation, O=SignPath Foundation, L=Lewes, S=Delaware, C=US   (thumbprint 3CF8CF26D8BA266C3A483AB7D26D4A818E317D76)
  Issuer: CN=GlobalSign GCC R45 CodeSigning CA 2020; timestamp DigiCert; PKCS#7 blob: 1 signer, 3 certs (GlobalSign root, CA, SignPath) – no Microsoft cosignature
> signtool verify /kp /c virtualaudiodriver.cat VirtualAudioDriver.sys
SignTool Error: A certificate chain processed, but terminated in a root certificate which is not trusted by the trust provider.   (exit 1)
> signtool verify /pa …  -> Successfully verified (exit 0)
```
INF: hwid `ROOT\VirtualAudioDriver`, section `NTamd64.10.0...22000` (Windows 11 only), `KmdfLibraryVersion 1.15`, endpoints named "Virtual Audio Driver by MTT" / "Virtual Mic Driver by MTT".
Upstream README: "This software is in beta and requires test signing to be enabled"; "Contact us for quotes on a custom build" for "named pipes, shared memory buffers, direct integration". Source review (`Source/Main/minwavertstream.cpp`, `adapter.cpp`): capture stream `WriteBytes()` zero-fills the DMA buffer (comment: "Instead of generating a tone, just output silence"), the optional tone generator is disabled by default (`g_DisableToneGenerator = 1`), no `IRP_MJ_DEVICE_CONTROL`/IOCTL and no section/shared-memory code anywhere in `Source/`; render data is discarded by default (`g_DoNotCreateDataFiles = 1`, otherwise only the SysVAD debug "save to file" path).
Issues: #15 "Driver Signature Problem" (Code 52 on Win 11; comment quoting the maintainer: "All will work in test signing mode, but we're stuck in the attestation phase at Microsoft"), #13 (Code 52), #16, #23 (signed INF restricted to build 22000), #18 "Blank audio from virtual mic" (by design).
https://github.com/VirtualDrivers/Virtual-Audio-Driver/issues/15

**Licence texts** – VB-Audio: https://vb-audio.com/Services/licensing.htm, section "VB-CABLE Distribution with other product": distributing the package with an application (free or commercial) and embedding it in an installation package with VB-CABLE silent installation is allowed if the donationware model stays applicable (user can see/identify VB-CABLE as VB-Audio's product and can donate; mention origin www.vb-cable.com and "donationware"); significant companies are encouraged to donate a licence fee; professional distribution where users cannot pay requires purchased licences; VB-CABLE A+B / C+D cannot be bundled. `readme.txt` (in ZIP): copying/diffusing the package as-is without modification is allowed; integrating it into another software installation procedure is not allowed without author agreement.

**Other candidates** – acxmic README/SIGNING.md (https://github.com/NezucloudResearch/acxmic; created 2026-10-08, MIT; says PadDisplay falls back to VB-CABLE until a Microsoft signature exists). Scream README (https://github.com/duncanthrax/scream; MS-PL, release 4.0 2022-09-17; Secure Boot/Win 11 caveats). VAC custom versions: https://vac.muzychenko.net/en/customversion.htm and purchase page ($30/$50). Microsoft: https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-
Silent switches: https://forum.vb-audio.com/viewtopic.php?p=7129 (forum posts, `-i -h`, driver prompt reported; vendor staff: installing a driver normally asks for user agreement).

Confidence: **High** – Microsoft signature of VB-CABLE, failure of the Virtual-Audio-Driver signature under kernel policy, no data path in the latter, licence texts as quoted. **Medium** – that VB-CABLE installs with Secure Boot on without any prompt through our own SetupAPI path (expected for WHQL/attestation catalogs, not tested); session-based mic detection. **Not verified** – cable latency, exclusive-mode apps, reboot need, VAC/Voicemeeter signatures, VB-Audio's answer to the redistribution question.
