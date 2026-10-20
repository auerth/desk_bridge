# Desk Bridge – Software-Plan

> **Produktname:** Desk Bridge · **Codename im Code:** `deskbridge` · **Marke:** softwelop
> **Stand:** 2026-10-16 · **Status:** Plan v1.1, Grundlage für den Entwicklungs-Thread
> **Plattform v1:** Windows 10 22H2+ / Windows 11 ↔ Windows. macOS und Linux kommen später, die Architektur ist von Anfang an darauf ausgelegt.

### Ziel-Setup (Thorben)
| | PC (Steuergerät) | Laptop (Zielgerät) |
|---|---|---|
| Netz | LAN-Kabel | LAN-Kabel |
| GPU | NVIDIA RTX 30/40/50 → NVDEC mit HEVC (inkl. 4:4:4) und AV1 | NVIDIA GeForce RTX 5060 Laptop (vermutlich, Hybrid/Optimus mit iGPU) → NVENC mit H.264/HEVC/AV1 |
| Monitor | **Super-Ultrawide 5120×1440 @ 120 Hz** (32:9) | internes Panel |
| Audio | Funk-Headset mit USB-Dongle | – |
| Rechte | Admin | Admin, privates Gerät |

---

## 0. Hinweise für den Entwicklungs-Thread

- Dieses Dokument ist die **Quelle der Wahrheit**. Wer davon abweicht, legt einen ADR an (`docs/adr/NNNN-titel.md`) und passt den Plan an.
- **Repo:** neues Git-Repo unter `C:\Projekte\desk-bridge` (nicht im Thinking-Workspace). Vorerst privat auf GitHub, später Open Source. *(Umgesetzt unter `C:\Projekte\desk_bridge`.)*
- **Arbeitsweise:** Meilensteine (Abschnitt 15) strikt nacheinander. Pro Meilenstein ein Branch und ein PR; eine eigene `CHANGELOG.md` mitführen. Erst die Abnahmekriterien erfüllen, dann weiter.
- **Zwei Geräte:** Die Entwicklung läuft auf dem PC. Für Tests auf dem Laptop gibt es ab M0 das Skript `scripts/deploy-laptop.ps1`, das den Build per SMB-Freigabe oder `robocopy` rüberkopiert. **Alles, was echte Hardware braucht, testet Thorben** (Checklisten in Abschnitt 14). Automatisierte Tests laufen mit Mock-Backends und zwei Instanzen auf `127.0.0.1`.
- **Produktname** „Desk Bridge“ steht trotzdem nur an einer Stelle (`crates/core/src/brand.rs` + `app/src/brand.ts`).
- **Oberstes Prinzip: Ein Installer, null Handarbeit.** Der Nutzer installiert **nur** Desk Bridge. Alles, was gebraucht wird (Treiber, FFmpeg, Laufzeiten, Firewall-Regeln, Autostart), bringt die Software selbst mit und richtet es selbst ein. Kein „bitte vorher X installieren“, auch nicht übergangsweise für Thorben (siehe Abschnitt 12.1). Jede neue Abhängigkeit muss sich daran messen lassen.
- Sprache: Code, Kommentare und Commits auf Englisch, UI-Texte auf Deutsch (i18n-fähig, Englisch folgt in M7).

---

## 1. Vision und Ziele

**Ein Schreibtisch, zwei Rechner, null Umschalten.** Man installiert Desk Bridge auf beiden Geräten, koppelt sie einmal mit einem Code, und danach gilt:

| # | Fähigkeit | Beschreibung |
|---|---|---|
| F1 | **Maus und Tastatur teilen** | Maus über die Bildschirmkante schieben, schon steuert man den Laptop. Der Laptop verhält sich wie ein weiterer Monitor. |
| F2 | **Laptop-Ton im Headset** | Alles, was der Laptop abspielt, kommt im Headset am PC an, gemischt mit dem PC-Ton. |
| F3 | **Mikro für den Laptop** | Das Headset-Mikro am PC erscheint auf dem Laptop als Mikrofon „Desk Bridge Mikrofon“. Teams, Zoom und Discord können es normal nutzen. **Hängt davon ab, ob es einen fertigen signierten Treiber gibt** (8.9). |
| F4 | **Gemeinsame Zwischenablage** | Text, formatierter Text, Bilder und Dateien: auf dem einen Gerät kopieren, auf dem anderen einfügen. |
| F5 | **Dateien senden** | Drop-Zone, Explorer-Kontextmenü „Senden an“, Kopieren und Einfügen von Dateien im Explorer. |
| F6 | **Fokusmodus** | Per Hotkey erscheint der Laptop im Vollbild auf dem PC-Monitor, in nativer Auflösung **5120×1440 @ 120 Hz** über ein virtuelles Display. Variante **Split**: Der Laptop belegt eine Hälfte des 32:9-Monitors (2560×1440), der PC die andere. Der Laptop kann zugeklappt sein. |
| F7 | **Schöne, einfache UI** | Im softwelop-Branding: Installieren, Koppeln, Loslegen. Tray-Icon, OSD beim Wechsel, Statusanzeigen. |

### Nicht-Ziele für v1 (bewusst später)
- Mehr als zwei Geräte (die Architektur erlaubt es, die UI nicht)
- Verbindung über das Internet bzw. außerhalb des LANs
- Laptop-Touchpad steuert PC (umgekehrte Richtung; Architektur ist symmetrisch, Feature folgt später)
- Drag & Drop von Dateien über die Bildschirmkante (v1.x)
- macOS und Linux (M8+)
- Spiele mit Anti-Cheat (injizierter Input wird dort oft ignoriert; das ist kein Bug von uns)

---

## 2. Nutzungsszenarien

1. **Morgens:** PC und Laptop starten, Desk Bridge verbindet sich automatisch (Autostart). Das Tray-Icon wird grün.
2. **Nebenbei arbeiten:** Am PC arbeiten, Maus nach rechts aus dem Monitor schieben, der Cursor erscheint auf dem Laptop-Display. Einen Teams-Chat beantworten, Maus zurück.
3. **Meeting:** Teams auf dem Laptop startet einen Call. Ton kommt im Headset, das Mikro am PC wird übertragen. Im PC-Tray leuchtet der Hinweis „Laptop nutzt dein Mikro“.
4. **Längere Laptop-Session:** `Strg+Alt+Shift+F` drücken. Der Ultrawide zeigt den Laptop-Desktop in 5120×1440 @ 120 Hz, die Laptop-Fenster liegen jetzt auf dem großen Monitor. Nochmal drücken, und der PC-Desktop ist zurück.
5. **Datei rüber:** Im Explorer auf dem PC `Strg+C` drücken, dann auf dem Laptop im Explorer `Strg+V`. Ein Fortschrittsdialog erscheint wie bei einer normalen Kopie.
6. **Screenshot:** `Win+Shift+S` auf dem Laptop, dann `Strg+V` in ein Programm auf dem PC.

---

## 3. Qualitätsziele (messbar)

| Bereich | Ziel (LAN-Kabel) | Ziel (WLAN 5 GHz) | Messung |
|---|---|---|---|
| Zusätzliche Input-Latenz | < 3 ms p95 | < 15 ms p95 | Zeitstempel im Event; Uhren über Ping/Pong-Offset abgeglichen |
| Audio Ende-zu-Ende (Laptop → Headset), **ohne** Eigenlatenz des Funk-Headsets | < 30 ms | < 60 ms | Klick-Test, Abschnitt 14 |
| Mikro Ende-zu-Ende (Headset → Laptop-App), ohne Headset-Eigenlatenz | < 30 ms | < 60 ms | Klick-Test |
| Fokusmodus Glass-to-Glass (ohne Monitor-Eigenlatenz) | < 30 ms p95 bei **5120×1440@120** | < 60 ms bei reduzierter FPS | Zeitstempel-Overlay im Testmodus |
| Fokusmodus Bildrate | stabil 120 fps bei Bewegung | ≥ 60 fps | Stats-Overlay |
| Fokusmodus Ein/Aus | < 2 s | < 2 s | Stoppuhr im Log |
| Reconnect nach Sleep/Wake | < 5 s nach Netzwerk-Verfügbarkeit | – | Log |
| CPU im Leerlauf (verbunden, nichts läuft) | < 1 % | – | Task-Manager |
| CPU Audio aktiv | < 3 % | – | – |
| RAM (Agent + UI) | < 150 MB | – | – |
| Stabilität | 8 h Dauerbetrieb ohne Hänger, Drift oder Stuck-Keys | – | Soak-Test |

---

## 4. Systemüberblick

```
┌──────────────────────── PC (Steuergerät) ────────────────────────┐        ┌────────────────────── Laptop (Zielgerät) ───────────────────────┐
│                                                                  │        │                                                                 │
│  Maus/Tastatur ──► Input-Capture (LL-Hooks + Raw Input) ─┐       │        │       ┌─► Input-Injector (SendInput)                            │
│                                                          │       │        │       │                                                         │
│  Headset-Mic ───► WASAPI Capture ──► Opus ──┐            │       │  QUIC  │       │   ┌─► Ring-Buffer ─► „Desk Bridge Mikrofon“ ─► Teams  │
│                                             │            ▼       │  (TLS  │       │   │     (virtueller Audio-Treiber)                      │
│  Headset ◄── WASAPI Render ◄── Jitter-Buf ◄─┼──── Session-Core ◄─┼─ 1.3) ─┼─► Session-Core ─► Opus-Dec                                    │
│                                             │      (Rust)        │  UDP   │    (Rust)    ▲                                                  │
│  Zwischenablage ◄──────────────────────────►│                    │ 47820  │              ├── Opus ◄── Loopback „Desk Bridge Lautsprecher“  │
│                                             │                    │        │              │                                                  │
│  Ultrawide ◄── Renderer (D3D11) ◄── Decoder ◄── Video-Frames ◄───┼────────┼──────────────┴── HW-Encoder ◄── DXGI-Capture ◄── Virt. Display │
│                                                                  │        │                                          (IddCx, 5120×1440)     │
│  UI (Tauri) ◄─IPC─► Agent                                        │        │  UI (Tauri) ◄─IPC─► Agent                                       │
└──────────────────────────────────────────────────────────────────┘        └─────────────────────────────────────────────────────────────────┘
```

**Wichtig: Beide Geräte installieren dieselbe Software.** Welche Rolle ein Gerät hat, ergibt sich pro Funktion:
- **Input-Quelle** = das Gerät, an dem Maus und Tastatur physisch hängen (v1: PC)
- **Audio-Senke** = das Gerät mit dem Headset (v1: PC)
- **Stream-Quelle** = das Gerät, das im Fokusmodus gezeigt wird (v1: Laptop)

---

## 5. Tech-Stack und Begründung

| Bereich | Wahl | Warum | Alternativen (verworfen) |
|---|---|---|---|
| Kern-Sprache | **Rust** (stable, Edition 2024) | Niedrige Latenz, kein GC-Ruckeln, sicher, gute Windows-Bindings (`windows-rs`), später auch auf macOS/Linux | C#/.NET (ist im Kern Windows-lastig, GC), C++ (unsicherer, langsamer zu entwickeln) |
| UI | **Tauri 2** + **React 19 + TypeScript + Vite** | Moderne, schöne UI mit Web-Tech und Brand-CSS, kleine Binaries, plattformübergreifend, Rust-Backend direkt angebunden | Electron (groß, RAM), WinUI 3 (nur Windows), egui/Slint (weniger „sexy“ und weniger flexibel im Branding) |
| UI-Bausteine | Radix UI Primitives, eigene Komponenten, Zustand (State), Motion (Animationen), Lucide als Ergänzung zu den Brand-Icons | Barrierearm, gut stylebar | MUI/Ant (eigene Optik, schwer zu branden) |
| Async | **tokio** | Standard | – |
| Transport | **QUIC** über `quinn` + `rustls` | Verschlüsselung (TLS 1.3) eingebaut, mehrere Streams ohne Head-of-Line-Blocking, unzuverlässige Datagramme für Audio, nur ein UDP-Port | TCP + eigenes TLS (HoL-Blocking), WebRTC (zu viel Overhead, schwer zu tunen) |
| Discovery | **mDNS/DNS-SD** über `mdns-sd`, Fallback manuelle IP | Automatisch finden im LAN | UDP-Broadcast (unschöner) |
| Pairing | **SPAKE2** (`spake2`) + gepinnte Zertifikate (`rcgen`) | Sicheres Koppeln mit 6-stelligem Code, kein MITM | Nur Fingerprint-Vergleich (umständlicher) |
| Steuer-Protokoll | **Protobuf** über `prost` | Schema-Evolution: PC und Laptop können unterschiedliche Versionen haben | postcard/bincode (keine Feld-Tags → bricht bei Versionsmischung) |
| Medien-Header | Eigenes kompaktes Binärformat | Minimaler Overhead pro Audio- und Videopaket | – |
| Windows-APIs | `windows` (windows-rs) | Offizielle Microsoft-Bindings | `winapi` (veraltet) |
| Audio-I/O | `wasapi`-Crate bzw. direkt `windows-rs` | Event-gesteuert, Loopback, geringe Latenz, volle Kontrolle | `cpal` (zu abstrakt für Loopback und Latenz-Tuning) |
| Audio-Codec | **Opus** (`audiopus`/`opus`), Option: rohes PCM | 10-ms-Frames mit geringer Latenz, robust über WLAN | – |
| Resampling/Drift | `rubato` | Ausgleich der Clock-Drift zwischen beiden Soundkarten | – |
| Video | **FFmpeg** (LGPL-Build, als DLLs **mitgeliefert**, dynamisch gelinkt) über `ffmpeg-next`/`rsmpeg`; Encoder NVENC (primär, RTX 5060), QSV/AMF für andere Laptops; Decoder D3D11VA, Software-Decode als Fallback | Eine API für alle GPU-Hersteller; bewährt (Sunshine nutzt dasselbe Prinzip) | Media Foundation direkt (Low-Latency-Konfiguration fummelig), Vendor-SDKs einzeln (3× Aufwand), x264 (GPL), OpenH264 (Patent-Lizenz nur für Ciscos Binaries) |
| Capture | DXGI Desktop Duplication, Fallback Windows.Graphics.Capture | Geringste Latenz, liefert Dirty-Rects und Cursor-Info getrennt | – |
| Rendering (PC) | Eigenes natives Fenster (`winit`) + D3D11-Swapchain (Flip-Model, waitable) | Die WebView ist für Video zu langsam | Video in Tauri-WebView (zu langsam) |
| Virtuelles Display | **Virtual Display Driver** (Open Source, MIT, IddCx, signierte Releases) mitliefern | Beliebige Auflösungen inkl. 5120×1440@120, signiert, aktiv gepflegt | Eigener IddCx-Treiber (später optional) |
| Virtuelles Audio | **„Desk Bridge Audio“**: virtueller Lautsprecher + Mikrofon mit Datenpfad zur eigenen App, **mitgeliefert**. Ein **fertiger, bereits signierter, weitergebbarer** OSS-Treiber (Kriterien in 8.9). Ein eigener SysVAD-Fork ist verschoben, weil die Signierung ein kostenpflichtiges EV-Zertifikat bräuchte | Gibt es sonst nicht als Teil einer Installation | Nur Loopback (Laptop-Lautsprecher würden mitspielen), VB-Cable (Fremdinstallation, Lizenz) |
| Installer | Tauri-Bundler → **NSIS** mit Custom-Hooks + eigenem Helfer `deskbridge-setup.exe` (Treiber über SetupAPI, Firewall, Task) | In Tauri integriert; keine Fremd-Tools wie devcon/nefcon nötig | WiX (mehr Aufwand) |
| Laufzeiten | Rust mit **statischer CRT** (`+crt-static`), WebView2 im Installer **eingebettet** (`offlineInstaller`), eventuell nötige MSVC-Runtime-DLLs app-lokal | Kein „bitte VC++ Redistributable installieren“ | – |
| Schrift | **Nur Montserrat** (SIL OFL, darf mitgeliefert werden): Bold/SemiBold für Überschriften, Regular/Medium für Labels, Light für Fließtext | Gilmer wird bewusst nicht verwendet (Lizenz) | Gilmer |
| Updates | `tauri-plugin-updater` + GitHub Releases | Signierte Updates | – |
| Logging | `tracing` + `tracing-appender` (rollierende Dateien) | Standard, strukturiert | – |
| Config | TOML (`serde` + `toml`), Pfade über `directories` | Menschenlesbar | – |
| Fehler | `thiserror` (Libs), `anyhow` (Binaries) | Standard | – |
| CI | GitHub Actions `windows-latest`: fmt, clippy (`-D warnings`), Tests, Tauri-Build, Artefakt | – | – |

---

## 6. Architektur

### 6.1 Prozesse

| Prozess | Läuft als | Aufgabe | Ab |
|---|---|---|---|
| `deskbridge-agent.exe` | Benutzersitzung, **erhöht** (Autostart über Aufgabenplanung „mit höchsten Privilegien“) | Der eigentliche Kern: Netzwerk, Input, Audio, Zwischenablage, Capture/Encode, Decode/Render | M1 |
| `Desk Bridge.exe` (Tauri) | Benutzersitzung, normal | UI, Tray, OSD-Overlay; spricht über Named Pipe `\\.\pipe\deskbridge-ui` mit dem Agent | M0 |
| `deskbridge-service.exe` | Windows-Dienst (SYSTEM) | Startet und überwacht den Agent; startet den Helfer auf dem Secure Desktop (UAC-Abfrage, Sperrbildschirm, Anmeldung); `SendSAS` für Strg+Alt+Entf | M6 |

**Warum Agent und UI getrennt?** Die UI kann abstürzen oder geschlossen werden, ohne dass Maus und Ton wegfallen. Der Agent muss erhöht laufen (siehe 8.1, UIPI), die UI nicht.
**Vereinfachung für M0/M1:** Der Agent darf anfangs im Tauri-Prozess laufen (Feature-Flag `embedded-agent`), solange `core` als Library sauber getrennt ist. Spätestens in M2 wird er ein eigener Prozess.

### 6.2 Repo-Struktur

```
desk-bridge/
├─ Cargo.toml                    # Workspace
├─ crates/
│  ├─ proto/                     # .proto-Dateien + prost-Build, Medien-Header, Versionierung
│  ├─ net/                       # QUIC (quinn), Discovery (mDNS), Pairing (SPAKE2), Identität/Zertifikate
│  ├─ platform/                  # Traits für alle OS-Schnittstellen (siehe 6.3) + Mock-Implementierungen
│  ├─ platform-windows/          # Windows-Implementierungen (windows-rs)
│  ├─ input/                     # Plattformunabhängige Logik: Layout-Mathe, Kantenwechsel, Hotkeys, Key-State-Tracking
│  ├─ audio/                     # Opus, Jitter-Buffer, Drift-Ausgleich, Mixer, Pegelmessung
│  ├─ clipboard/                 # Format-Mapping, Delayed Rendering, Loop-Vermeidung
│  ├─ files/                     # Dateiübertragung, Chunking, Resume, Hashing (BLAKE3)
│  ├─ video/                     # Capture-Pipeline, FFmpeg-Encode/-Decode, Frame-Pacing, Bitrate-Steuerung
│  ├─ display/                   # Steuerung des virtuellen Displays, Topologie sichern/wiederherstellen
│  ├─ core/                      # Session-Orchestrierung, Zustandsmaschinen, Config, Peer-Verwaltung, IPC-Server
│  ├─ agent/                     # Binary: deskbridge-agent
│  └─ service/                   # Binary: deskbridge-service (M6)
├─ app/                          # Tauri-App
│  ├─ src-tauri/                 # Rust-Seite der UI (IPC-Client, Tray, Fenster)
│  └─ src/                       # React + TS
│     ├─ brand/                  # Tokens, Fonts, Logos, Icons (aus softwelop.com/brand)
│     ├─ components/
│     ├─ screens/
│     └─ i18n/
├─ drivers/
│  ├─ audio/                     # eingebundener, signierter OSS-Audiotreiber (Paket + Lizenz) – M3b
│  └─ display/                   # Virtual Display Driver (Submodule/Release-Download + Config)
├─ installer/                    # NSIS-Hooks, Firewall/Treiber/Aufgaben-Skripte
├─ scripts/                      # deploy-laptop.ps1, dev-two-instances.ps1, measure-*.ps1
├─ tools/
│  ├─ latency-probe/             # Mess-Tools (Input, Audio, Video)
│  └─ fake-peer/                 # Headless-Peer für Tests
├─ docs/
│  ├─ adr/                       # Architecture Decision Records
│  ├─ protocol.md
│  ├─ testing.md
│  └─ architecture.md
└─ .github/workflows/
```

### 6.3 Plattform-Abstraktion (der Schlüssel für Mac/Linux später)

Alle OS-Zugriffe laufen über Traits in `crates/platform`. Die Logik in `input`, `audio`, `clipboard`, `video` und `core` kennt **kein** Windows.

```rust
pub trait InputCapture   { fn start(&self, sink: InputSink) -> Result<()>; fn set_grab(&self, grab: bool); /* blockt lokale Eingabe */ }
pub trait InputInjector  { fn key(&self, ev: KeyEvent); fn mouse_rel(&self, dx: i32, dy: i32); fn mouse_abs(&self, x: f64, y: f64);
                           fn button(&self, b: Button, down: bool); fn wheel(&self, dx: i32, dy: i32); fn release_all(&self); }
pub trait ScreenInfo     { fn screens(&self) -> Vec<Screen>; fn cursor_pos(&self) -> Point; fn watch(&self, cb: ...); }
pub trait ClipboardBackend { fn watch(&self, cb: ...); fn read(&self, fmt: Format) -> Result<Bytes>; fn offer(&self, formats: Vec<Format>, provider: ...); }
pub trait AudioCapture   { /* Mikro oder Loopback, liefert f32-Frames + Timestamps */ }
pub trait AudioPlayback  { /* Render-Stream auf Gerät X */ }
pub trait VirtualAudio   { fn speaker_frames(&self) -> ...; fn write_mic(&self, frames: &[f32]); fn mic_in_use(&self) -> bool; }
pub trait ScreenCapturer { /* GPU-Texturen + Cursor-Metadaten */ }
pub trait VideoEncoder / VideoDecoder / VideoPresenter
pub trait DisplayManager { fn enable_virtual(&self, mode: Mode) -> Result<DisplayId>; fn save_topology(&self) -> Topology; fn restore(&self, t: Topology); }
pub trait PowerManager   { fn on_sleep_wake(&self, cb: ...); fn set_lid_action_do_nothing(&self, on: bool); }
```

Zu jedem Trait gibt es ein **Mock** (`platform::mock`), damit `core` komplett ohne Hardware testbar ist.

### 6.4 Zustandsmaschinen (in `core`)

**Verbindung:** `Idle → Discovering → Connecting → Handshaking → Connected → (Degraded) → Reconnecting → Connected …`
- Reconnect mit exponentiellem Backoff (0,5 s … 10 s) und sofortigem Retry bei Netzwerkwechsel oder Wake.
- Heartbeat alle 500 ms; nach 3 s ohne Antwort `Degraded`, nach 6 s `Reconnecting`.

**Input-Fokus:** `Local → Remote(peer, screen) → Local`, dazu `Locked` (Cursor ist auf dem aktuellen Gerät gesperrt).
- **Sicherheitsregel:** Bei jedem Verlassen von `Remote` (Kantenwechsel, Hotkey, Verbindungsabbruch, Timeout) ruft das **Zielgerät** `release_all()` auf, also Key-Up für alle von uns gedrückten Tasten und Buttons. Das Steuergerät hebt den Grab auf. Hängende Tasten darf es nie geben.

**Fokusmodus:** `Off → Preparing (virt. Display an, Topologie) → Streaming → Stopping (Topologie zurück) → Off`. Jeder Schritt hat einen Timeout und einen Rollback.

---

## 7. Netzwerk und Protokoll

### 7.1 Identität und Pairing
- Beim ersten Start erzeugt jedes Gerät eine **Device-ID** (UUIDv7) und ein Ed25519-Schlüsselpaar mit selbstsigniertem Zertifikat (`rcgen`). Der private Schlüssel liegt DPAPI-geschützt in `%APPDATA%\softwelop\DeskBridge\identity.bin`.
- **Ablauf beim Koppeln:**
  1. Gerät A: „Gerät koppeln“ zeigt einen 6-stelligen Code (gültig 2 min).
  2. Gerät B findet A über mDNS (oder per IP-Eingabe) und gibt den Code ein.
  3. Über eine temporäre QUIC-Verbindung (Zertifikat noch unverifiziert) läuft **SPAKE2** mit dem Code und erzeugt einen gemeinsamen Schlüssel.
  4. Mit diesem Schlüssel tauschen beide ihre Zertifikats-Fingerprints aus (MAC-geschützt).
  5. Beide speichern den Peer in `peers.toml` (ID, Name, Fingerprint, letzte IP).
- Danach immer **mTLS mit gepinnten Fingerprints**. Unbekannte Geräte werden abgewiesen.
- Entkoppeln geht in der UI auf beiden Seiten und löscht den Peer.

### 7.2 Discovery
- Service `_deskbridge._udp.local`, TXT-Records: `id`, `name`, `ver`, `proto`.
- Zusätzlich die zuletzt bekannte IP direkt anwählen (schneller Reconnect).
- Fallback: manuelle IP/Hostname in den Einstellungen.

### 7.3 Transport (eine QUIC-Verbindung, mehrere Kanäle)
- **Port:** UDP **47820** (konfigurierbar), ALPN `deskbridge/1`.
- Keep-Alive 1 s, Idle-Timeout 10 s. Congestion Control: Cubic (Standard); für Video BBR testen (ADR).
- Kein Nagle-Problem, weil QUIC; trotzdem Input-Events sofort flushen.

| Kanal | QUIC-Typ | Inhalt | Begründung |
|---|---|---|---|
| `control` | bidi-Stream #0 | Hello, Capabilities, Layout, Zustandswechsel, Ping/Pong, Fehler | zuverlässig, geordnet |
| `input` | eigener bidi-Stream | Key, Button, Move, Wheel, Enter/Leave, EdgeHit | **Zuverlässig und geordnet**: Ein verlorenes Key-Up hieße hängende Taste. Im LAN kostet das keine spürbare Latenz. |
| `clipboard` | uni-Streams pro Transfer | Offer, Request, Data | zuverlässig |
| `files` | uni-Stream pro Datei | Chunks à 256 KiB | parallel, mit Resume |
| `audio` | **Datagramme** | Opus-Frames (10 ms) | Verlust ist besser als Verzögerung |
| `video` | uni-Stream pro Frame (zunächst) | Encodierte Frames | zu spät → Frame verwerfen, IDR anfordern; Datagramme + FEC später per ADR prüfen |

### 7.4 Steuer-Nachrichten (Protobuf, Auszug)

```proto
syntax = "proto3";
package deskbridge.v1;

message Hello { string device_id = 1; string name = 2; string os = 3; string app_version = 4;
                uint32 proto_version = 5; repeated Capability caps = 6; repeated Screen screens = 7; }
message Screen { uint32 id = 1; int32 x = 2; int32 y = 3; uint32 w = 4; uint32 h = 5; uint32 dpi = 6;
                 uint32 refresh_mhz = 7; bool primary = 8; string name = 9; }
message LayoutUpdate { repeated Placement placements = 1; uint64 revision = 2; }  // wer liegt wo
message Ping { uint64 t0_us = 1; }  message Pong { uint64 t0_us = 1; uint64 t1_us = 2; }  // RTT + Clock-Offset

// input-Stream
message InputEvent { uint64 ts_us = 1; oneof ev {
  Key key = 2; MouseRel rel = 3; MouseAbs abs = 4; MouseButton btn = 5; Wheel wheel = 6;
  Enter enter = 7; Leave leave = 8; EdgeHit edge_hit = 9; ToggleState toggles = 10; } }
message Key { uint32 scancode = 1; bool extended = 2; bool down = 3; }
message Enter { uint32 screen_id = 1; float rel_pos = 2; Edge edge = 3; ToggleState toggles = 4; }
message ToggleState { bool caps = 1; bool num = 2; bool scroll = 3; }

// Fokusmodus
message FocusStart { uint32 width = 1; uint32 height = 2; uint32 fps = 3; Codec codec = 4; uint32 bitrate_kbps = 5; bool yuv444 = 6; }
message FocusReady { uint32 width = 1; uint32 height = 2; Codec codec = 3; }
message KeyframeRequest {}  message FocusStop {}  message StreamStats { ... }

// Audio-Steuerung
message AudioRoute { Stream stream = 1; bool enabled = 2; }   // LAPTOP_TO_PC, MIC_TO_LAPTOP
message MicInUse { bool in_use = 1; }                           // Laptop meldet: eine App hat das virtuelle Mikro geöffnet
```

**Medien-Header (binär, little endian):** `stream_id u8 | flags u8 | seq u16 | ts_us u64 | payload…` (Audio). Video zusätzlich `frame_id u32 | is_key u8 | width u16 | height u16`.

**Versionierung:** `proto_version` wird beim Hello geprüft. Bei gleicher Major-Version wird die Schnittmenge der `caps` genutzt; bei abweichender Major-Version zeigt die UI „Bitte beide Geräte aktualisieren“.

*(Umsetzung: `crates/proto/proto/deskbridge.proto`; Tasten werden als USB-HID-Usage-Codes übertragen, siehe Abschnitt 17.)*

### 7.5 Firewall
- Der Installer legt die Regel „Desk Bridge (UDP 47820 + mDNS)“ für die Profile **Privat** und **Domäne** an (COM `INetFwPolicy2` oder `netsh advfirewall`, Remote-Bereich `LocalSubnet`). Firmennetze sind als Domäne eingestuft; mit einer reinen Privat-Regel finden sich die Geräte nicht mehr, sobald Windows das Netz umklassifiziert. **Öffentlich** bleibt zu.
- Ist das Netzwerk als **Öffentlich** eingestuft, zeigt die UI einen Hinweis mit Erklärung und dem Button „Netzwerk als privat markieren“ (das ist eine Systemeinstellung, nur nach Bestätigung).

---

## 8. Subsysteme im Detail

### 8.1 Maus und Tastatur (F1)

**Steuergerät (PC):**
- Der Prozess ist **Per-Monitor-DPI-Aware v2** (Manifest), damit alle Koordinaten physische Pixel sind.
- Eigener Thread mit Message-Loop für `WH_MOUSE_LL` + `WH_KEYBOARD_LL` (`SetWindowsHookExW`). **Hook-Callbacks machen nichts außer Enqueue** (lock-free Channel), sonst entfernt Windows den Hook wegen `LowLevelHooksTimeout`.
- **Raw Input** (`RegisterRawInputDevices`, `RIDEV_INPUTSINK`) für rohe Maus-Deltas (`lLastX/lLastY`), hochauflösendes Scrollen und Zusatztasten.
- **Lokal:** Hooks lassen alles durch und prüfen nur auf Kanten und Hotkeys.
- **Remote:** Hooks blocken lokale Eingaben (Rückgabe ≠ 0), Raw-Input-Deltas gehen als `MouseRel` raus, Tasten als `Key{scancode, extended}`. Der PC-Cursor bleibt an der Kante stehen; optional wird er ausgeblendet.
- Hotkeys werden **im Hook** erkannt (nicht über `RegisterHotKey`), damit sie auch im Remote-Zustand greifen.
- Gesendet werden **Scancodes statt Virtual Keys**, damit das Tastaturlayout des Zielgeräts gilt (DE ↔ DE funktioniert 1:1).
- Win-Taste, Alt+Tab, Strg+Esc und Win+L werden im Remote-Zustand ans Ziel geschickt und lokal geblockt. **Ausnahme:** Strg+Alt+Entf lässt sich nicht abfangen. Ersatz ist `Strg+Alt+Ende` (wie bei RDP), das löst auf dem Ziel per Dienst `SendSAS` aus (M6).

**Zielgerät (Laptop):**
- `SendInput` mit `KEYEVENTF_SCANCODE` (+ `KEYEVENTF_EXTENDEDKEY`), Maus relativ über `MOUSEEVENTF_MOVE`. Dadurch gelten die Mauseinstellungen des Laptops (Geschwindigkeit, Beschleunigung), und es fühlt sich nativ an. Optional „1:1-Modus“ ohne Beschleunigung.
- Scrollen: `MOUSEEVENTF_WHEEL`/`HWHEEL` mit feinen Deltas (Präzisions-Scrollen).
- Nach jedem injizierten Move prüft der Laptop per `GetCursorPos`, ob der Cursor an der **Rückkehrkante** steht und weiter dagegen gedrückt wird. Dann sendet er `EdgeHit`, und das Steuergerät wechselt zurück. Damit gilt die echte Cursor-Position inkl. Beschleunigung.
- Der Laptop führt Buch über alle gedrückten Tasten und Buttons, für `release_all()`.
- Beim `Enter` werden Caps/Num/Scroll-Lock synchronisiert (gegebenenfalls Toggle-Taste injizieren).

**UIPI / Rechte:** Ein nicht erhöhter Prozess kann nicht in erhöhte Fenster injizieren (Task-Manager, Installer). Deshalb läuft der Agent ab M2 erhöht über die Aufgabenplanung beim Anmelden, ohne UAC-Prompt. UAC-Abfrage, Sperrbildschirm und Anmeldebildschirm (Secure Desktop) brauchen den SYSTEM-Dienst (M6).

### 8.2 Bildschirm-Layout und Kantenwechsel
- Jedes Gerät meldet seine Bildschirme (physische Pixel, DPI, Position). Bei Änderungen (`WM_DISPLAYCHANGE`, `WM_DPICHANGED`) schickt es ein Update.
- **Layout-Editor** in der UI wie in den Windows-Anzeigeeinstellungen: Die Bildschirme beider Geräte als Kacheln, maßstabsgetreu nach physischer Größe, per Drag anordnen und andocken (links/rechts/oben/unten, mit Versatz).
- **Wechsel-Mathe:** Überlappende Kantensegmente werden über die **relative Position** auf dem gemeinsamen Segment (0…1) abgebildet. Unterschiedliche Auflösungen und DPI wirken so natürlich.
- Optionen: Wechsel-Verzögerung (0–500 ms), „nur mit gedrückter Taste wechseln“ (z. B. Strg), „Ecken sperren“ (keine Wechsel in den Bildschirmecken, damit Startmenü oder Schließen-X nicht versehentlich wechseln), Doppel-Anstoßen.
- **OSD:** Beim Wechsel blendet das Zielgerät eine kleine Pille ein: Icon + „Laptop“, 600 ms, Fade.

### 8.3 Laptop-Ton → Headset (F2)
- **Laptop:** Das Standard-Wiedergabegerät ist **„Desk Bridge Lautsprecher“** (virtueller Treiber, M3b). Der Agent liest dessen Daten (Ring-Buffer bzw. WASAPI-Loopback auf diesem Endpunkt). Die echten Laptop-Lautsprecher bleiben still.
  - **Übergang M3a:** WASAPI-Loopback auf dem echten Standardgerät. **Zu prüfen:** ob Stummschalten oder Volume 0 auch den Loopback stumm macht (je nach Treiber). Falls ja, ist für M3a als Workaround ein Dummy-Ausgabegerät nötig.
- 48 kHz, Stereo, f32 intern, 5-ms-Pakete.
- **Codec „Auto“:** Bei Kabel-LAN und Verlust < 0,1 % geht **rohes PCM** (16 bit, ≈ 1,5 Mbit/s) raus: null Codec-Latenz, minimale CPU. Sonst Opus `RESTRICTED_LOWDELAY` mit 10-ms-Frames und 128–192 kbit/s. Umschaltung nahtlos.
- Senden als QUIC-Datagramm.
- **PC:** Adaptiver **Jitter-Buffer** (Start 10 ms auf Kabel bzw. 20 ms auf WLAN, Bereich 5–60 ms je nach gemessenem Jitter), PLC bei Verlust, Ausgabe über WASAPI Shared Mode auf das Headset. Windows mischt mit dem PC-Ton.
- **Funk-Headset mit Dongle:** Das Headset bringt selbst ca. 20–40 ms Latenz mit, umso wichtiger ist unser Anteil. Viele Dongles melden **mehrere Geräte** (z. B. „Game“ und „Chat“); die Geräteauswahl zeigt alle mit verständlichen Namen. Wird das Headset ausgeschaltet, verschwindet das Gerät. Dann puffert Desk Bridge nicht endlos, sondern pausiert den Stream und setzt automatisch fort, sobald das Gerät wieder da ist (`IMMNotificationClient`). Das Mikro wechselt dabei nicht heimlich auf ein anderes Gerät.
- **Clock-Drift:** Die Soundkarten laufen nie exakt gleich schnell. Der Füllstand des Jitter-Buffers regelt ein Resampling-Verhältnis (`rubato`, ±0,2 %) nach, unhörbar und ohne Knackser nach Stunden.
- **Stille-Erkennung:** Bei digitaler Stille werden nur Keep-Alive-Frames gesendet (spart CPU und Bandbreite).
- UI: Lautstärke und Stumm für „Laptop-Ton“ separat, Pegelanzeige, gemessene Latenz.

### 8.4 Headset-Mikro → Laptop (F3)
- **PC:** WASAPI-Capture vom gewählten Mikro (Standard: Kommunikations-Standardgerät). Shared Mode, damit PC-Apps das Mikro gleichzeitig nutzen können. Opus `VOIP` oder `RESTRICTED_LOWDELAY`, Mono 48 kHz, 10-ms-Frames, 64 kbit/s.
- **Laptop:** Jitter-Buffer und Drift-Ausgleich wie oben, dann ins **„Desk Bridge Mikrofon“** des virtuellen Treibers schreiben.
- **Datenschutz-Feature (wichtig!):** Der Treiber meldet, ob eine App den Mikro-Endpunkt **geöffnet** hat (`MicInUse`). Nur dann nimmt der PC das Mikro auf und sendet. Im PC-Tray erscheint der Hinweis „🎙 Laptop nutzt dein Mikro (Teams)“, sofern der Prozessname ermittelbar ist. Zusätzlich gibt es einen globalen Stumm-Hotkey.
- **Option:** „Desk Bridge Mikrofon/Lautsprecher als Standardgerät setzen“, inklusive Kommunikationsgerät. Die API (`IPolicyConfig`) ist undokumentiert, wird aber verbreitet genutzt; das Setzen gibt es nur mit Checkbox-Zustimmung.
- **Keine Übergangslösung mit Fremdsoftware:** Die Mikro-Weiterleitung kommt zusammen mit dem eigenen bzw. mitgelieferten Treiber (M3b). Bis dahin ist die Funktion in der UI als „kommt bald“ ausgegraut.

### 8.5 Zwischenablage (F4)
- `AddClipboardFormatListener` → `WM_CLIPBOARDUPDATE`.
- **Loop-Vermeidung:** Eigene Schreibvorgänge bekommen ein privates Format `DeskBridge.Origin` mit Transfer-ID, außerdem wird `GetClipboardSequenceNumber` gemerkt.
- **Unterstützte Formate:** `CF_UNICODETEXT`, `HTML Format`, `Rich Text Format`, Bilder (`PNG`, `CF_DIBV5` → intern als PNG übertragen), Dateien (`CF_HDROP` bzw. virtuelle Dateien, siehe 8.6).
- **Strategie:**
  - Text < 1 MiB wird sofort übertragen (eager): Copy auf A, kurz darauf liegt der Text auch auf B.
  - Alles andere läuft über **Delayed Rendering**: A schickt nur `ClipboardOffer{formate, größen}`, B registriert die Formate mit `SetClipboardData(fmt, NULL)`. Erst beim Einfügen (`WM_RENDERFORMAT`) wird synchron mit Timeout (5 s) und Fortschritts-OSD bei großen Daten von A geholt.
- **Einstellungen:** Zwischenablage-Sync an/aus pro Richtung, maximale Größe, „keine Passwort-Manager-Inhalte“ (Formate `ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory = 0` respektieren).

### 8.6 Dateiübertragung (F5)
- **Stufe 1 (M4a):** Dateien in der Zwischenablage (`CF_HDROP`) → Offer mit Dateiliste und Größen. Beim Einfügen auf B werden sie nach `%LOCALAPPDATA%\softwelop\DeskBridge\Incoming\<id>\` übertragen und dann als `CF_HDROP` bereitgestellt. Explorer kopiert von dort ans Ziel. Fortschritt zeigt unser OSD.
- **Stufe 2 (M4b, die elegante Lösung):** **Virtuelle Dateien** über OLE: ein eigenes `IDataObject` mit `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` (je Datei ein `IStream`, der aus dem Netzwerk liest), gesetzt per `OleSetClipboard`. Explorer zeigt dann **seinen eigenen Kopierdialog mit Fortschritt**, ohne Zwischenkopie. Ordner werden rekursiv als Descriptor-Liste abgebildet.
- **Weitere Wege:** Drop-Zone in der UI, Eintrag unter „Senden an → Desk Bridge: Laptop“ (Verknüpfung im `SendTo`-Ordner, ruft `deskbridge-agent send <pfade>` auf), Zielordner einstellbar (Standard `Downloads\Desk Bridge`).
- **Technik:** 256-KiB-Chunks, mehrere Dateien parallel (max. 4 Streams), BLAKE3-Prüfsumme pro Datei, Resume nach Abbruch, Zeitstempel erhalten. Ziel: ≥ 80 % der Link-Geschwindigkeit.
- **Verlauf:** die letzten 50 Transfers in der UI, mit „Im Ordner zeigen“.

### 8.7 Fokusmodus – Laptop im Vollbild auf dem PC (F6)

**Ablauf beim Einschalten (Hotkey `Strg+Alt+Shift+F` oder Tray/UI):**
1. Der PC ermittelt den Zielmonitor (Standard: der Monitor, auf dem der Cursor ist) und dessen Modus, bei Thorben **5120×1440 @ 120 Hz**.
2. Der PC sendet `FocusStart{5120, 1440, 120, codec, bitrate}`. Codec und 4:4:4 ergeben sich aus der Schnittmenge der Encoder-Fähigkeiten (Laptop) und Decoder-Fähigkeiten (PC), die beim Hello als Capabilities ausgetauscht werden.
3. Der Laptop:
   a. sichert die aktuelle Display-Topologie (`QueryDisplayConfig`),
   b. aktiviert das **virtuelle Display** mit genau diesem Modus (8.8),
   c. macht es zum primären Display; optional (Standard an) wird das interne Panel abgeschaltet, damit alle Fenster auf den „großen“ Monitor wandern (`SetDisplayConfig`),
   d. startet Capture und Encoder und antwortet `FocusReady`.
4. Der PC öffnet ein randloses Vollbildfenster auf dem Zielmonitor (eigenes natives Fenster, nicht die WebView) und rendert die Frames.
5. Input wechselt auf **absolute Koordinaten**: Die PC-Mausbewegung bewegt einen lokal gezeichneten Cursor sofort (gefühlt null Latenz). Gleichzeitig gehen `MouseAbs` an den Laptop (`MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`, normalisiert auf den virtuellen Monitor).
6. Audio und Mikro laufen ohnehin weiter.

**Beim Ausschalten:** `FocusStop` → der PC schließt das Fenster, der Laptop stoppt den Encoder, stellt die gesicherte Topologie wieder her (Rollback auch bei Absturz: Topologie wird zusätzlich auf Platte gesichert und beim nächsten Agent-Start geprüft) und schaltet das virtuelle Display ab.

**Pipeline Laptop:**
- **Last:** 5120×1440×120 ≈ **885 Megapixel/s**, das entspricht etwa 4K bei 107 fps. Das schafft NVENC der RTX 5060, aber es ist die anspruchsvollste Stelle des Projekts. Deshalb gibt es **gleich zu Beginn von M5 einen Spike**: Capture → NVENC → Decode bei 5120×1440@120 messen, bevor die restliche Pipeline gebaut wird.
- **Hybrid-Grafik (Optimus):** Das interne Panel hängt meist an der iGPU, NVENC sitzt auf der NVIDIA-dGPU. Das virtuelle Display muss deshalb **auf der NVIDIA-GPU gerendert werden** (der Virtual Display Driver erlaubt die Wahl der Render-GPU). Dann liegen Capture-Textur und Encoder auf demselben Adapter, zero-copy. Sonst wäre pro Frame eine Kopie zwischen den GPUs nötig (Fallback über Shared Texture, kostet Latenz). `crates/display` wählt den Adapter mit HW-Encoder automatisch.
- **Capture:** DXGI Desktop Duplication auf dem virtuellen Output (funktioniert mit IddCx-Displays). Frames bleiben als D3D11-Textur auf der GPU.
- **Cursor:** Form und Position aus der Duplication-API **getrennt** senden (nicht ins Bild brennen). Der PC zeichnet den Cursor selbst.
- **Encode:** FFmpeg mit HW-Encoder über D3D11-HW-Frames. Primär `hevc_nvenc` (RTX 5060), zusätzlich `av1_nvenc`, `h264_nvenc`, `*_qsv`, `*_amf` für andere Laptops.
  - Low-Latency-Preset (P1–P3, `tune=ull`), **keine B-Frames**, keine Lookahead, VBV ≈ 1 Frame, unendliche GOP plus Intra-Refresh **oder** IDR nur auf Anfrage.
  - Startwerte (Kabel): **HEVC 5120×1440@120, 120 Mbit/s**, adaptiv 40–300 Mbit/s nach Verlust, RTT und Encode-Zeit. Bei WLAN 60 fps und 40 Mbit/s.
  - **AV1** als Option, wenn der PC AV1 hardwareseitig decodieren kann (bessere Qualität pro Bit). Bei Thorben möglich: RTX 5060 encodiert AV1, RTX 30/40/50 decodiert es. Der M5-Spike entscheidet, ob HEVC oder AV1 Standard wird.
  - Option „Text gestochen scharf“: HEVC 4:4:4 (NVENC kann es), **wenn der PC-Decoder es unterstützt**; bei NVIDIA RTX 30+ ist das der Fall.
  - **Kein Software-Encoder-Fallback in v1:** Ohne HW-Encoder ist der Fokusmodus mit einem Hinweis ausgegraut (praktisch jeder Laptop seit ca. 2015 hat NVENC, QSV oder AMF). Für OSS später eventuell ergänzen.
- **Frame-Pacing:** Ändert sich nichts (keine Dirty-Rects), wird nichts encodiert; alle 500 ms ein Keep-Alive-Frame. Bei 120 Hz ist das Budget pro Frame 8,3 ms, deshalb sind Capture, Encode und Senden als Pipeline über Threads verteilt, nicht sequenziell.

**Pipeline PC:**
- Empfang → FFmpeg `d3d11va`-Decode → D3D11-Textur → Swapchain `FLIP_DISCARD` mit `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`, `ALLOW_TEARING` und VRR (G-Sync/FreeSync, falls der Monitor es kann). Jedes Frame wird **sofort bei Ankunft** präsentiert, ohne auf VSync zu warten.
- Der PC-Decoder muss 5120×1440@120 in HEVC schaffen. Das wird beim Start geprüft (`D3D11 VideoDecoder`-Profile + Testdecode); sonst schlägt die App automatisch 60 fps vor.
- Skalierung, falls die Auflösungen nicht exakt passen (Fallback M5a), mit Letterbox und Bilinear/Lanczos-Shader.
- Zu spät gekommene Frames werden verworfen; bei Decoder-Fehler `KeyframeRequest`.
- Stats-Overlay (`Strg+Alt+Shift+I`): FPS, Bitrate, Verlust, Encode-, Netz-, Decode- und Gesamt-Latenz.

**Split-Modus (passt perfekt zum 32:9-Monitor):**
- Der Laptop bekommt ein virtuelles Display mit **2560×1440 @ 120 Hz**. Der PC zeigt es randlos auf der linken oder rechten Hälfte des Ultrawide, der PC-Desktop bleibt auf der anderen Hälfte.
- Das Stream-Fenster meldet sich beim PC als **AppBar** an (`SHAppBarMessage`). Dadurch schrumpft der Arbeitsbereich des PCs auf die freie Hälfte, und maximierte PC-Fenster überdecken den Laptop nicht.
- Die Mitte des Monitors wirkt wie eine **Bildschirmkante**: Maus auf die Laptop-Hälfte schieben → Eingaben gehen an den Laptop (absolut), zurück → PC. Kein Hotkey nötig.
- Umschalten Vollbild ↔ Split ↔ aus per Hotkey (`Strg+Alt+Shift+F` wechselt durch, `Strg+Alt+Shift+D` für Split direkt) oder in der UI. Seite und Breite (50/50, 2/3, 1/3) sind einstellbar; das virtuelle Display wird jeweils passend umkonfiguriert.

**Extras:**
- „Fenster-Modus“: Laptop-Bild in einem normalen, frei skalierbaren Fenster statt Vollbild (praktisch nebenbei).
- **Automatisch** (Option, Standard aus): Wird der Laptop zugeklappt, startet der Fokusmodus. Wird er aufgeklappt, endet er.
- **Laptop zugeklappt lassen:** Die Option setzt die Deckel-Aktion „Nichts unternehmen“ für den Netzbetrieb (`powercfg`, Systemeinstellung, nur mit Zustimmung; beim Deinstallieren zurücksetzen).

### 8.8 Virtuelles Display
- Mitgeliefert wird der **Virtual Display Driver** (IddCx, MIT-Lizenz, signierte Releases, unterstützt eigene Auflösungen und Bildwiederholraten). **Beim Start von M5b prüfen:** aktuelle Version, Signatur-Status, Steuer-API (Settings-Datei bzw. Pipe) und Lizenz. Ergebnis als ADR festhalten.
- Der Treiber wird **vom Desk-Bridge-Installer mitgebracht und still installiert** (Treiberpaket im Installer, Geräteanlage durch `deskbridge-setup.exe` per SetupAPI). Der Nutzer muss den Treiber-Hersteller nicht kennen.
- `crates/display` kapselt: Treiber vorhanden?, virtuellen Monitor mit Modus X an/aus, **Render-GPU wählen** (die mit HW-Encoder, siehe Optimus in 8.7), Topologie sichern/setzen/wiederherstellen.
- Die Modi des PC-Zielmonitors werden beim Pairing und bei Änderungen in die Treiber-Config geschrieben, damit genau der passende Modus existiert. Bei Thorben: 5120×1440@120/60 (Vollbild) und 2560×1440@120/60 bzw. 3413×1440 und 1707×1440 (Split-Varianten).
- Solange der Fokusmodus aus ist, ist **kein** virtueller Monitor aktiv. Windows sieht dann nur das interne Panel, und es gibt keine „Geisterfenster“ auf unsichtbaren Monitoren.
- **Fallback M5a** (bevor das virtuelle Display integriert ist): Streamen der nativen Laptop-Auflösung mit Letterbox.

### 8.9 Treiber und Signierung (ehrliche Einschätzung)
- **Virtuelles Display:** Fertiger signierter Treiber, deshalb **geringes Risiko**.
- **Virtuelles Audio („Desk Bridge Audio“):** Das ist der **härteste Teil des Projekts** (Kernel-Mode, C++, WDK).
- **Recherche-Spike zu Beginn von M3b, Ergebnis als ADR.** Bewertet werden fertige, bereits signierte OSS-Treiber (z. B. aus dem Virtual-Drivers-Umfeld), mit diesen Kriterien in absteigender Wichtigkeit:
  1. **Signiert** (Attestation/WHQL), installierbar **ohne Testmodus und mit Secure Boot an** → Muss
  2. **Lizenz erlaubt Weitergabe und Änderung** (MIT/BSD/Apache/MS-PL) → Muss
  3. **Render- und Capture-Endpunkt mit Datenpfad zur eigenen App** (Shared-Memory-Ring-Buffer bzw. IOCTL), nicht nur „Kabel“-Loopback → Muss
  4. Meldet „Capture-Stream geöffnet/geschlossen“ (für `MicInUse`) → Soll (ließe sich sonst über WASAPI-Session-Enumeration auf dem Endpunkt nachbilden)
  5. Gerätenamen frei wählbar („Desk Bridge Mikrofon/Lautsprecher“) → Soll
  6. Aktiv gepflegt, Windows 10/11 x64 (ARM64 später) → Soll
  - **Regel:** Erfüllt ein fertiger Treiber 1–3, wird er eingebunden: mitgeliefert, still installiert, **unverändert** (jede Änderung am Treiber würde die Signatur brechen; eigene Gerätenamen nur, wenn er sie per Konfiguration erlaubt). Sonst wird F3 zurückgestellt (siehe Signierung unten).
- **Eigener Treiber (verschoben, nur zur Dokumentation, falls später Budget oder Sponsoring da ist):**
  - Basis Microsoft **SysVAD** (MIT), reduziert auf 1 Render- und 1 Capture-Endpunkt (48 kHz, Stereo/Mono).
  - Datenpfad zum User-Mode: Device-Interface mit IOCTLs, gemeinsamer Ring-Buffer (vom Treiber gemappt) und Event für „Daten verfügbar“.
  - Zusätzlich IOCTL bzw. Event „Capture-Stream geöffnet/geschlossen“ für `MicInUse`.
- **Signierung, ehrlich:** Mit Secure Boot lädt Windows nur Kernel-Treiber, die **Microsoft signiert** hat. Wegen „null Handarbeit“ fällt Test-Signing (BIOS-Einstellung, `bcdedit`, Wasserzeichen) als Weg für den Nutzer aus.
  - Ein **eigener** Treiber bräuchte Microsoft **Attestation Signing**. Das setzt ein **EV-Code-Signing-Zertifikat** voraus (laufende Kosten). **Entscheidung (2026-10-16): Dafür gibt es kein Budget.**
  - **Daraus folgt:** Desk Bridge nutzt **nur einen fertigen, bereits signierten, weitergebbaren** virtuellen Audio-Treiber (Kriterien 1–3 oben). Der eigene SysVAD-Fork ist **nicht** der Plan B, sondern verschoben.
  - **Wenn der Spike keinen passenden Treiber findet:**
    - Laptop-Ton → Headset (F2) läuft trotzdem über WASAPI-Loopback (M3a), ganz ohne Treiber.
    - Das **virtuelle Mikro (F3) wird zurückgestellt**. Die UI zeigt es ehrlich als „noch nicht verfügbar“, der Rest bleibt voll nutzbar.
    - Später erneut prüfen: neue OSS-Treiber, Sponsoring der Signierkosten nach dem OSS-Release, oder doch ein Zertifikat, wenn sich das Projekt lohnt.
    - Nur auf Thorbens ausdrücklichen Wunsch: eigener Treiber mit Test-Signing als private Bastellösung. Das widerspricht „null Handarbeit“, deshalb nicht Standard.
  - Ziel: Treiber-Installation **ohne Windows-Rückfrage**. Microsoft-signierte Pakete installieren normalerweise still. Fragt Windows trotzdem nach, legt der Installer das Herausgeber-Zertifikat vorab in „Vertrauenswürdige Herausgeber“. Beim Spike prüfen.
  - Der Display-Treiber ist bereits signiert; für ihn entfällt das Thema, sofern das ADR in M5b das bestätigt.

---

## 9. UI/UX im softwelop-Branding

### 9.1 Brand-Grundlagen (Quelle: https://softwelop.com/brand/)
- **Assets:** `tokens/brand-tokens.css`, `tokens/colors.json`, Logos (SVG) und Icons (`icons/icon-set-komplett.svg`) nach `app/src/brand/` übernehmen. Die Quelle steht in der README. **Fonts nicht** von dort übernehmen (Gilmer entfällt); Montserrat kommt als OFL-Variable-Font aus dem offiziellen Google-Fonts-Repo, mit `OFL.txt` im Repo.
- **Farben:**
  | Token | Wert | Verwendung in der App |
  |---|---|---|
  | `--sw-deep-trust` | `#21264E` | Text (hell), Haupt-Fläche (dunkel) |
  | `--sw-core-logic` | `#194093` | Primär-Buttons, aktive Zustände, Links, Fokus-Ring |
  | `--sw-bright-code` | `#A3C7EB` | Sekundär-Flächen, Akzent im Dark Mode, Pegelanzeigen |
  | `--sw-signal-orange` | `#F18557` | **Akzent**: „Live“-Zustände (Mikro aktiv, Fokusmodus an), CTA |
  | `--sw-soft-spark` | `#F7AE67` | Warnungen (sanft), Highlights |
  | `--sw-powerline-red` | `#E36150` | Fehler, Trennen, Stumm-Indikator |
  | `--sw-cream` | `#F6E9DF` | Karten- und Flächenton (hell) |
  | `--sw-white` | `#FFFFFF` | Hintergrund (hell) |
- **Verläufe:** `--sw-gradient-deep` für Onboarding-Hintergrund und Dark-Mode-Header, `--sw-gradient-sunset` für den Erfolgs-Screen „Verbunden!“, `--sw-gradient-coral` sparsam.
- **Typo (entschieden): nur Montserrat.** Textfarbe immer `#21264E` (hell) bzw. Creme (dunkel), nie Schwarz. Lesbarkeit geht vor Brand-Purismus:
  | Rolle | Gewicht | Größe |
  |---|---|---|
  | Display/Hero (Onboarding) | Bold 700 | 32–40 px |
  | Überschrift Screen | Bold 700 | 24 px |
  | Überschrift Karte | SemiBold 600 | 16–18 px |
  | Fließtext | Light 300 (ab 15 px) bzw. Regular 400 (< 15 px) | 14–16 px, Zeilenhöhe 1,5 |
  | Labels, Buttons, Chips, Tray-Flyout | **Medium 500** | 12–14 px |
  | Zahlen (Latenz, Bitrate) | Medium 500, `font-variant-numeric: tabular-nums` | – |
  - Satzschreibung (sentence case), keine Versalien außer bei sehr kurzen Badges.
- **Logo:** softwelop-Wordmark klein im Über-Dialog und Onboarding-Footer („by softwelop“, min. 70 px breit). Keine Effekte, keine Verzerrung, kein unruhiger Hintergrund.
- **Eigenes App-Icon „Desk Bridge“ im softwelop-Stil (entschieden):**
  - Motiv: zwei Bildschirme (Monitor + Laptop) als geometrische Line-Art, verbunden durch einen **Brückenbogen** in Signal Orange. Strichstärke wie im softwelop-Icon-Set.
  - App-Icon: Motiv in Weiß bzw. Creme auf `--sw-gradient-deep` (abgerundetes Quadrat, Windows-11-Stil). Tray-Icon: vereinfachtes Motiv einfarbig (hell/dunkel je nach Taskleiste) mit farbigen Status-Punkten.
  - Erstellen als SVG in M0 (2–3 Varianten zur Auswahl für Thorben), daraus `.ico` (16/20/24/32/48/64/256 px) und Tray-Varianten generieren. Bei 16 px Pixel-Hinting prüfen.
- **Icons:** Die Brand-Line-Icons (Laptop, Monitor usw.) passen perfekt zu Geräten. Fehlende Icons (Mikro, Lautsprecher, Zwischenablage, Tastatur) kommen aus Lucide mit gleicher Strichstärke, eingefärbt in Deep Trust.

### 9.2 Abgeleitetes UI-Design-System (`app/src/brand/ui-tokens.css`)
- **Dark Mode** (in der Brand-Guideline nicht definiert, daher hier abgeleitet):
  - Hintergrund `#161A38` (abgedunkeltes Deep Trust), Flächen `#21264E`, erhöhte Flächen `#2A3060`
  - Text `#F6E9DF` (Creme) bzw. `#FFFFFF`, Akzent Signal Orange, Primär `#A3C7EB`
  - Folgt dem Windows-Theme; manuell umschaltbar.
- Radius 12 px (Karten), 8 px (Buttons/Inputs), 999 px (Pillen/Status-Chips). Abstände in einer 4-px-Skala (4/8/12/16/24/32/48). Weiche Schatten nur im Hellmodus (`0 4px 24px rgba(33,38,78,.08)`).
- Windows-11-Feeling: Mica/Acrylic-Hintergrund für das Hauptfenster (`window-vibrancy`), abgerundete Fensterecken, eigene Titelleiste mit Windows-Fensterknöpfen.
- Motion: 150–250 ms, `ease-out`. Statuswechsel animiert (Chip pulsiert sanft bei „Live“). `prefers-reduced-motion` wird respektiert.
- Barrierefreiheit: Kontrast ≥ 4,5:1, vollständige Tastaturbedienung, sichtbarer Fokus-Ring (Core Logic), Screenreader-Labels.

### 9.3 Screens

1. **Onboarding** (erster Start, Hintergrund Deep-Verlauf)
   - Willkommen: Logo, ein Satz („Ein Schreibtisch. Zwei Rechner. Null Umschalten.“), Button „Los geht’s“.
   - Gerätename (vorbelegt mit dem Computernamen).
   - „Ist das Gerät mit Maus, Tastatur und Headset?“ → Ja (PC) / Nein (Laptop). Setzt die Standard-Rollen, später änderbar.
   - Koppeln: Das eine Gerät zeigt einen großen 6-stelligen Code, das andere listet gefundene Geräte und fragt den Code ab.
   - **Layout:** „Wo steht dein Laptop?“, Bildschirme per Drag anordnen.
   - Audio: Headset wählen, Testton „vom Laptop“ abspielen, Mikro-Test mit Pegel.
   - Fertig-Screen mit Sunset-Verlauf und „Verbunden ✓“. Die wichtigsten drei Hotkeys stehen als Spickzettel daneben.
2. **Dashboard (Startseite)**
   - Großes Geräte-Paar-Visual (zwei Brand-Icons, dazwischen eine animierte Verbindungslinie, Farbe nach Qualität).
   - Status-Chips: `Maus & Tastatur` · `Laptop-Ton` · `Mikro` · `Zwischenablage` · `Fokusmodus`. Jeder Chip ist umschaltbar und zeigt seinen Zustand (aus / bereit / live).
   - Großer Button **„Fokusmodus starten“**.
   - Verbindungsqualität: RTT, Kabel/WLAN, Latenz-Mini-Sparkline.
3. **Bildschirme:** Layout-Editor + Wechsel-Optionen (8.2).
4. **Audio:** zwei Karten.
   - „Laptop → Headset“: Ausgabegerät, Lautstärke, Stumm, Pegel, Latenz, Qualität (Opus/PCM).
   - „Headset-Mikro → Laptop“: Eingabegerät, Pegel, Stumm-Hotkey, „nur senden, wenn genutzt“ (Standard an), „als Standardgerät setzen“.
5. **Dateien & Zwischenablage:** Drop-Zone, Verlauf, Zielordner, Formate und Größenlimits, Richtungs-Schalter.
6. **Fokusmodus:** Modus (Vollbild / Split / Fenster) mit Live-Vorschau-Grafik des 32:9-Monitors, Split-Seite und -Breite, Zielmonitor, Auflösung (Auto = Monitor), FPS (Auto/60/120/144), Codec (Auto/HEVC/AV1/H.264), Qualität (Bitrate Auto/Manuell), „Text scharf (4:4:4)“, „internes Laptop-Display abschalten“, Deckel-Automatik.
7. **Tastenkürzel:** Liste aller Hotkeys, editierbar, mit Konflikt-Erkennung.
8. **Einstellungen:** Autostart, Sprache, Theme, Netzwerk (Port, manuelle IP), Geräte (umbenennen, entkoppeln), Updates, Diagnose (Logs öffnen, Diagnosepaket exportieren, Latenz-Test).
9. **Über:** Version, Lizenzen (Drittanbieter), Link zu softwelop.com.

### 9.4 Tray
- Icon-Zustände: verbunden (normal), getrennt (grau), Mikro live (orangener Punkt), Fokusmodus (Badge).
- Menü:
  - Status-Zeile („Verbunden mit Laptop · 1 ms“)
  - Fokusmodus an/aus
  - Laptop-Ton stumm
  - Mikro-Weiterleitung stumm
  - Cursor sperren
  - Datei senden…
  - Öffnen
  - Pausieren (alles trennen)
  - Beenden
- Linksklick öffnet ein kompaktes Flyout (wie das Windows-Schnellmenü) mit den Status-Chips.

### 9.5 OSD-Overlay
- Transparentes, klick-durchlässiges, immer-oben-Fenster (separates Tauri-Fenster oder natives Layered Window für minimale Latenz).
- Zeigt: Gerätewechsel („→ Laptop“), Mikro stumm/aktiv, Fokusmodus an/aus, Dateitransfer-Fortschritt, „Verbindung verloren – Eingabe zurück am PC“.

### 9.6 Standard-Hotkeys (alle änderbar)
| Aktion | Standard |
|---|---|
| Fokusmodus: aus → Vollbild → Split → aus | `Strg+Alt+Shift+F` |
| Split-Modus direkt an/aus | `Strg+Alt+Shift+D` |
| Cursor auf aktuellem Gerät sperren | `Rollen` (Scroll Lock) |
| Direkt zum anderen Gerät springen | `Strg+Alt+Shift+Leertaste` |
| Mikro-Weiterleitung stumm | `Strg+Alt+Shift+M` |
| Laptop-Ton stumm | `Strg+Alt+Shift+S` |
| Stats-Overlay (Fokusmodus) | `Strg+Alt+Shift+I` |
| Strg+Alt+Entf an Zielgerät | `Strg+Alt+Ende` |
| **Notfall: alles lokal, Fokus beenden** | `Strg+Alt+Shift+Rücktaste` |

---

## 10. Konfiguration und Datenablage
| Was | Wo |
|---|---|
| Einstellungen | `%APPDATA%\softwelop\DeskBridge\config.toml` |
| Gekoppelte Geräte | `%APPDATA%\softwelop\DeskBridge\peers.toml` |
| Identität (DPAPI-verschlüsselt) | `%APPDATA%\softwelop\DeskBridge\identity.bin` |
| Logs (7 Tage, rollierend) | `%LOCALAPPDATA%\softwelop\DeskBridge\logs\` |
| Eingehende Dateien (temporär) | `%LOCALAPPDATA%\softwelop\DeskBridge\Incoming\` |
| Gesicherte Display-Topologie | `%LOCALAPPDATA%\softwelop\DeskBridge\topology.json` |

- Config mit `schema_version`, Migrationen in `core::config::migrate`.
- Die UI ändert Einstellungen nur über den Agent (IPC); der Agent schreibt die Datei und lädt sie live neu.

---

## 11. Sicherheit und Datenschutz
- **Alles bleibt lokal.** Keine Cloud, kein Account, **keine Telemetrie**. Fehlerberichte werden nur lokal erstellt und vom Nutzer bewusst exportiert.
- Ende-zu-Ende verschlüsselt (QUIC/TLS 1.3, mTLS mit gepinnten Zertifikaten). Es werden Tastenanschläge übertragen, deshalb gibt es **niemals** einen unverschlüsselten Modus.
- Der Pairing-Code ist nur kurz gültig, mit Rate-Limit (3 Fehlversuche → 60 s Sperre).
- Nur „Privat“-Netzwerkprofil in der Firewall.
- Das Mikro wird nur übertragen, wenn das Zielgerät es aktiv nutzt; das ist sichtbar angezeigt.
- Sensible Zwischenablage-Inhalte (Passwort-Manager-Flags) werden nicht synchronisiert.
- Der Agent läuft erhöht, deshalb ist die IPC-Pipe auf den aktuellen Benutzer beschränkt (ACL), und es gibt keine Netzwerk-API außer dem Peer-Protokoll.
- Abhängigkeiten werden mit `cargo audit` und `cargo deny` in der CI geprüft.
- Später für OSS: `SECURITY.md`, Responsible-Disclosure-Kontakt.

---

## 12. Installation, Autostart, Updates, Signierung

### 12.1 Prinzip: Alles mitgeliefert, null Handarbeit
Der Nutzer lädt **eine** Datei herunter (`DeskBridge-Setup-x.y.z.exe`), installiert sie auf beiden Geräten und koppelt. **Fertig.** Es gibt keinen Schritt, bei dem man etwas anderes installieren, im BIOS umstellen, `bcdedit` ausführen oder eine Einstellung von Hand setzen muss.

| Komponente | Wie sie mitkommt | Lizenz | Ab |
|---|---|---|---|
| Desk Bridge Agent, UI, Dienst | im Installer | MIT OR Apache-2.0 | M0 |
| WebView2-Runtime | Offline-Installer eingebettet; installiert nur, falls nicht vorhanden (Win 11 hat sie) | Microsoft, frei weitergebbar | M0 |
| C-Runtime | statisch gelinkt (`+crt-static`); falls FFmpeg-DLLs sie brauchen, `vcruntime`-DLLs app-lokal | Microsoft, weitergebbar | M0 |
| Montserrat | im App-Bundle | SIL OFL 1.1 | M0 |
| Firewall-Regel | von `deskbridge-setup.exe` angelegt | – | M1 |
| Autostart (Aufgabenplanung, erhöht) | von `deskbridge-setup.exe` angelegt | – | M2 |
| Opus | statisch in den Agent gelinkt | BSD | M3a |
| Virtueller Audio-Treiber | Treiberpaket im Installer, Gerät per SetupAPI angelegt, signiert (8.9) | je nach ADR (MIT/MS-PL) | M3b |
| FFmpeg (LGPL-Build mit NVENC/AMF/QSV/D3D11VA) | DLLs im Programmordner; in der CI gebaut bzw. als LGPL-Shared-Build bezogen, Version gepinnt, Hash geprüft | LGPL 2.1+ (ohne `--enable-gpl`/`--enable-nonfree`) | M5a |
| Virtual Display Driver | Treiberpaket im Installer, still installiert | MIT | M5b |

- **Regel für die Entwicklung:** Eine neue Abhängigkeit, die nicht in diese Tabelle passt (also nicht mitlieferbar oder nicht still installierbar), braucht ein ADR und Thorbens OK.
- **GPU-Treiber** (NVIDIA usw.) liefern wir natürlich nicht mit. Fehlt der HW-Encoder, zeigt die UI einen klaren Hinweis.

### 12.2 Installer (NSIS, per-machine, Admin)
1. Dateien nach `C:\Program Files\softwelop\Desk Bridge\`
2. `deskbridge-setup.exe install` (eigener Helfer, kein devcon/nefcon/pnputil-Gefrickel für den Nutzer):
   - Firewall-Regel (Privat)
   - Treiber ins Driver Store aufnehmen (`DiInstallDriverW`) und root-enumerierte Geräte anlegen (`SetupDiCreateDeviceInfo` + `SetupDiCallClassInstaller`)
   - Aufgabenplanung `DeskBridge Agent` beim Anmelden, mit höchsten Privilegien
   - Dienst registrieren (ab M6)
3. Startmenü-Eintrag, „Senden an“-Verknüpfung
4. Am Ende startet die App direkt ins Onboarding.
5. **Deinstallation** entfernt alles sauber: Treiber und Geräte, Firewall-Regel, Task, Dienst, eine geänderte Deckel-Einstellung und optional die Benutzerdaten.
6. **Upgrade** über denselben Installer, ohne Neu-Pairing; Treiber werden nur aktualisiert, wenn sich ihre Version geändert hat.
7. Ein **Neustart ist nicht nötig**; falls doch (z. B. beim Treiber-Update), fragt der Installer ausdrücklich.

### 12.3 Weiteres
- **Single Instance** für UI und Agent.
- **Updates:** Tauri-Updater mit signiertem Manifest auf GitHub Releases. Die UI zeigt Updates an, installiert wird erst nach Klick. Optional „Partnergerät auch aktualisieren“: Das Update-Paket geht über die bestehende Verbindung rüber, der Partner fragt per Dialog nach. Haben beide Geräte unterschiedliche Versionen, weist die UI darauf hin.
- **Code-Signing (Binaries), ohne Budget:**
  - Eigengebrauch: unsigniert. Dateien, die per LAN oder eigenem Build kommen, haben kein „Mark of the Web“, also gibt es keine SmartScreen-Warnung. Nur aus dem Internet geladene Installer zeigen einmal „Unbekannter Herausgeber → Trotzdem ausführen“.
  - Ab OSS-Release: **SignPath Foundation** (kostenlos für Open-Source-Projekte) für Installer und Binaries beantragen → ADR in M7. Kernel-Treiber deckt das nicht ab, deshalb gilt weiterhin 8.9.

---

## 13. Logging und Diagnose
- `tracing` mit Spans pro Subsystem, Level per Config/UI umschaltbar.
- **Diagnose-Seite:** Live-Metriken (RTT, Paketverlust, Audio-Puffer-Füllstand, Drift-Ratio, Video-Latenzen, CPU des Agent).
- **„Diagnosepaket exportieren“:** ZIP mit Logs, Config (ohne Schlüssel), Systeminfo (OS, GPU, Audiogeräte, Displays).
- Crash-Handler: Panic-Hook schreibt einen Report. Der Dienst (M6) startet den Agent bei Absturz neu, **vorher** gibt er auf dem Zielgerät alle Tasten frei (Watchdog).

---

## 14. Teststrategie

**Automatisiert (CI):**
- Unit-Tests: Layout-Mathe (Kantenabbildung, DPI), Hotkey-Erkennung, Key-State-Tracking/`release_all`, Jitter-Buffer und Drift-Regler (simulierte Uhren), Protobuf-Kompatibilität (alte ↔ neue Nachrichten), Clipboard-Format-Mapping, Datei-Chunking/Resume.
- Integrations-Tests: zwei `core`-Instanzen mit Mock-Plattform über `127.0.0.1`. Szenarien: Pairing, Reconnect, Kantenwechsel, Verbindungsabbruch während einer gedrückten Taste (→ `release_all` wird aufgerufen), Datei 1 GiB, Clipboard-Delayed-Rendering.
- Netzwerk-Chaos: Testmodus mit künstlichem Verlust, Jitter und Latenz im Transport.
- `tools/fake-peer`: Headless-Peer, der Events aufzeichnet bzw. abspielt.

**Manuell mit echter Hardware (Thorben), Checkliste pro Meilenstein in `docs/testing.md`:**
- Input: Kantenwechsel in alle Richtungen, Ecken, Spiele/Vollbild-Apps, Task-Manager (erhöht), Sperrbildschirm (M6), DE-Sonderzeichen (`@ € { } \ | ~ ä ö ü ß`), AltGr, Tottasten (`^ ´ \``), Win-Shortcuts, Mediatasten, Maus-Zusatztasten.
- Audio: Musik 1 h (Drift/Knackser), Teams-Testanruf, Discord, Lautstärke 0/100, Gerätewechsel im laufenden Betrieb (Headset ab- und anstecken).
- Latenz-Messungen:
  - Input: `tools/latency-probe input`, injiziert Events und misst die Rundreise.
  - Audio: Klick vom Laptop spielen und mit dem Mikro am Headset-Ausgang aufnehmen, Abstand messen.
  - Video: Testmodus blendet einen Zeitstempel ein. Bildschirm und Uhr mit dem Handy in Zeitlupe filmen bzw. Zeitstempel-Differenz per synchronisierter Uhr berechnen.
- Robustheit: Laptop zuklappen/aufklappen, Sleep/Wake beider Geräte, WLAN ↔ Kabel wechseln, Router-Neustart, einseitiger Neustart, 8-h-Soak.
- Fokusmodus: 20× ein/aus, danach muss die Topologie stimmen. Agent während Fokusmodus killen, beim nächsten Start muss die Topologie wiederhergestellt werden.

---

## 15. Roadmap – Meilensteine

> Größen: S ≈ wenige Tage, M ≈ 1–2 Wochen, L ≈ 2–4 Wochen, XL ≈ 4+ Wochen (bei Feierabend-Tempo grob; mit KI-Unterstützung eher schneller).
> **Prinzip:** So früh wie möglich im Alltag nutzbar. Nach M2 ersetzt die App den KVM schon für Maus und Tastatur.

### M0 – Fundament (S)
- [ ] Repo, Cargo-Workspace und Crate-Skelett wie in 6.2, `rust-toolchain.toml`, `rustfmt`/`clippy`-Config, `.editorconfig`
- [ ] CI: fmt, clippy `-D warnings`, Tests, `cargo deny`, Tauri-Build, Installer als Artefakt
- [ ] Tauri-2-App mit React/TS/Vite, Brand-Tokens, Logos und Icons übernommen, **Montserrat** (OFL) eingebunden, Typo-Skala aus 9.1, Light/Dark-Theme
- [ ] **Eigenes App-Icon**: 2–3 SVG-Entwürfe nach 9.1 → Thorben wählt → `.ico` und Tray-Varianten
- [ ] UI-Shell: Navigation, leere Screens, Tray mit Menü, Single Instance, eigene Titelleiste, Mica
- [ ] `tracing`-Logging, Config laden/speichern, `brand.rs`/`brand.ts`
- [ ] NSIS-Installer mit eingebettetem WebView2-Offline-Installer, statische CRT, Grundgerüst `deskbridge-setup.exe`
- [ ] `LICENSE-MIT`, `LICENSE-APACHE`, `cargo deny` mit Lizenz-Whitelist (keine GPL im Kern)
- [ ] `docs/architecture.md`, `docs/adr/0001-tech-stack.md` (fasst Abschnitt 5 zusammen)
- [ ] `scripts/deploy-laptop.ps1`, `scripts/dev-two-instances.ps1`

**Abnahme:** Der Installer aus der CI installiert auf beiden Geräten **ohne jede weitere Installation**, auch auf einem frischen Windows 10. Die App startet mit Tray und eigenem Icon, sieht im Branding gut aus (hell und dunkel), und die CI ist grün.

### M1 – Verbindung (M)
- [ ] Identität und Zertifikate, DPAPI-Speicherung
- [ ] mDNS-Announce/-Browse, manuelle IP
- [ ] Pairing mit Code (SPAKE2), Peers speichern, Entkoppeln
- [ ] QUIC mTLS mit Pinning, `control`-Kanal, Hello/Caps, Ping/Pong (RTT, Clock-Offset)
- [ ] Verbindungs-Zustandsmaschine, Reconnect/Backoff, Reaktion auf Netzwerkwechsel und Sleep/Wake (`WM_POWERBROADCAST`, `NotifyAddrChange`)
- [ ] Agent als eigener Prozess + IPC (Named Pipe, ACL) zur UI, oder bewusst noch `embedded-agent`
- [ ] Firewall-Regel im Installer, Hinweis bei Netzprofil „Öffentlich“
- [ ] UI: Onboarding-Pairing-Flow, Dashboard mit Verbindungsstatus und RTT

**Abnahme:** Koppeln in < 30 s ohne Doku. Nach Sleep/Wake des Laptops verbindet sich die App automatisch in < 5 s nach Netzwerk-Verfügbarkeit. 24 h verbunden ohne Abbruch. Ein fremdes, ungekoppeltes Gerät wird abgewiesen.

### M2 – Maus, Tastatur, Text-Zwischenablage (L) → *erster Alltagseinsatz*
- [ ] LL-Hooks-Thread, Raw Input, Grab/Ungrab
- [ ] `SendInput`-Injector (Scancodes, extended, relativ/absolut, Wheel/HWheel, XButtons)
- [ ] Bildschirm-Erkennung + Änderungs-Events, DPI-Awareness v2
- [ ] Layout-Editor (UI) + Layout-Sync + Wechsel-Mathe
- [ ] Kantenwechsel inkl. `EdgeHit` vom Zielgerät, Ecken sperren, Verzögerung
- [ ] Hotkey-Engine im Hook, Standard-Hotkeys, Cursor-Sperre, Notfall-Hotkey
- [ ] Key-State-Tracking + `release_all` bei jedem Fokusverlust und Verbindungsabbruch, Toggle-Sync (Caps/Num)
- [ ] OSD-Overlay „→ Laptop“
- [ ] Agent-Autostart erhöht über die Aufgabenplanung
- [ ] Zwischenablage **Text** (`CF_UNICODETEXT`) eager, beide Richtungen, Loop-Schutz
- [ ] Input-Latenz-Probe

**Abnahme:** Eine Woche Alltag **ohne KVM für Maus und Tastatur**. Keine hängenden Tasten im Abbruch-Test (LAN-Kabel während Shift-Halten ziehen, 10×). Alle DE-Sonderzeichen korrekt. Input-Latenz p95 < 3 ms (Kabel). Copy/Paste von Text in beide Richtungen.

### M3 – Audio und Mikro (L–XL)
- **M3a – Laptop-Ton → Headset (ohne Treiber)**
  - [ ] WASAPI-Loopback (Laptop), Mute-Verhalten der Laptop-Lautsprecher prüfen (ADR)
  - [ ] PCM- und Opus-Pfad mit Auto-Umschaltung, Datagramm-Transport, Medien-Header
  - [ ] Jitter-Buffer adaptiv, PLC, Drift-Regler mit `rubato`
  - [ ] WASAPI-Render (PC), Geräteauswahl (Dongle-Geräte „Game/Chat“), Lautstärke/Stumm, Pegel, Latenzanzeige
  - [ ] Gerätewechsel und Hot-Plug (`IMMNotificationClient`): Headset aus/an, Dongle ab/an
- **M3b – Virtueller Audio-Treiber + Mikro → Laptop**
  - [ ] **Spike + ADR:** fertige, bereits signierte OSS-Treiber nach den Kriterien in 8.9 suchen und bewerten (Signatur prüfen: `signtool verify /kp`, Lizenz, Datenpfad, Install ohne Testmodus). **Ergebnis Thorben vorlegen.**
  - [ ] **Kein passender Treiber gefunden** → M3b endet hier: Mikro in der UI als „noch nicht verfügbar“, weiter mit M4. Neue Prüfung in M7.
  - [ ] **Passender Treiber gefunden** → `VirtualAudio`-Backend für dessen Datenpfad, Capture-geöffnet-Signal (oder Nachbildung über WASAPI-Sessions)
  - [ ] Stille Installation über `deskbridge-setup.exe`, sauberes Deinstallieren
  - [ ] WASAPI-Capture (PC), Mono-Opus/PCM, Senden; Laptop schreibt ins virtuelle Mikro
  - [ ] `MicInUse` → PC nimmt nur dann auf; Tray-Anzeige „Laptop nutzt dein Mikro“; Stumm-Hotkey
  - [ ] „Als Standardgerät setzen“ (Wiedergabe + Kommunikation)
  - [ ] Laptop-Ton über virtuellen Lautsprecher statt Loopback (Laptop-Lautsprecher bleiben stumm)

**Abnahme M3a:** Laptop-Ton kommt im Headset an, die Laptop-Lautsprecher bleiben still (oder das Problem ist per ADR dokumentiert und gelöst).
**Abnahme M3b** (nur wenn ein Treiber gefunden wurde): Auf einem frischen Laptop installiert der Desk-Bridge-Installer den Audio-Treiber **ohne Rückfrage, ohne Testmodus, mit Secure Boot an**. Teams-Call auf dem Laptop über das Funk-Headset am PC, das Gegenüber bestätigt gute Qualität. Ende-zu-Ende-Latenz < 30 ms (Kabel, ohne Headset-Eigenlatenz). 8 h Musik ohne Knackser oder Drift. Headset aus- und wieder einschalten ohne Absturz, der Ton kommt automatisch zurück. Mikro wird nachweislich nur übertragen, wenn eine Laptop-App es geöffnet hat.

### M4 – Zwischenablage komplett und Dateien (M–L)
- [ ] Formate HTML, RTF, Bilder (PNG/DIBV5), Delayed Rendering mit Timeout
- [ ] M4a: Dateien über Staging-Ordner + `CF_HDROP`, Fortschritts-OSD
- [ ] M4b: Virtuelle Dateien (`IDataObject`, `FILEDESCRIPTOR`/`FILECONTENTS`, Ordner rekursiv)
- [ ] Datei-Transport: Chunks, parallel, BLAKE3, Resume
- [ ] Drop-Zone, „Senden an“, Zielordner, Verlauf
- [ ] Datenschutz-Flags der Zwischenablage respektieren, Größenlimits, Richtungs-Schalter

**Abnahme:** 2-GB-Datei per Strg+C/Strg+V im Explorer mit Explorer-Fortschritt bei ≥ 80 % Link-Speed. Ordner mit 1.000 kleinen Dateien klappt. Screenshot (`Win+Shift+S`) auf dem Laptop lässt sich in Paint/Teams auf dem PC einfügen. Passwort aus dem Passwort-Manager wird **nicht** synchronisiert.

### M5 – Fokusmodus (XL)
- **M5 Spike (zuerst!)** – Machbarkeit 5120×1440@120
  - [ ] Prototyp: Capture → `hevc_nvenc` (RTX 5060) → QUIC → D3D11VA-Decode → Present, ohne UI
  - [ ] Messen: Encode-/Decode-Zeit pro Frame, Glass-to-Glass, Optimus-Adapter-Frage (auf welcher GPU liegt der Desktop?)
  - [ ] ADR mit Ergebnis und Festlegung der Standardwerte
- **M5a – Streaming in nativer Laptop-Auflösung**
  - [ ] DXGI-Duplication-Capture, Cursor-Metadaten
  - [ ] FFmpeg-LGPL-Build mitliefern, HW-Encode (Erkennung NVENC/QSV/AMF), Low-Latency-Settings
  - [ ] Capabilities-Austausch Encoder ↔ Decoder (Codec, 4:4:4, max. Auflösung/FPS)
  - [ ] Video-Transport (Stream pro Frame), Verwerfen, Keyframe-Request
  - [ ] PC: natives Vollbildfenster, D3D11VA-Decode, Flip-Swapchain mit Tearing/VRR, lokaler Cursor, Letterbox-Skalierung
  - [ ] Absolute Input-Abbildung, Hotkey ein/aus, Stats-Overlay
- **M5b – Ultrawide über virtuelles Display**
  - [ ] Virtual Display Driver mitliefern und still installieren (ADR: Version, Steuerung, Signatur, Render-GPU-Wahl)
  - [ ] `DisplayManager`: Modus setzen, Render-GPU = NVIDIA, Topologie sichern/setzen/wiederherstellen, Crash-Recovery
  - [ ] Internes Panel optional aus, Deckel-Automatik, Deckel-Aktion-Option
- **M5c – Split-Modus und Feinschliff**
  - [ ] **Split-Modus**: virtuelles Display in halber/Drittel-Breite, AppBar auf dem PC, Monitormitte als Kante
  - [ ] Adaptive Bitrate, 4:4:4-Option, AV1-Option, Fenster-Modus
  - [ ] Optional: BBR- bzw. Datagramm+FEC-Experiment (ADR)

**Abnahme:** **5120×1440@120** stabil mit Glass-to-Glass p95 < 30 ms (Kabel). Text im Browser und in der IDE gut lesbar. Split-Modus: PC-Fenster maximieren sich nur in ihre Hälfte, die Maus wechselt über die Monitormitte nahtlos. Ein/Aus < 2 s, 20/20 Mal korrekte Topologie danach. Agent-Kill im Fokusmodus führt beim Neustart zu sauber wiederhergestellter Topologie. 1 h Video schauen ohne Aussetzer.

### M6 – Alltagstauglichkeit und Robustheit (L)
- [ ] SYSTEM-Dienst: Agent-Supervisor, Helfer auf dem Secure Desktop (UAC/Sperrbildschirm/Anmeldung), `SendSAS`
- [ ] Watchdog: bei Agent-Absturz Tasten freigeben, neu starten
- [ ] Sleep/Wake- und Netzwechsel-Härtung, Versionsmix-Handling
- [ ] Auto-Update (Tauri-Updater), Diagnosepaket, Crash-Reports lokal
- [ ] Performance: Leerlauf-CPU < 1 %, RAM < 150 MB

**Abnahme:** UAC-Abfrage und Sperrbildschirm des Laptops sind per PC-Tastatur bedienbar. 2 Wochen Alltag ohne Neustart der App. Update von Version x auf x+1 auf beiden Geräten ohne Neu-Pairing.

### M7 – Release-Vorbereitung und Open Source (M)
- [ ] Onboarding-Feinschliff, Englisch-Übersetzung, Barrierefreiheit prüfen
- [ ] Lizenz **MIT OR Apache-2.0** (entschieden, Dateien seit M0 im Repo) prüfen; Drittanbieter-Lizenzen im Über-Dialog (`cargo about`, inkl. FFmpeg-LGPL-Hinweis, OFL, Treiber)
- [ ] README (mit Screenshots/GIF), CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, Issue-Templates
- [ ] Code-Signing aller Binaries und des Installers final (ADR)
- [ ] Projektseite auf softwelop.com

**Abnahme:** Eine fremde Person installiert die App auf zwei Rechnern und ist ohne Hilfe in < 5 min startklar.

### M8+ – Danach
- macOS (CGEventTap/CGEventPost, ScreenCaptureKit, VideoToolbox, Core Audio HAL-Plugin für virtuelles Audio) und Linux (X11: XInput2/XTest; Wayland: libei/Portale, PipeWire für Audio und Capture, uinput)
- Mehr als zwei Geräte
- Drag & Drop über die Bildschirmkante
- Laptop-Touchpad/-Tastatur steuert PC (umgekehrte Richtung)
- Fokusmodus umgekehrt (PC auf Laptop) und mehrere virtuelle Monitore

---

## 16. Risiken und Gegenmaßnahmen

| Risiko | Wahrscheinlichkeit | Auswirkung | Gegenmaßnahme |
|---|---|---|---|
| **Kein fertiger signierter Audio-Treiber passt** → virtuelles Mikro nicht möglich (kein Budget für eigenes EV-Zertifikat) | mittel–hoch | hoch (F3 fällt vorerst weg) | Gründlicher Spike in M3b; Laptop-Ton (F2) funktioniert trotzdem; nach OSS-Release neu prüfen (Sponsoring, neue Treiber) |
| Loopback-Capture plus stumme Laptop-Lautsprecher klappt nicht | mittel | mittel | früh in M3a testen; virtueller Lautsprecher (M3b) löst es sauber |
| Hängende Tasten bei Abbruch | mittel | hoch (nervt extrem) | zuverlässiger Input-Kanal, `release_all` überall, Watchdog, eigener Test |
| 5120×1440@120 überfordert die Encode-/Decode-Kette | mittel | mittel | Spike am Anfang von M5; Fallback 60 fps bzw. Split-Modus mit halber Fläche |
| Optimus: virtuelles Display landet auf der iGPU → Kopie zwischen GPUs | mittel | mittel | Render-GPU im Display-Treiber festlegen; Shared-Texture-Fallback |
| PC-GPU kann HEVC 5120×1440@120 bzw. 4:4:4 nicht decodieren | niedrig–mittel | mittel | Capabilities-Check beim Start, automatische Rückfallwerte |
| Funk-Headset-Dongle verschwindet und taucht wieder auf (Ausschalten) | hoch | niedrig | Hot-Plug-Handling, automatisch fortsetzen, kein heimlicher Gerätewechsel |
| Display-Topologie wird nicht sauber wiederhergestellt | mittel | hoch | Topologie auf Platte sichern, Recovery beim Start, Notfall-Hotkey |
| Windows-Updates ändern Verhalten (Hooks, Audio) | niedrig | mittel | Plattform-Schicht gekapselt, Soak-Tests nach Updates |
| FFmpeg-Lizenzfalle (versehentlich GPL-Build) | niedrig | mittel | Build-Flags in der CI prüfen (`ffmpeg -L`), Hash-Pinning |
| Scope-Creep / Motivation | mittel | hoch | Meilensteine mit Alltagsnutzen; nach M2 schon nützlich |

---

## 17. Vorbereitung für macOS/Linux (schon in v1 beachten)
- Keine Windows-Typen außerhalb von `platform-windows`.
- Scancodes intern als **USB-HID-Usage-Codes** führen und pro OS mappen (Windows-Scancode ↔ HID ↔ macOS-Keycode ↔ evdev). Das von Anfang an so machen, das spätere Umbauen wäre teuer.
- Koordinaten immer physische Pixel + DPI mitschicken.
- Audio intern immer f32/48 kHz.
- Clipboard-Formate intern als MIME-Typen (`text/plain`, `text/html`, `image/png`, `text/uri-list`) und pro OS mappen.

---

## 18. Entscheidungen

### Entschieden (2026-10-16)
| Thema | Entscheidung |
|---|---|
| Produktname | **Desk Bridge** |
| App-Icon | eigenes Produkt-Icon im softwelop-Stil (9.1) |
| Schrift | nur Montserrat (Gilmer entfällt) |
| UI-Typo | Medium/Regular für kleine Labels, Lesbarkeit vor Brand-Purismus |
| Laptop-GPU | NVIDIA RTX 5060 Laptop (vermutlich; wird zur Laufzeit erkannt) → NVENC |
| Monitor | 5120×1440 @ 120 Hz |
| Netz | beide per LAN-Kabel |
| Headset | Funk mit USB-Dongle am PC |
| PC-GPU | NVIDIA RTX 30/40/50 → AV1- und HEVC-4:4:4-Decoding möglich |
| Audio-Treiber | nur fertiger, signierter OSS-Treiber (Spike mit Kriterien in 8.9); **kein Budget für EV-Zertifikat**, eigener Treiber verschoben |
| Code-Signing | Eigengebrauch unsigniert, ab OSS-Release SignPath Foundation (kostenlos) |
| Split-Modus | **ja**, in M5c |
| Lizenz | MIT OR Apache-2.0 |
| Installation | ein Installer bringt alles mit, null Handarbeit (12.1) |
