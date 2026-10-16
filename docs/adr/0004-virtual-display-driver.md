# ADR 0004 – Virtual display driver for focus mode (M5b)

* Status: **Proposed**
* Date: 2026-10-16
* Scope: plan sections 8.8, 8.9, 12.1, 16. Companion spike: ADR 0003 (virtual audio).
* Files: `drivers/display/` (package, config template, README)

## Zusammenfassung für Thorben (Deutsch)

**Empfehlung: Den „Virtual Display Driver“ (VirtualDrivers/MikeTheTech, MIT) mitliefern, und zwar die x64-Version 24.12.24 (identisch in Release 25.7.23).**
Er steht bereits unter `drivers/display/package/` (unverändert, mit Lizenz). Die Signatur habe ich selbst geprüft: gültig, aber von der
**SignPath Foundation** (GlobalSign-Zertifikat), **nicht von Microsoft**. Das ist hier in Ordnung, weil der Treiber ein *User-Mode*-Treiber
(UMDF/IddCx) ist: Die strenge Kernel-Signaturpflicht (Secure Boot) gilt nur für Kernel-Treiber. Windows verlangt für die Installation aber, dass der
Herausgeber vertrauenswürdig ist. Ohne Vorbereitung erscheint einmal die Rückfrage „Möchten Sie diese Gerätesoftware installieren?“. Unser Installer
legt deshalb vorab genau dieses eine Zertifikat (Fingerabdruck fest eingetragen) in „Vertrauenswürdige Herausgeber“ – das ist der Weg, den Plan 8.9 schon vorsieht.
Konfiguration: eine XML-Datei (`C:\VirtualDisplayDriver\vdd_settings.xml`) plus eine Named Pipe zum Neuladen; die Render-GPU wird per Name der NVIDIA-GPU gewählt
(Standard: die GPU mit dem meisten VRAM, bei einem Optimus-Laptop also die dGPU). Ein Monitor lässt sich zur Laufzeit nur durch **Aktivieren/Deaktivieren des Geräts** ein- und ausblenden (kein „Monitor
hinzufügen“-Befehl). **Nicht geprüft** (geht nur auf einem sauberen Rechner): echte stille Installation, 5120×1440@120 auf der RTX 5060, Dauer des Ein-/Ausblendens.
Risiken: letzter signierter Release ist über ein Jahr alt; offene Fehler zu `DXGI_ERROR_ACCESS_LOST` und Instabilität unter Windows 11 Build 26200; die Named Pipe
ist für alle lokalen Prozesse offen. Die Alternativen (SudoVDA, virtual-display-rs, Parsec VDD, usbmmidd_v2) fallen wegen selbstsignierter Zertifikate bzw. Lizenz aus.

## Context

Plan 8.8 wants a virtual monitor for focus mode (laptop streams to the PC, native mode of the target monitor, e.g. 5120×1440@120, split variants 3413/1707×1440),
installed silently by `deskbridge-setup.exe`, with a selectable render GPU (Optimus: the NVIDIA dGPU with NVENC) and **no virtual monitor while focus mode is off**.
Hard constraints from 8.9/12.1: no EV certificate budget, only already-signed redistributable drivers, installable with Secure Boot on and without test signing, "zero manual work".
Plan 8.9 states that the display driver is "already signed, low risk, if the ADR in M5b confirms it". This ADR checks exactly that.

## Options considered

| | **VirtualDrivers VDD** (MttVDD) | SudoVDA (SudoMaker / Apollo) | MolotovCherry virtual-display-rs | Parsec VDD | Amyuni usbmmidd_v2 |
|---|---|---|---|---|---|
| Licence | **MIT** (`LICENSE`, verified; derived from MS IDD sample, MS-PL) | README: "MIT and CC0 or Public Domain"; **no LICENSE file** in the repo (GitHub: none), binary only inside GPL-3 Apollo | MIT (GitHub) | Parsec proprietary driver; "available to Parsec Teams and Warp customers" | not found / commercial (could not verify) |
| Latest release | `25.7.23` (2025-07-23); x64 binary of 2024-12-24; master active until 2026-06 | no releases; Apollo v0.4.6 (2025-07-13) | v0.3.1 (2023-10-22), repo last push 2025-03 | 0.45 (IddCx 1.5) | n/a |
| Signature of released x64 package | **SignPath Foundation / GlobalSign, valid** (`signtool /pa` ok; **not** Microsoft, `/kp` fails) | **self-signed `CN=sudovda@su.mk`**, `install.bat` adds it to the *Root* and TrustedPublisher stores | **self-signed** `DriverCertificate.cer`, user must add to Root + TrustedPublisher | SignPath (per README), not evaluated | not evaluated |
| Installable w/o test mode, Secure Boot on | yes for UMDF, after publisher trust (see Decision) | only after trusting an unknown root | only after trusting an unknown root | n/a | n/a |
| Custom modes (5120×1440@120 …) | `vdd_settings.xml`, arbitrary w×h@rate (float), limit ≈100 modes | per-monitor mode passed at runtime (IOCTL), 640×480 … 8K, up to 500 Hz | per-monitor modes via IPC, up to 10 monitors | max 5 custom modes (registry), ≤4K/240 Hz, **no HDR** -> 5120×1440 impossible without patching | n/a |
| Render GPU choice | `<gpu><friendlyname>` / `SETGPU` (DXGI description; default = most VRAM) | registry `gpuName`, `IOCTL_SET_RENDER_ADAPTER` (LUID) | not found | not found | n/a |
| Runtime monitor add/remove | **no** (count fixed at adapter start; use device enable/disable) | **yes** (`IOCTL_ADD_/REMOVE_VIRTUAL_DISPLAY`, watchdog) | **yes** (IPC) | yes (IOCTL) | n/a |
| HDR | yes (HDR10/12, Win 11 23H2+) | yes (24H2) | no | no | no |
| Verdict | **recommended** | functional benchmark, trust model unacceptable | rejected (self-signed root, stale) | rejected (licence, limits) | rejected (licence not established) |

Other notes:
* **Sunshine** (LizardByte) has no driver of its own; a Sunshine PR (VDD #487, merged 2026-06-22) says they "are considering integrating VDD directly into Sunshine". Apollo uses SudoVDA. -> VDD is the de-facto standard in that ecosystem.
* **LizardByte/libdisplaydevice** (MIT, active 2026-10) is a C++ library for display topology save/restore; a good reference for `crates/display` topology handling (not a driver).
* Criteria from plan 8.9 for audio (WHQL/attestation) do **not** transfer: an IddCx driver is user-mode (`UmdfService`, `UmdfExtensions = IddCx0102` in `MttVDD.inf`).

## Decision / Recommendation

**Bundle VirtualDrivers/Virtual-Display-Driver, x64, driver version 24.12.24 (INF `DriverVer = 12/24/2024,11.30.4.434`), unmodified.**

Package (copied to `drivers/display/package/`, hashes verified after copy):

| File | SHA-256 |
|---|---|
| `MttVDD.dll` | `C9CA837F57A98FBD43BC416A7F535A95843626E7759EAF85CF0CD7CE334DBB05` |
| `MttVDD.inf` | `550D211FE481E74DFE3F9D724ED78BE48B3A9113405965D683D9373E8D672F5D` |
| `mttvdd.cat` | `08A0093FC9B2E32B287A6F8A77CA4DE0A31830D29FC33D2B13A918DC859468F6` |
| `LICENSE` (MIT) | `C2285C867DDC9B88F43B4F8FA2C77A9EEC66207166C1217B37353A2CCD62AC03` |

Source: https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/24.12.24 (`Signed-Driver-v24.12.24-x64.zip`, SHA-256 `f93e7ce3…da82fa4`);
byte-identical files in https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.7.23 (`VirtualDisplayDriver-x86.Driver.Only.zip`, SHA-256 `e2421069…e418b3a`; "x86" is a misnomer, PE machine = 0x8664).

* **hwid:** `Root\MttVDD` (also `MttVDD`), class `Display` `{4D36E968-E325-11CE-BFC1-08002BE10318}`, UMDF service `MttVDD` with `UmdfLibraryVersion=2.25.0`, extension `IddCx0102`, upper filter `IndirectKmd`.
* **Install recipe** (details in `drivers/display/README.md`): (1) write `vdd_settings.xml` from `config/vdd_settings.deskbridge.xml` to `C:\VirtualDisplayDriver`; (2) import only the SignPath leaf
  certificate (thumbprint pin `3CF8CF26D8BA266C3A483AB7D26D4A818E317D76`) from `mttvdd.cat` into `LocalMachine\TrustedPublisher`; (3) `DiInstallDriver` + root device node `Root\MttVDD` through SetupAPI; (4) disable the device after install unless focus mode is on.
* **Runtime (`crates/display`):** modes + GPU through the XML; reload via pipe `\\.\pipe\MTTVirtualDisplayPipe` (`RELOAD_DRIVER`, `SETGPU "<name>"`); virtual monitor on/off through enabling/disabling the device node; topology backup/restore by us (libdisplaydevice is a good model).
* **Default modes** (`config/vdd_settings.deskbridge.xml`): 5120×1440@120/60, 2560×1440@120/60, 3413×1440@120, 1707×1440@120, 1920×1080@60 (safe). If odd widths are rejected: 3414 + 1706 (sums to 5120).

## Consequences

* Plan 8.8/12.1 stay valid; 8.9's sentence "Display-Treiber bereits signiert" must be refined to: *signed by SignPath Foundation (publisher signature), UMDF, installs after pre-trusting that publisher*. Needs Thorben's OK because the installer **modifies a certificate store** (TrustedPublisher): any software signed by this same SignPath Foundation certificate (it signs many OSS projects) would also be treated as a trusted publisher for driver-install prompts. Mitigation: pin the leaf thumbprint, remove it on uninstall if we added it, mention it in the installer text. The alternative is one Windows prompt for the user (violates "zero manual work", but is a click, not a BIOS change).
* The shipped x64 binary is from 2024-12-24; the 2025/2026 fixes in master (HDR bit depth, PCI-bus GPU selection, multi-monitor swap-chain) are **not** available in a signed build. We cannot produce signed builds ourselves. Track upstream releases; re-run this verification for every version bump (Authenticode, `signtool /pa`, hash, thumbprint).
* Monitor hot-plug is "device enable/disable", not "add monitor": `crates/display` needs a device-toggle helper (admin, SetupAPI) and must handle the 1-3 s settle time plus the "disable while an app uses it" case.
* Stability budget in M5b: spike items for DXGI desktop duplication loss, 884 Mpx/s mode, odd widths, Windows 11 build 26200 (our dev machine is 10.0.26200), HAGS on/off.
* ARM64 is not possible (WHQL required there, upstream #197/#465). Windows 10/11 in S mode is not supported (needs WHQL).
* The pipe is world-accessible (`D:(A;;GA;;;WD)`): any local process can reload the driver or change count/GPU. Impact: local DoS of the virtual monitor; no privilege escalation seen in the code reviewed. Accept, document, and do not store secrets in the XML.

## Risks and open items

1. **Not verified on a clean machine:** silent install with Secure Boot (we rely on MS Learn rules + upstream issue #315 / community script), device enable/disable behaviour, 5120×1440@120 on an RTX 5060 Laptop, desktop duplication on the IddCx output, Optimus render path.
2. Upstream bus factor: two maintainers, last signed release 15 months old, SignPath leaf certificate expires 2027-09-07 (existing signatures stay valid via timestamp, but a new release will carry a new leaf -> pin must be updated).
3. Open upstream bugs that may bite: #399 (DXGI_ERROR_ACCESS_LOST after mode change), #268 (fps drops in exclusive fullscreen), #490 (instability on build 26200 with 4 monitors), #254/#82 (init failure with many modes), "update GPU driver -> black screen" warning.
4. Licence: MIT is clear; the repository also carries MS-PL-derived Microsoft sample code. `NOTICE.md` records it. No separate `THIRD_PARTY_NOTICES` file exists upstream for the display driver.

## Evidence

All commands were run read-only on 2026-10-16 against files downloaded into `…\scratchpad\research\vdd-25.7.23\` and `…\vdd-25.5.2\` (nothing executed, nothing installed).

Release list (`gh release list -R VirtualDrivers/Virtual-Display-Driver`): `25.7.23` Latest 2025-07-23 ("Beta: Virtual Driver Control"), `25.5.2`, `24.12.24` (2024-12-24, "officially signed certificates")…
Release assets: 25.7.23 = `VDD.Control.25.7.23.zip`, `VirtualAudioDriver-x86.Driver.Only.zip`, `VirtualDisplayDriver-ARM64.Driver.Only.zip`, `VirtualDisplayDriver-x86.Driver.Only.zip`.

SHA-256 of downloaded archives (`sha256sum`):
```
f93e7ce3d640c83c419b1b72d2c83b2d8e34d83e8b7ba11f3328a40d2da82fa4  Signed-Driver-v24.12.24-x64.zip
e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a  VirtualDisplayDriver-x86.Driver.Only.zip   (25.7.23, actually x64)
dcd487161469c726bab60f174a7e7f70ee02afd437cad0d5c0e6755953de5a25  VirtualDisplayDriver-ARM64.Driver.Only.zip (25.7.23)
a701f2272e9fcf382849b24f913c6dd07597b3b1116525f2e90182f019609154  VDD.Control.25.7.23.zip (71 MB GUI; not used by us)
```

`Get-AuthenticodeSignature` (x64 package, `…\vdd-25.5.2\x\`):
```
mttvdd.cat : Valid | Signatur wurde überprüft.
  Signer  : CN=SignPath Foundation, O=SignPath Foundation, L=Lewes, S=Delaware, C=US
  Issuer  : CN=GlobalSign GCC R45 CodeSigning CA 2020, O=GlobalSign nv-sa, C=BE
  Validity: 12/17/2024 – 09/07/2027, Thumbprint 3CF8CF26D8BA266C3A483AB7D26D4A818E317D76
  Timestamp: CN=DigiCert Timestamp 2024
MttVDD.dll : Valid, same signer/thumbprint/timestamp
```
`signtool verify` (Windows SDK 10.0.26100, `/c <catalog>`):
```
signtool verify /pa /c mttvdd.cat MttVDD.dll   -> "Successfully verified: MttVDD.dll"   (exit 0)
signtool verify /kp /c mttvdd.cat MttVDD.dll   -> "A certificate chain processed, but terminated in a root certificate
                                                   which is not trusted by the trust provider." / File not valid (exit 1)
```
Reading: valid publisher signature, **no Microsoft root** -> would be rejected as a *kernel* driver, irrelevant for UMDF.

PE / INF facts: `MttVDD.dll` machine `0x8664` (x64); INF is UTF-16 LE: `[Standard.NTamd64] %DeviceName%=MyDevice_Install, Root\MttVDD` and `, MttVDD`; `UmdfExtensions = IddCx0102`; `UpperFilters = IndirectKmd`; `Provider=MikeTheTech`; class Display.
The ARM64 build in 25.7.23 is newer (2025-07-15) than the x64 build (2024-12-24); the x64 DLL lacks e.g. the `ColourFormat` strings that the ARM64 DLL has.

Driver control surface, checked in `Driver.cpp` (master) **and** in the UTF-16 string table of the shipped `MttVDD.dll`: pipe `\\.\pipe\MTTVirtualDisplayPipe`; commands `RELOAD_DRIVER`, `SETGPU`, `SETDISPLAYCOUNT`, `GETSETTINGS`, `GETALLGPUS`, `GETASSIGNEDGPU`, `D3DDEVICEGPU`, `IDDCXVERSION`, `PING`, `LOG_DEBUG`, `LOGGING`, `HDRPLUS`, `SDR10`, `CUSTOMEDID`, `PREVENTSPOOF`, `CEAOVERRIDE`, `HARDWARECURSOR` all present; `VDDPATH` registry value present. Config path default `C:\VirtualDisplayDriver\vdd_settings.xml`.
Monitors are created in `IndirectDeviceContext::FinishInit()` (`for i < numVirtualDisplays: CreateMonitor(i)` -> `IddCxMonitorArrival`), i.e. at adapter start. GPU: `AdapterOption::selectBestGPU()` = adapter with largest `DedicatedVideoMemory`; `apply()` calls `IddCxAdapterSetRenderAdapter`.

Why UMDF may be installed without a Microsoft signature (Microsoft Learn):
* "Starting with Windows 10, version 1607, Windows will not load any new kernel-mode drivers which are not signed by the Dev Portal." (kernel-mode only) – https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-
* "To be considered signed by PnP device installation, the catalog file … must be signed by WHQL or signed by a third-party release certificate" and "editions for architectures other than x86 and amd64 require catalog files … signed by WHQL" – https://learn.microsoft.com/en-us/windows-hardware/drivers/install/pnp-device-installation-signing-requirements--windows-vista-and-later-

Upstream evidence on installation:
* Issue #315 "Silent Installation not possible?": `pnputil /add-driver MttVDD.inf /install` -> "The publisher of an Authenticode(tm) signed catalog has not yet been established as trusted"; maintainer: use devcon/nefcon; commenter: first install shows a trust prompt for *SignPath Foundation* – https://github.com/VirtualDrivers/Virtual-Display-Driver/issues/315
* Upstream script importing the catalog's certificates into `Cert:\LocalMachine\TrustedPublisher` then `nefconc install MttVDD.inf Root\MttVDD` – https://github.com/VirtualDrivers/Virtual-Display-Driver/blob/master/Community%20Scripts/silent-install.ps1
* ARM64: #197, #465 (needs test signing/WHQL) – confirms the MS Learn rule.
* README comparison table marks VDD "Signed ✅", virtual-display-rs not signed; README: "Free code signing on Windows provided by SignPath.io, certificate by SignPath Foundation".

Rejected alternatives, evidence:
* SudoVDA: `sudovda.cer` = `CN=sudovda@su.mk` (self-issued, 2025-07-13 – 2030-07-13); `Get-AuthenticodeSignature sudovda.cat` -> `UnknownError … terminated in a root certificate which is not trusted`; Apollo's `install.bat` runs `certutil -addstore -f root "sudovda.cer"`. IOCTL header (`sudovda-ioctl.h`) shows `IOCTL_ADD_VIRTUAL_DISPLAY` (w, h, refresh, GUID), `IOCTL_REMOVE_VIRTUAL_DISPLAY`, `IOCTL_SET_RENDER_ADAPTER`, hwid `root\sudomaker\sudovda`, interface GUID `{e5bcc234-1e0c-418a-a0d4-ef8b7501414d}`. Technically the best API; unacceptable trust model and no standalone release/licence file. Worth re-checking if SudoMaker obtains a real certificate.
* virtual-display-rs README: "The certificate needs installation for Windows to accept the driver … `certutil -addstore -f root`".
* Parsec VDD README (github.com/nomi-san/parsec-vdd): "available to Parsec Teams and Warp customers", up to 4K/240 Hz, max 5 custom resolutions, no HDR.

Confidence: **High** for licence, signature facts, file hashes, config/pipe surface (read from source and from the shipped binary). **Medium** for "installs silently with Secure Boot after TrustedPublisher import" (documentation + third-party reports, no own test). **Low/unknown** for runtime behaviour at 5120×1440@120 and for hot-plug timing.
