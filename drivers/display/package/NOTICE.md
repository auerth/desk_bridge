# Notice – Virtual Display Driver (IddCx), unmodified upstream binaries

The three files `MttVDD.dll`, `MttVDD.inf`, `mttvdd.cat` in this directory are the
**unmodified** x64 release binaries of the "Virtual Display Driver" project
(https://github.com/VirtualDrivers/Virtual-Display-Driver, release 24.12.24 asset
`Signed-Driver-v24.12.24-x64.zip`; identical files are in release 25.7.23, asset
`VirtualDisplayDriver-x86.Driver.Only.zip`, which despite its name contains the x64 build).

* Licence of the project: MIT, see `LICENSE` in this directory (copied verbatim from the
  upstream repository, `Copyright (c) 2024 Virtual Display`).
* The driver code derives from Microsoft's Indirect Display Driver sample
  (https://github.com/microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay,
  Microsoft Public License, MS-PL) and uses the Windows Driver Frameworks headers (MIT).
  Microsoft, Windows and IddCx are trademarks of Microsoft Corporation.
* Code signing of these binaries: SignPath Foundation (https://signpath.org), certificate by
  SignPath Foundation, issued by GlobalSign. This is a publisher signature, **not** a Microsoft
  (WHQL/attestation) signature, see `../README.md` and `docs/adr/0004-virtual-display-driver.md`.
* Do not edit, re-save, re-zip with line-ending conversion or otherwise change these files:
  any change invalidates the catalog signature.
