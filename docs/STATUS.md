# Desk Bridge – Umsetzungsstand gegen den Plan (2026-10-16)

Dieses Dokument ordnet jeden Meilenstein aus [PLAN.md](PLAN.md) ein: was im Code fertig und
automatisch getestet ist, was **nur auf echter Hardware** abgenommen werden kann (Checklisten in
[testing.md](testing.md)) und welche Entscheidungen noch bei Thorben liegen.

Legende: **Code** = implementiert und mit Mocks/Loopback getestet · **HW** = Abnahme auf echter
Hardware steht aus · **Entscheidung** = braucht dein OK.

## Automatisierte Prüfung (Stand dieser Auslieferung)

* `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
  (über 500 Tests, davon 14 Integrationstests mit zwei Engines auf `127.0.0.1`), `cargo deny check`
  (Lizenzen, Advisories mit dokumentierten Ausnahmen) laufen sauber.
* NSIS-Installer wird gebaut (`DeskBridge_0.1.7_x64-setup.exe`, ca. 245 MiB inkl. WebView2-Offline-
  Installer und FFmpeg-DLLs). **Er wurde auf diesem PC bewusst nicht ausgeführt** (er legt Firewall-
  Regel, Aufgabe, Dienst und eine Richtlinie an).
* Echte Windows-Pfade, die auf dem Entwicklungs-PC gelaufen sind: Hooks/Injektion (Selbsttest),
  Bildschirme, DPAPI, Zwischenablage inkl. virtueller Dateien über die Shell, WASAPI Loopback/Mikro/
  Wiedergabe, Spike 5120×1440@120 (Capture→NVENC→D3D11VA→Present, synthetisch), Agent-Start mit
  Named Pipe und `--stop`.

## Meilensteine

| M | Inhalt laut Plan | Code | Noch offen (HW / Entscheidung) |
|---|---|---|---|
| M0 | Workspace, Tauri-UI, Branding, App-Icon (3 Entwürfe), CI, NSIS-Installer, Skripte, Docs | fertig | **Entscheidung:** Icon-Variante A/B/C (`app/src/brand/app-icon/`, Standard A). HW: Installer auf frischem Windows 10 und auf dem Laptop. Updater-Schlüssel erzeugen (`npm run tauri signer generate`), `tauri-plugin-updater` wieder aktivieren, sobald `zip` auf crates.io wieder aufgelöst werden kann |
| M1 | Identität, mDNS, Pairing (SPAKE2), QUIC mTLS, Reconnect, IPC, Firewall | fertig | HW: Sleep/Wake < 5 s, 24 h, Netzprofil „Öffentlich“/Domäne (dieser PC ist eine Domäne: `install --include-domain`) |
| M2 | Maus/Tastatur, Layout, Kantenwechsel, Hotkeys, Key-Sicherheit, Text-Clipboard | fertig | HW: Woche Alltag, DE-Sonderzeichen/AltGr, Latenz p95 < 3 ms (`latency-probe input`) |
| M3a | Laptop-Ton → Headset | fertig | HW: Loopback bei stummem Laptop (ADR 0002), Dongle „Game/Chat“, Hot-Plug |
| M3b | Virtuelles Mikro | Backend für ein signiertes Virtual-Cable fertig, ohne Treiber ehrlich „nicht verfügbar“ | **Entscheidung (ADR 0003):** Kein Treiber erfüllt Plan-Kriterien 1–3 wörtlich. Vorschlag: VB-CABLE Pack45 (Microsoft-signiert) – Lizenz verlangt Zustimmung des Autors für die Einbettung. Alternativ F3 zurückstellen |
| M4 | Zwischenablage komplett, Dateien | fertig | HW: Explorer-Kopierdialog bei virtuellen Dateien, 2-GB-Datei ≥ 80 % Link-Speed |
| M5 | Fokusmodus (Spike, Streaming, virtuelles Display, Split) | fertig (Pipeline mit synthetischem Backend getestet, echte Kette im Spike) | **Entschieden (ADR 0004):** Display-Treiber (SignPath-signiert) wird mitgeliefert; der Installer importiert genau dieses Zertifikat (Fingerprint fest verdrahtet) in „Vertrauenswürdige Herausgeber“ und entfernt es bei der Deinstallation wieder (`drivers/display/driver.json`: `enabled: true`). HW: 5120×1440@120 mit virtuellem Display auf der RTX 5060, Optimus, Split/AppBar, 20× ein/aus, Agent-Kill |
| M6 | Dienst, Secure-Desktop-Helfer, SendSAS, Watchdog, Diagnose, Update-Angebot | fertig | HW: UAC/Sperrbildschirm/Strg+Alt+Ende (Installer setzt `SoftwareSASGeneration=1`, nur falls nicht gesetzt, `--no-sas-policy` zum Verzicht), Agent-Kill bei gedrückter Taste, 2-Wochen-Dauerlauf |
| M7 | Release-Vorbereitung | Docs, README mit Screenshots, Lizenzbericht (`THIRD_PARTY_LICENSES.html`), Projektseite (`docs/site/`), EN/DE-UI | Code-Signing (SignPath-Antrag), echte Release-Notes, Barrierefreiheits-Prüfung mit Screenreader |

## Bewusste Abweichungen vom Plan (alle begründet in ADRs/Code-Kommentaren)

* **FFmpeg 8.1 statt 7.1** (der 7.1-LGPL-Build hat kein `av1_nvenc`), LGPL, als DLLs; nur vier DLLs
  werden ausgeliefert (ADR 0005).
* **4:4:4 wird nicht angeboten** (D3D11-Frames und D3D11VA kennen kein 4:4:4); die Option bleibt im
  Protokoll. **H.264 ist bei 5120 px Breite nicht möglich** (NVENC-Limit 4096).
* **AV1-Standard** ist offen: Die RTX 3070 Ti des PCs kann kein AV1 encodieren; der Lauf auf der
  RTX 5060 entscheidet (`video-spike --codec av1`).
* Eigener **Kaiser-Sinc-Resampler** statt `rubato` (klickfreie Live-Ratenänderung, getestet).
* Eigener **ZIP-Schreiber** für das Diagnosepaket, weil alle `zip`-Crate-Versionen yanked sind.
* **Rust ab 1.90** (MSRV angehoben, damit `time` ≥ 0.3.47 mit dem Sicherheitsfix aufgelöst wird).
* `deskbridge-setup install` registriert den **Dienst standardmäßig** (Plan: ab M6) und setzt
  `SoftwareSASGeneration` (nur wenn nicht konfiguriert, wird bei der Deinstallation zurückgenommen).

## Was ich nicht verifizieren konnte (ehrlich)

Alles, was zwei echte Rechner, den Laptop (RTX 5060 / Optimus / Deckel), das Funk-Headset, ein frisches
Windows 10, UAC/Sperrbildschirm oder Secure Boot braucht. Die konkreten Prüfschritte stehen in
`docs/testing.md` und in den Berichten der ADRs 0002–0006.
