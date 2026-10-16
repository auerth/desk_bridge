# Security Policy

## Reporting a vulnerability

Please report security issues **privately** to **security@softwelop.com** (or use GitHub's "Report a
vulnerability" button under the repository's *Security* tab). Do not open a public issue.

Include, if you can: affected version, Windows version, a description of the impact, steps or a
proof of concept, and whether the issue is already public. You can encrypt the report on request.

What to expect (best effort, small team):

* acknowledgement within **3 working days**,
* a first assessment within **10 working days**,
* a fix or mitigation plan communicated before public disclosure; we aim to release a fix within
  **90 days** and will credit you in the release notes unless you prefer otherwise.

Please give us reasonable time to fix the issue before disclosing it. We will not take legal action
against good-faith research that respects user privacy and does not disrupt other people's systems.

## Supported versions

Desk Bridge has not had a stable release yet. Until 1.0 only the **latest release** (and `main`)
receives security fixes. After 1.0 the latest minor release is supported. Both paired devices should
run the same version; the app warns about mismatches.

## Threat model (summary)

Desk Bridge transmits keystrokes, clipboard contents, files, audio and screen contents between two
computers, and its agent runs elevated, so it is security-sensitive by nature.

What we protect against

* **Eavesdropping and tampering on the LAN:** every connection is QUIC/TLS 1.3 with mutual
  authentication and **pinned self-signed certificates**. There is no unencrypted mode.
* **Unknown devices:** a device that is not in `peers.toml` is rejected right after the handshake,
  before any application data is processed.
* **Man in the middle during pairing:** pairing uses SPAKE2 with a six-digit code, valid for 2
  minutes, three failures lock the code for 60 seconds, and the confirmation MACs are bound to both
  TLS certificate fingerprints.
* **Local privilege abuse:** the UI-to-agent named pipe is restricted by ACL to the current user and
  the agent exposes no network API apart from the peer protocol. The firewall rule is limited to the
  **Private** profile, the agent program and the local subnet.
* **Privacy:** no cloud, no account, no telemetry. The microphone is transmitted only while an
  application on the other device actually has it open, with a visible indicator. Clipboard
  contents flagged as sensitive (password managers) are not synchronised.
* **Supply chain:** `cargo deny` (licences, sources) and `cargo audit` (RustSec) run in CI;
  dependencies are updated by Dependabot; updates are minisign-signed; FFmpeg is a pinned LGPL
  build verified in CI.
* **Stuck input:** leaving the remote state always releases every injected key and button on the
  target (a reliability issue that is also treated as a security-relevant failure).

Out of scope / known limits

* Anyone who is already logged in on a paired device with the same privileges can use Desk Bridge
  as intended; the pairing is the trust decision. Pair only devices you control.
* Physical access, malware already running on either device, and compromise of the user account.
* Games with anti-cheat may block injected input; that is not a vulnerability.
* Until the open-source release, binaries are not code-signed (SignPath Foundation planned);
  verify the installer's SHA-256 published with each release. Kernel drivers shipped with Desk Bridge
  are third-party and keep their own signatures, see `drivers/*/README.md`.
* Denial of service by a device on the same LAN flooding UDP 47820 is not mitigated beyond QUIC's
  own protections.

## Hardening tips

* Keep the network profile of both devices on **Private** and do not expose UDP 47820 to the
  internet.
* Unpair devices you no longer use (UI, on both sides).
* Keep both devices on the latest version.
