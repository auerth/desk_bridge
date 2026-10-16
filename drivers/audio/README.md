# drivers/audio – virtual microphone (F3), M3b

Status: **no driver is bundled here (on purpose).** Decision pending, see `docs/adr/0003-virtual-audio-driver.md` (Proposed, 2026-10-16).

Why `package/` does not exist:

* The only candidate that passes plan 8.9 criterion 1 (Microsoft-signed, loads with Secure Boot) is **VB-CABLE** (VB-Audio). Its licence is
  donationware, not open source. VB-Audio's website allows bundling it with an application including a silent install, but the `readme.txt`
  inside the package says integrating it into another installer needs the author's agreement. That contradiction must be resolved in writing
  before the files are redistributed (neither in this repo nor in our installer). The plan's rule is "only if the licence clearly permits".
* The only open-source candidate with a microphone (VirtualDrivers/Virtual-Audio-Driver, MIT) is **not** Microsoft-signed (SignPath Foundation only),
  fails the kernel-mode signing check, and has no data path to a user-mode app. Not bundled.

## Candidate to bundle after approval: VB-CABLE Pack45 (unmodified)

| | |
|---|---|
| Download | https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip |
| ZIP SHA-256 | `b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb` (1 318 877 bytes) |
| Files for Win 10/11 x64 | `vbMmeCable64_win10.inf` `61c857be74831cc299d9be62f8d49d137f14063454fc54f859d5bc9b4b813daf` · `vbaudio_cable64_win10.sys` `f01344602472f1b527de5ee98f18987c03db48fea444e457d061e937f1d531d5` · `vbaudio_cable64_win10.cat` `0a921ebadbe39cf3fa7c14ff37d1ca22565a9651244bd3ac177dc810bc99072e` |
| Vendor setup (alternative) | `VBCABLE_Setup_x64.exe` `734c35dfa6d98f48782a451633ceb471166ec70d60482fd89a1123d0ee3c4f41` |
| Signature | catalog: **Microsoft Windows Hardware Compatibility Publisher** (cert 2024-01-11..2025-01-10, Microsoft timestamp), `.sys`: vendor EV certificate (GlobalSign) + the Microsoft catalog. `signtool verify /kp /c <cat> <sys>` = *Successfully verified* (kernel-mode policy). |
| hwid | `VBAudioVACWDM` (root-enumerated), class MEDIA, service `VBAudioVACMME`, manufacturer "VB-Audio Software" |
| Endpoints | render **"CABLE Input (VB-Audio Virtual Cable)"**, capture **"CABLE Output (VB-Audio Virtual Cable)"**, 48 kHz / 24 bit / stereo default |
| Licence | `readme.txt` in the ZIP + https://vb-audio.com/Services/licensing.htm ("VB-CABLE Distribution with other product"). Must show: origin www.vb-cable.com and "VB-CABLE is a donationware, all participations are welcome"; users must be able to identify it as VB-Audio's product (do not rename the endpoints); no modification. |
| Silent install | our SetupAPI path (see ADR 0003) or vendor `VBCABLE_Setup_x64.exe -i -h` (community-documented, not verified) |
| Last release | Pack45 (2024-10; ZIP re-published 2025-12-05) – still the current package on https://vb-audio.com/Cable/ on 2026-10-16 |

Verification snippet for the build pipeline (PowerShell; run it on the *downloaded, not yet extracted-and-executed* ZIP):

```powershell
$zip = "VBCABLE_Driver_Pack45.zip"
if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne "B950E39F01AF1D04EA623C8F6D8EB9B6EA5C477C637295FABF20631C85116BFB") { throw "VB-CABLE hash mismatch" }
Expand-Archive $zip -DestinationPath vbcable -Force
$cat = Get-AuthenticodeSignature vbcable\vbaudio_cable64_win10.cat
if ($cat.Status -ne "Valid" -or $cat.SignerCertificate.Subject -notlike "CN=Microsoft Windows Hardware Compatibility Publisher*") { throw "unexpected catalog signer" }
```

## Not verified / to do in M3b

Clean-VM install with Secure Boot on (prompt? reboot needed?), added latency of the cable, WASAPI-session-based `MicInUse` incl. exclusive-mode apps,
written OK from VB-Audio. If any of it fails: F3 deferred, UI says "noch nicht verfügbar" (plan 8.9).

## Watch list (re-check every few months)

`NezucloudResearch/acxmic` (MIT, real data path, but no Microsoft signature yet) · `VirtualDrivers/Virtual-Audio-Driver` (attestation pending) ·
VAC custom version (commercial) · sponsoring of EV/attestation cost after the OSS release.
