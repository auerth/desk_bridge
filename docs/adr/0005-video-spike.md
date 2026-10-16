# ADR 0005 – Video spike: capture → NVENC → framing → D3D11VA → present at 5120×1440@120 (M5)

* Status: **Proposed** (measured on the owner's PC; the laptop leg is still untested)
* Date: 2026-10-16
* Scope: plan 8.7, 3, 12.1, 16. Code: `crates/video`, `crates/platform-windows/src/video/`, `tools/video-spike`.

## Zusammenfassung für Thorben (Deutsch)

**Ergebnis: 5120×1440@120 Hz ist machbar.** Auf deinem PC (RTX 3070 Ti) läuft die ganze Kette Capture-Textur → `hevc_nvenc` → Paketierung →
D3D11VA-Decode → Fenster mit 120 fps stabil (0 verworfene Frames in 30 s, 3600 Frames). Die Kette ist **zero-copy** (die Textur bleibt auf der GPU).
Mit Preset **p1** braucht der Encoder ca. **4,5 ms** pro Frame (Budget 8,3 ms), die Gesamtzeit „Bild erzeugt → decodiert → an `Present` übergeben“
liegt bei **p50 4,8 ms / p99 5,3 ms** (ohne Monitor-Eigenlatenz und ohne die ~2 ms GPU-Zeit für Farbumrechnung und Skalierung im Fenster).
Das Ziel „Glass-to-Glass p95 < 30 ms“ ist damit auf dem Kabel sehr komfortabel erreichbar, **sofern** die Laptop-GPU (RTX 5060, NVENC der 8. Generation)
mindestens so schnell ist wie die 3070 Ti – das ist nicht gemessen.

Wichtige Erkenntnisse:

* **Standard-Codec HEVC**, Preset **p1**, Tune `ull`, CBR, VBV ≈ 1 Frame, 120 Mbit/s Start. **p2 ist zu knapp** (7,7 ms von 8,3 ms), p3/p4 schaffen 120 fps nicht.
* **AV1 konnte nicht gemessen werden:** Die RTX 3070 Ti (und die AMD-iGPU) kann kein AV1 *encodieren*. Der AV1-*Decoder* der 3070 Ti schafft 5120×1440 in ~2 ms/Frame.
  Ob AV1 auf der RTX 5060 Standard wird, entscheidet erst ein Lauf auf dem Laptop (`video-spike --codec av1`).
* **H.264 geht nicht bei 5120 Pixeln Breite** (NVENC-Limit 4096). Nur als Rückfallebene bei kleineren Bildern.
* **FFmpeg 8.1 statt 7.1** (der 7.1-LGPL-Build hat kein `av1_nvenc`), LGPL (v3), dynamisch gelinkt, 124 MB DLLs.
* **4:4:4 geht in dieser Pipeline nicht** (D3D11-Frames kennen kein 4:4:4, FFmpegs D3D11VA decodiert kein 4:4:4). Die Option bleibt im Code, wird aber nicht angeboten.
* **Echte Desktop-Aufnahme** deines Hauptmonitors war auf diesem stark ausgelasteten PC langsam (~17 fps); das ist hier nicht aussagekräftig (siehe „Offene Punkte“) und muss auf dem Laptop mit dem virtuellen Display neu gemessen werden.

## Setup of the measurements

* PC: AMD Ryzen 9 9950X, NVIDIA GeForce RTX 3070 Ti (driver 32.0.16.1088) driving one 5120×1440 monitor at ~143 Hz, AMD Radeon iGPU (idle), Windows 11 26200. The machine was in normal use
  (browser with video etc.; `nvidia-smi` showed other encoder/decoder load) – numbers below are therefore not lab-grade; tails (p99/max) in particular.
* Tool: `target\release\video-spike.exe` (see "How to run"). FFmpeg 8.1.3 LGPL shared build (`vendor/ffmpeg/SOURCE.txt`), `ffmpeg-next` 8.1 raw FFI.
* Loopback on **one machine, one GPU**: a second D3D11 device on the same adapter plays the viewing PC. No network (a `tokio::io::duplex` stream per frame carries the real `write_frame`/`read_frame`
  framing), no jitter, no laptop dGPU/iGPU interplay (Optimus), and encode, decode and the synthetic picture generator compete for the *same* GPU, which a real two-machine setup does not.
* Picture source: `--synthetic` – a GPU-rendered worst case (full-frame horizontally scrolling sharp "text" band + gradients + a fast box; every pixel of the band changes every frame). Real desktop content is mostly easier.
* Clock: `QueryPerformanceCounter` for every stage; capture timestamp = moment the picture was submitted, so "glass-to-glass" below is *capture timestamp → decoded / `Present` returned*,
  **without** monitor scan-out and **without** GPU execution time of the final colour conversion/scaling (measured separately: ~1.7 ms for NV12→BGRA + scaled draw, run unthrottled at ~500 fps).
* First 30 frames of every run are excluded (encoder open, clock ramp). Runs of 20–30 s.

## Results

All HEVC runs `hevc_nvenc`, **zero-copy path** (capture BGRA texture → D3D11 video processor (BT.709 limited NV12) → FFmpeg D3D11 hw frame → NVENC), `tune=ull`, `rc=cbr`, `zerolatency`, `delay=0`, no B-frames,
no lookahead, infinite GOP (IDR only on request/at start), VBV = 1 frame. Decoder: `hevc` + D3D11VA on the same GPU. "frames" = decoded frames out of produced frames.

### Main case: HEVC 5120×1440@120, 120 Mbit/s target, preset p1 (30 s, window presenter for 6 s)

| stage | n | mean ms | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|---:|
| capture → encode submit (queue) | 3567 | 0.01 | 0.01 | 0.02 | 0.02 | 0.14 |
| colour convert BGRA→NV12 (CPU submit; GPU time is inside "encode") | 3567 | 0.09 | 0.08 | 0.15 | 0.22 | 0.50 |
| **encode** (`send_frame`…packet) | 3567 | 4.48 | 4.48 | 4.59 | 4.74 | 6.88 |
| packetize (`write_frame` + `read_frame`) | 3567 | ~0 | 0.01 | 0.02 | 0.03 | – |
| decode, CPU submit | 3567 | 0.17 | 0.15 | 0.27 | 0.34 | 1.54 |
| decode incl. GPU completion | 3567 | 0.30 | 0.30 | 0.47 | 0.57 | 1.71 |
| present call (CPU submit) | 685 | 0.06 | 0.05 | 0.09 | 0.11 | 0.18 |
| **capture → decoded** | 3567 | 4.91 | 4.91 | 5.16 | 5.30 | 11.06 |
| **capture → presented (Present returned)** | 685 | 4.77 | 4.75 | 5.03 | 5.24 | 5.44 |
| interval between presents | 685 | 8.33 | 8.33 | 8.58 | 8.74 | 8.85 |

120.0 fps sustained, 0 receiver-filter drops, 4 of 3601 pictures dropped at the source queue (producer jitter), 2 encode-deadline misses, 89 Mbit/s average (CBR undershoots because the content is cheap),
one key frame (first). Picture check: with a generous VBV the first decoded frame matches the source luma grid (BT.709 limited) with **mean abs error 0.32** → colour matrix and range are right;
with the 1-frame VBV the very first IDR of this text-heavy worst case is bit-starved (mean error ~11; subsequent frames refine) – see "IDR size".

### Other configurations (20 s each, synthetic)

| configuration | encode ms p50 / p99 | capture→decoded ms p50 / p99 | fps | Mbit/s avg | verdict |
|---|---|---|---:|---:|---|
| HEVC 5120×1440@120 **p1** | 4.48 / 4.74 | 4.9 / 5.3 | 119.8 | 89 | **recommended** |
| HEVC 5120×1440@120 p1, `tune=ll` | 4.50 / 4.67 | 4.8 / 5.0 | 119.8 | 91 | same as `ull` |
| HEVC 5120×1440@120 p1, VBV 2 frames | 4.38 / 4.62 | 4.8 / 5.1 | 119.8 | 108 | bigger IDR budget, +20 % bitrate |
| HEVC 5120×1440@120 p1, intra-refresh | 4.42 / 4.68 | 4.9 / 5.2 | 119.8 | 90 | works, no timing change |
| HEVC 5120×1440@144 p1 | 4.69 / 5.13 | 5.2 / 6.0 | 143.8 | 73 | headroom for the monitor's 144 Hz |
| HEVC 5120×1440@120 **p2** | 7.67 / 7.84 | 8.0 / 8.4 | 119.8 | 90 | 0.3–0.5 % deadline misses → too tight |
| HEVC 5120×1440@120 p3 | 8.87 / 9.04 | 18 / 27 (queueing) | 110 | 61 | cannot hold 120 fps |
| HEVC 5120×1440@120 p4 | 10.6 / 11.1 | 32 / 34 | 92 | 49 | cannot hold 120 fps |
| HEVC 5120×1440@60 p1 | 7.9 / 11.7 | 8.5 / 26 | 59.9 | 14 | slower per frame than at 120 fps (GPU low-power state, see below) |
| HEVC 2560×1440@120 p1 (split mode) | 4.3 / 5.9 | 5.0 / – | 119.8 | 46 | fine (two of three 2560 runs showed multi-100 ms stalls in the *decode/GPU-sync* stage, one clean run: GPU time-slicing against other desktop load, see below) |
| H.264 4096×1152@120 p1 (reference) | 3.87 / 4.29 | 4.3 / 4.8 | 119.8 | 120 | works; needs ~35 % more bits; **5120 wide is impossible** (NVENC H.264 limit 4096) |
| HEVC 5120×1440@60 p1, **read-back fallback** | 5.2 / 9.5 | 8.4 / 13.7 | 59.6 | 14 | +2.8 ms for GPU→CPU NV12 read-back; at 120 fps it cannot keep up (88 fps) |
| **AV1 5120×1440 hardware encode** | – | – | – | – | **not available**: `av1_nvenc` → "Codec not supported" on RTX 3070 Ti (needs Ada/Blackwell); AMD iGPU has no AV1 encode either |
| AV1 5120×1440 **decode** only (SVT-AV1 test stream, 8.8 Mbit/s) | – | decode 2.0 / 3.8 GPU | 473 (unthrottled) | – | D3D11VA AV1 decode keeps up easily (low-bitrate stream; high-bitrate cost not measured) |
| HEVC decode only, unthrottled (61 Mbit/s, 5120×1440) | – | decode ~1.9 GPU | 506 incl. present | – | decode + NV12→BGRA + scaled draw ≈ 2 ms per frame |

**Presenter** (D3D11 flip model, `ALLOW_TEARING`, waitable object, VP NV12→BGRA, letterboxed bilinear draw): window, fullscreen (verified full 5120×1440 coverage, taskbar covered) and
region+AppBar modes ran; screenshots confirmed picture, stats-overlay text and the remote-cursor sprite. AppBar: PC work area shrank from `0,0–5120,1392` to `0,0–2560,1392` while the
right-half region window was open and was restored on close. Present throughput without sync ≈ 500 fps.
`Present` return time (CPU) is 0.05 ms; the display-side latency (≤ 1 refresh at 143 Hz with tearing, independent flip in fullscreen) is **not** included above and cannot be measured in software.

**Bitrate control:** `set_bitrate` takes effect in place, no IDR/reopen: a step 120→40 Mbit/s at t=10 s moved the stream from ~90 to ~35 Mbit/s within the next second (`--bitrate-step 10:40`).

### Optimus / adapter notes

* Zero-copy needs the encoder device = the device that owns the capture texture (`GpuFrame.device`). The encoder therefore opens **lazily on the first frame's device**; `VideoBackend::create_encoder` only gets the adapter *name*.
* `DuplicateOutput` only works on the adapter that owns the output. On an Optimus laptop the virtual (IddCx) display must render on the NVIDIA GPU (ADR 0004 recommends selecting it by name); capturing the iGPU panel with a dGPU device fails.
  Not testable here (this PC's monitor hangs on the NVIDIA GPU; a third adapter that also reports as the 3070 Ti is probably the Parsec virtual display adapter that is installed on this PC).
* Encoder candidates are restricted by PCI vendor (NVIDIA→`*_nvenc`, Intel→`*_qsv`, AMD→`*_amf`); AMF "opens" on foreign GPUs and fails later, so unrestricted fallback gives false positives.
  The AMD iGPU of this PC opened `hevc_amf`/`h264_amf` (not run). QSV was not testable (no Intel GPU).

## Decision / recommended defaults

| setting | value | why |
|---|---|---|
| codec (5120×1440@120) | **HEVC** (`hevc_nvenc`); AV1 as user option only where `av1_nvenc` really opens | AV1 encode cannot be proven on this hardware; H.264 impossible above 4096 px |
| NVENC preset / tune | **`p1`** / `ull` (`ll` identical), `rc=cbr`, `zerolatency=1`, `delay=0`, `forced-idr=1`, no B-frames, no lookahead, no AQ, `multipass=disabled`, `g=-1` (infinite GOP) | p2 leaves 0.6 ms of 8.3 ms; p3+ cannot do 120 fps on a 7th-gen NVENC. Re-test p2 on the RTX 5060 |
| VBV | 1 frame (`rc_buffer_size = bitrate/fps`) | lowest latency; IDRs are quality-starved (below) |
| start bitrate | 120 Mbit/s wired HEVC at 5120×1440@120 (scales with pixel rate; AV1 ×0.7, H.264 ×1.5 – `negotiate::default_bitrate_kbps`); ABR range 40–300 Mbit/s wired, 8–80 Mbit/s Wi-Fi | actual use was ~90 Mbit/s on hard content, so 120 is a safe ceiling |
| fps fallbacks | 120 → 60 (`AbrDecision::suggest_fps` when encode/decode time > 95 % of the budget); 2560×1440 split mode stays 120 | 5120×1440@144 also held (4.7 ms) if the monitor runs 144 Hz |
| H.264 | only ≤ 4096 px per side (enforced in `negotiate`) | NVENC/D3D11VA limit |
| 4:4:4 | not offered (`encode_yuv444 = decode_yuv444 = false` in the probes) | D3D11 hw frames have no 4:4:4 format; FFmpeg D3D11VA cannot decode it although NVIDIA exposes the RExt profile GUIDs (`hevc_rext_444_profile=true` in `--probe`). Needs a CUDA/CUVID frame path (future ADR) |
| encoder input | zero-copy only for NVENC; **read-back fallback for AMF/QSV and CPU frames** | fallback costs ~2.8 ms/frame at 5120×1440 (viable ≤ 60 fps) |
| decode | D3D11VA, bind flags `DECODER|SHADER_RESOURCE`, `extra_hw_frames=4`, software fallback (BGRA) | |
| bundled FFmpeg DLLs | `avcodec-62, avformat-62, avutil-60, swscale-9, swresample-6` (~124 MB); `avfilter`/`avdevice` not needed | a slim LGPL build with only NVENC/AMF/QSV encoders and D3D11VA/dav1d decoders would be ~10× smaller (later) |

## Consequences and engine requirements

* **GPU power state matters:** at 60 fps (lighter load) NVENC latency per frame was *higher* (8–10 ms vs 4.5 ms at 120 fps) because the GPU dropped to a low-power state (630 MHz observed). The encoder thread must
  therefore not be latency-tuned on idle benchmarks; request "prefer maximum performance" for the Desk Bridge process (Windows Graphics Settings / NVIDIA profile) or accept that the GPU ramps with load.
* **Never let a slow decode/present stall the receive path.** With the GPU time-sliced against other desktop load we observed periodic ~16 ms stalls and, in two 2560×1440 runs, backlogs of up to 1.3 s.
  The viewer must decode every frame (references!) but **present only the newest decoded one**, and use `LateFrameFilter::with_max_age_us` to request a keyframe if it falls far behind.
* **IDR size:** with a 1-frame VBV an IDR is quantised hard. After `KeyframeRequest` (rate limited to one per 250 ms by `pacing::IdrThrottle`) the picture is soft for ~1–2 frames. A VBV of 2 frames costs ~20 % bitrate (108 vs 90 Mbit/s here) and
  mostly helps the first frame; intra-refresh (`EncoderOptions::intra_refresh`) works and is an option for lossy links (not measured for recovery behaviour).
* **Frame ids / uni streams:** a lost or late frame stream breaks the reference chain; `LateFrameFilter` drops delta frames until a keyframe arrives and requests one (tested, including 32-bit wrap-around).
* NV12 texture arrays can only be created with `D3D11_BIND_DECODER`; the encoder pool uses `initial_pool_size = 0` (one texture per frame, recycled by FFmpeg's buffer pool).

## Open points / risks

1. **Real desktop capture is unproven for performance.** `--capture` (DXGI Desktop Duplication of this PC's primary 5120×1440 output) produced 100+ source fps but the downstream stages ran at ~17–20 fps
   (VP submit 8–13 ms, NVENC 35–40 ms per frame, copy completion ~60 ms) while the GPU was shared with other desktop applications (other NVENC/NVDEC users visible in `nvidia-smi`, DWM at 143 Hz); the same GPU did 120 fps with the synthetic source minutes earlier.
   Cause not isolated (`DB_NOENC=1`, i.e. capture without encoding, shows the GPU completing the desktop copy ~60 ms after the frame time). **Must be re-measured on the laptop with the virtual display** (no other load on that GPU).
   Functionally the capture→encode→decode path worked (pointer shape/position are delivered separately; ACCESS_LOST re-creation is implemented but not provoked).
2. Optimus (iGPU panel + NVIDIA virtual display), IddCx output capture, AV1 on RTX 5060, QSV/AMF zero-copy and the AMF/QSV encoders were **not** exercised.
3. 4:4:4 requires a different frame path.
4. The loopback shares one GPU between encode and decode; a real pair splits the load, so the end-to-end number on two machines should be *better*, plus network time (LAN: < 0.5 ms) and display scan-out.
5. `av1_nvenc` is missing in the BtbN 7.1 LGPL build; this ADR pins the 8.1.3 build. `ffmpeg-sys-next` 7.1 also fails with libclang 22 (opaque `FF_PAD_STRUCTURE` structs), 8.1 works. The rolling `latest` URL is not stable: archive the zip with the hash in `vendor/ffmpeg/SOURCE.txt`.

## How to run (owner)

```powershell
# one-time: FFmpeg DLLs are copied next to the exe by tools/video-spike/build.rs (vendor/ffmpeg must exist)
cargo build --release -p video-spike
.\target\release\video-spike.exe --probe                                   # encoders/decoders/outputs + negotiate
.\target\release\video-spike.exe --synthetic --codec hevc --fps 120 --bitrate 120 --preset p1 --seconds 30 --present=6 --verify
.\target\release\video-spike.exe --synthetic --codec av1  --fps 120 --bitrate 80        # laptop (RTX 5060): AV1 hardware encode
.\target\release\video-spike.exe --synthetic --codec hevc --readback --fps 60            # cost of the fallback path
.\target\release\video-spike.exe --capture --seconds 8                                   # real primary output (<= 10 s)
.\target\release\video-spike.exe --synthetic --seconds 12 --present=5 --present-mode region      # split-mode window + AppBar
.\target\release\video-spike.exe --codec av1 --encoder-name libsvtav1 --decode-bench 180 --fps 60   # AV1 decode benchmark with a software-made stream
```

Report the markdown tables that are printed; they paste straight into this ADR. Diagnostics: `--nv12-test`, `--present-bench`, env `DB_NOENC=1`, `DB_NOSYNC=1`.
