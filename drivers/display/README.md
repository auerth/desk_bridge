# drivers/display – Virtual Display Driver (IddCx, MIT)

Status: **candidate bundled for M5b, recommended in `docs/adr/0004-virtual-display-driver.md` (Proposed).**
Nothing in this directory has been installed or executed; everything below is from static inspection
of the files, the upstream source and the upstream issue tracker (2026-10-16).

## What is in `package/`

| File | Origin | SHA-256 |
|---|---|---|
| `MttVDD.dll` (x64 UMDF/IddCx driver, 251 640 bytes) | upstream release `24.12.24`, asset `Signed-Driver-v24.12.24-x64.zip`; byte-identical in release `25.7.23`, asset `VirtualDisplayDriver-x86.Driver.Only.zip` (the "x86" in the name is wrong, the PE machine type is 0x8664 = x64) | `C9CA837F57A98FBD43BC416A7F535A95843626E7759EAF85CF0CD7CE334DBB05` |
| `MttVDD.inf` (UTF-16, `DriverVer = 12/24/2024,11.30.4.434`) | same | `550D211FE481E74DFE3F9D724ED78BE48B3A9113405965D683D9373E8D672F5D` |
| `mttvdd.cat` | same | `08A0093FC9B2E32B287A6F8A77CA4DE0A31830D29FC33D2B13A918DC859468F6` |
| `LICENSE` (MIT) | upstream repo `LICENSE`, verbatim | `C2285C867DDC9B88F43B4F8FA2C77A9EEC66207166C1217B37353A2CCD62AC03` |
| `NOTICE.md`, `.gitattributes` | written by us | – |

Source release page: https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/24.12.24
(and `.../25.7.23`, the newest release, 2025-07-23; its x64 files are the ones above).
Release zip hashes: `Signed-Driver-v24.12.24-x64.zip` = `F93E7CE3D640C83C419B1B72D2C83B2D8E34D83E8B7BA11F3328A40D2DA82FA4`,
`VirtualDisplayDriver-x86.Driver.Only.zip` (25.7.23) = `E24210692B442B39AF763536330CE78B423F19342B7A7792C26DE3944E418B3A`.

**`package/.gitattributes` marks everything binary on purpose.** The repo-wide `* text=auto eol=lf`
would rewrite `MttVDD.inf` and invalidate the catalog hash. Never touch these files.

## Signature status (verified, see ADR 0004 for the command output)

* `mttvdd.cat` and `MttVDD.dll`: Authenticode **Valid**, signer `CN=SignPath Foundation` (thumbprint
  `3CF8CF26D8BA266C3A483AB7D26D4A818E317D76`, valid until 2027-09-07), issuer `GlobalSign GCC R45
  CodeSigning CA 2020`, RFC 3161 timestamp by DigiCert. `signtool verify /pa /c mttvdd.cat MttVDD.dll` succeeds.
* **Not** signed by Microsoft (no WHQL/attestation). `signtool verify /kp ...` (kernel-mode policy) fails.
  That is acceptable only because this is a **user-mode (UMDF) driver**: the kernel-mode signing policy
  does not apply to it; the PnP catalog-signature rule does, and a third-party release certificate
  satisfies it on x64 (Microsoft Learn, "PnP Device Installation Signing Requirements").
* Consequence: on a clean machine Windows shows "Would you like to install this device software?
  (SignPath Foundation)" unless the publisher certificate is already in
  `Cert:\LocalMachine\TrustedPublisher`. `deskbridge-setup.exe` must therefore add the **leaf**
  certificate (pin the thumbprint above) to TrustedPublisher before installing. Not verified on a
  clean machine (only upstream issue #315 and the upstream `silent-install.ps1` as evidence).
* ARM64: not usable (Windows requires WHQL there, upstream issue #197/#465). x64 only.

## Install recipe for `deskbridge-setup` (to be validated on a clean Win 11 VM with Secure Boot on)

1. Extract the leaf certificate from `mttvdd.cat` (PKCS#7, `SignedCms`/`X509Certificate2Collection.Import`),
   require `Thumbprint == 3CF8CF26D8BA266C3A483AB7D26D4A818E317D76` (update the pin when a newer
   driver release is bundled), import **only that certificate** into `LocalMachine\TrustedPublisher`.
   Do not import the GlobalSign root/intermediate (upstream's community script does; unnecessary).
2. `DiInstallDriver(path\to\MttVDD.inf, 0, ...)` (puts the package into the Driver Store).
3. `SetupDiCreateDeviceInfo` (class GUID `{4D36E968-E325-11CE-BFC1-08002BE10318}`, class `Display`,
   `DICD_GENERATE_ID`), `SPDRP_HARDWAREID = "Root\MttVDD\0\0"`, `SetupDiCallClassInstaller(DIF_REGISTERDEVICE)`,
   then `UpdateDriverForPlugAndPlayDevices(hwid "Root\MttVDD", inf)`. This is exactly what
   `devcon install MttVDD.inf Root\MttVDD` / `nefconc --create-device-node ... --hardware-id Root\MttVDD` do.
4. Create `C:\VirtualDisplayDriver\` and write `vdd_settings.xml` (from `config/vdd_settings.deskbridge.xml`)
   **before** step 3, so that the first start already has the right modes. (Alternative location:
   `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH` = folder. The driver runs as a UMDF host
   (LocalService); the folder must be readable by it, and writable if you want to use `SETGPU` /
   `SETDISPLAYCOUNT`, which rewrite the XML.)
5. Uninstall: remove the device node (`DIF_REMOVE`), `pnputil /delete-driver oemXX.inf /uninstall` equivalent
   (`DiUninstallDriver`), delete `C:\VirtualDisplayDriver`, and remove the TrustedPublisher certificate only if we added it.
6. Never leave the device enabled unattended across GPU-driver updates (upstream warning: display priority
   can get scrambled, black screen). `crates/display` must put the device in the disabled state when focus mode is off.

## Runtime control for `crates/display`

### Modes and GPU – `vdd_settings.xml` (flat schema of the shipped x64 binary)

See `config/vdd_settings.deskbridge.xml` (7 modes, empty `<global>`, GPU `default`). Facts from the
driver source (`Driver.cpp`, `AdapterOption.h`) and the shipped binary's string table:

* Each `<resolution>` = one mode `width x height @ refresh_rate`; fractional rates allowed (float).
* `<global><g_refresh_rate>` is **multiplied with every resolution**; keep it empty.
* Keep the list short (upstream #254: init fails at ~108 mode combinations; #82: ~140).
* GPU: `<gpu><friendlyname>` = DXGI adapter description, e.g. `NVIDIA GeForce RTX 5060 Laptop GPU`.
  Unknown name or `default` -> adapter with the most dedicated VRAM (the dGPU on Optimus). The driver then calls
  `IddCxAdapterSetRenderAdapter` with that adapter's LUID. (Selection by `"name,pcibus"` / LUID exists only in
  unreleased master; the shipped x64 DLL has no such string.)
* 5120x1440@120 = 884 Mpx/s "pixel rate": the driver sets `pixelRate = w*h*rate` and `totalSize = activeSize` (no
  blanking), there is no cable bandwidth limit. No code path rejects it. **Not tested on hardware.**

### Applying changes at runtime – named pipe `\\.\pipe\MTTVirtualDisplayPipe`

Message-mode duplex pipe, one command per connection, UTF-16LE text (no terminator needed). Pipe ACL is
`D:(A;;GA;;;WD)` (everyone: full access; any local process can send commands, see ADR 0004 risks).
Commands present in the shipped x64 DLL (string table checked): `RELOAD_DRIVER`, `SETGPU "<adapter name>"`
(quotes required), `SETDISPLAYCOUNT <n>`, `GETSETTINGS`, `GETALLGPUS`, `GETASSIGNEDGPU`, `D3DDEVICEGPU`, `IDDCXVERSION`,
`PING` (reply `PONG` on the log channel), `LOG_DEBUG true|false`, `LOGGING true|false`, `HDRPLUS`, `SDR10`,
`CUSTOMEDID`, `PREVENTSPOOF`, `CEAOVERRIDE`, `HARDWARECURSOR` (+ true/false).

Minimal Rust example (windows-sys not needed, std is enough):

```rust
use std::{fs::OpenOptions, io::Write};

fn vdd_command(cmd: &str) -> std::io::Result<()> {
    let mut pipe = OpenOptions::new().write(true).open(r"\\.\pipe\MTTVirtualDisplayPipe")?;
    let bytes: Vec<u8> = cmd.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    pipe.write_all(&bytes) // driver reads one message, executes it, closes the pipe
}

// vdd_command("SETGPU \"NVIDIA GeForce RTX 5060 Laptop GPU\"")?;  // rewrites <friendlyname>, reloads
// vdd_command("RELOAD_DRIVER")?;                                   // re-read vdd_settings.xml
```

### Hot-plug (monitor on/off without reinstall)

The released driver creates its `<count>` monitors **when the adapter starts** (`IndirectDeviceContext::FinishInit`
-> `IddCxMonitorArrival`); there is no "add/remove one monitor" command (unlike SudoVDA, see ADR). Recommended
mechanism for plan 8.8 ("when focus mode is off there is no virtual monitor"): keep the device installed with
`<count>1`, and **enable / disable the device node** (`SetupDiSetClassInstallParams` with `DIF_PROPERTYCHANGE`,
`DICS_ENABLE` / `DICS_DISABLE`, `DICS_FLAG_GLOBAL`). Upstream ships a community script doing the same with
`Disable-PnpDevice`/`Enable-PnpDevice` (`Community Scripts/toggle-VDD.ps1`). Timing (expect 1-3 s) and
cleanliness of the topology change are **unmeasured**. Mode switch while running: rewrite XML + `RELOAD_DRIVER`
(or disable/enable); Windows' own `ChangeDisplaySettingsEx` / CCD `SetDisplayConfig` switches between the
already published modes without touching the driver.

## Known issues to keep in mind (all from the upstream tracker, all open)

* `DXGI_ERROR_ACCESS_LOST` on Desktop Duplication of a VDD output that does not recover after a resolution
  change (issue #399, Sunshine in a VM). Mitigation to plan in `crates/capture`: recreate device + duplication,
  fall back to Windows.Graphics.Capture.
* Frame-rate drops at high refresh in exclusive-fullscreen games (issue #268, VDD and SudoVDA alike).
* Instability with 4 monitors on Windows 11 build 26200 (issue #490; fix for the multi-monitor swap-chain
  threading, PR #482, is in master but **not in any signed release**). We use 1 monitor.
* Driver or GPU-driver updates can leave the virtual monitor as primary -> black screen. Safe-mode recovery is
  documented upstream; our topology backup/restore (plan 8.7/16) is mandatory.
* Last signed release 2025-07-23, x64 binary built 2024-12-24; master has newer fixes (HDR bit depth, PCI-bus GPU
  selection) that we cannot sign ourselves (UMDF driver would need a code-signing cert + trust step).
