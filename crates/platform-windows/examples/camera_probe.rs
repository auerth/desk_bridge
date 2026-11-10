//! Manual test of the webcam forwarding pieces on one machine.
//!
//! ```text
//! camera_probe list                     webcams
//! camera_probe webcam [secs]            read the first webcam (1280x720@30), report fps
//! camera_probe register <dll>           COM class under HKLM (administrator)
//! camera_probe install <dll>            COM class + machine-wide camera, enabled (administrator)
//! camera_probe uninstall                remove both (administrator)
//! camera_probe vcam [secs]              per-user test camera: synthetic pictures in, read back
//!                                       through the Frame Server, check the colours
//! camera_probe forward [secs]           webcam -> "Desk Bridge Kamera" live (open the Camera app)
//! camera_probe loop [secs] [png]        webcam -> per-user test camera -> read back through the
//!                                       Frame Server; saves picture 60 as PNG
//! ```

#[cfg(windows)]
fn main() {
    if let Err(e) = imp::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod imp {
    use deskbridge_camera::shm::SharedCamera;
    use deskbridge_camera::{WriterState, nv12, tick_ms};
    use deskbridge_platform::camera::{CameraCapture, CameraFeedState, VirtualCamera};
    use deskbridge_platform::video::{Frame, FrameData};
    use deskbridge_platform_windows::camera::{WinCameraCapture, WinVirtualCamera, register};
    use std::time::{Duration, Instant};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    type R<T> = Result<T, Box<dyn std::error::Error>>;

    pub fn run() -> R<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let secs = |i: usize| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(5u64);
        match args.first().map(String::as_str) {
            Some("list") => {
                for d in WinCameraCapture.list()? {
                    println!("{}  [{}]", d.name, d.id);
                }
            }
            Some("webcam") => webcam(secs(1))?,
            Some("register") => {
                let dll = std::path::absolute(args.get(1).ok_or("dll path missing")?)?;
                register::register_com(&dll)?;
                println!("registered {}", dll.display());
            }
            Some("install") => {
                let dll = std::path::absolute(args.get(1).ok_or("dll path missing")?)?;
                register::install(&dll)?;
                println!("installed {}", dll.display());
            }
            Some("uninstall") => {
                register::uninstall()?;
                println!("removed");
            }
            Some("vcam") => vcam(secs(1))?,
            Some("forward") => forward(secs(1), false, None)?,
            // like `forward`, plus a per-user test camera read back here; optional PNG path
            Some("loop") => forward(secs(1), true, args.get(2).cloned())?,
            _ => println!(
                "usage: camera_probe list|webcam|register <dll>|install <dll>|uninstall|vcam|forward"
            ),
        }
        Ok(())
    }

    fn webcam(secs: u64) -> R<()> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let h = WinCameraCapture.start(None, 1280, 720, 30, tx)?;
        let t0 = Instant::now();
        let mut n = 0u32;
        let mut first: Option<Instant> = None;
        while t0.elapsed() < Duration::from_secs(secs) {
            match rx.try_recv() {
                Ok(f) => {
                    first.get_or_insert_with(Instant::now);
                    n += 1;
                    if n == 1 {
                        let FrameData::Bgra { data, stride } = &f.data else {
                            unreachable!()
                        };
                        let avg: u64 = data.iter().step_by(97).map(|&b| u64::from(b)).sum::<u64>()
                            / (data.len() / 97).max(1) as u64;
                        println!(
                            "first picture {}x{} stride {stride}, mean byte {avg}, after {:?}",
                            f.width,
                            f.height,
                            t0.elapsed()
                        );
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        drop(h);
        let el = first.map_or(0.0, |f| f.elapsed().as_secs_f32());
        println!(
            "{n} pictures, {:.1} fps",
            f64::from(n) / f64::from(el.max(0.001))
        );
        Ok(())
    }

    /// The colour of picture `i`: changes every 15 pictures.
    fn colour(i: u64) -> (u8, u8, u8) {
        [(220, 30, 30), (30, 200, 30), (30, 30, 220)][((i / 15) % 3) as usize]
    }

    fn vcam(secs: u64) -> R<()> {
        // SAFETY: COM for this thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        println!("COM class -> {:?}", register::registered_dll());
        let cam = register::VirtualCameraDevice::open(false)?;
        cam.start()?;
        println!("per-user test camera started");
        // SAFETY: plain COM calls.
        let source: IMFMediaSource = unsafe { cam.raw().GetMediaSource()? };
        println!("media source from the Frame Server");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let st = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut shm = None;
            let mut i = 0u64;
            let mut log_pos = 0u64;
            while !st.load(std::sync::atomic::Ordering::SeqCst) {
                if shm.is_none() {
                    shm = SharedCamera::open_or_create().ok();
                    if shm.is_some() {
                        println!("writer: shared memory open");
                    }
                }
                if let Some(s) = &shm {
                    let ring = s.ring();
                    let (t, p) = ring.log_since(log_pos);
                    log_pos = p;
                    for l in t.lines() {
                        println!("  vcam: {l}");
                    }
                    let req = ring.reader_request(tick_ms());
                    let (w, h) = req.map_or((1280, 720), |r| (r.width, r.height));
                    ring.set_writer_state(WriterState::Streaming, tick_ms());
                    let c = colour(i);
                    ring.write_frame(w, h, 0, tick_ms(), |d| nv12::fill_rgb(d, w, h, c));
                    i += 1;
                }
                std::thread::sleep(Duration::from_millis(33));
            }
        });
        // SAFETY: plain Media Foundation calls.
        let result = unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let mt = reader.GetCurrentMediaType(stream)?;
            let size = mt.GetUINT64(&MF_MT_FRAME_SIZE)?;
            println!("current type {}x{}", size >> 32, size as u32);
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(stream, None, &out)?;
            let t0 = Instant::now();
            let mut n = 0u32;
            let mut seen = std::collections::BTreeSet::new();
            while t0.elapsed() < Duration::from_secs(secs) {
                let mut flags = 0;
                let mut s = None;
                reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut s))?;
                let Some(s) = s else { continue };
                n += 1;
                let b = s.ConvertToContiguousBuffer()?;
                let mut p = std::ptr::null_mut();
                let mut len = 0;
                b.Lock(&mut p, None, Some(&mut len))?;
                let mid = std::slice::from_raw_parts(p.add(len as usize / 2), 4);
                let px = (mid[2], mid[1], mid[0]);
                b.Unlock()?;
                let name = match px {
                    (r, g, b) if r > 150 && g < 90 && b < 90 => "red",
                    (r, g, b) if g > 150 && r < 90 && b < 90 => "green",
                    (r, g, b) if b > 150 && r < 90 && g < 90 => "blue",
                    _ => "other",
                };
                if seen.insert(name) {
                    println!("sample {n}: centre pixel {px:?} = {name}");
                }
            }
            println!(
                "{n} samples in {secs} s ({:.1} fps), colours seen: {seen:?}",
                f64::from(n) / secs as f64
            );
            Ok::<_, windows::core::Error>(())
        };
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = writer.join();
        let _ = unsafe { source.Shutdown() };
        let _ = cam.remove();
        result?;
        Ok(())
    }

    /// Reads the per-user test camera through the Frame Server like an app would; saves the
    /// 60th picture as PNG and counts placeholder-coloured pictures.
    fn self_reader(
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        png_path: Option<String>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let run = || -> R<()> {
                // SAFETY: COM for this thread; plain Media Foundation calls below.
                unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
                let cam = register::VirtualCameraDevice::open(false)?;
                cam.start()?;
                let source: IMFMediaSource = unsafe { cam.raw().GetMediaSource()? };
                unsafe {
                    let mut attrs = None;
                    MFCreateAttributes(&mut attrs, 1)?;
                    let attrs = attrs.unwrap();
                    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
                    let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)?;
                    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
                    let out = MFCreateMediaType()?;
                    out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
                    reader.SetCurrentMediaType(stream, None, &out)?;
                    let size = reader.GetCurrentMediaType(stream)?.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    let (w, h) = ((size >> 32) as usize, (size as u32) as usize);
                    let t0 = Instant::now();
                    let (mut n, mut placeholder) = (0u32, 0u32);
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        let mut s = None;
                        let mut flags = 0u32;
                        reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut s))?;
                        let Some(s) = s else { continue };
                        n += 1;
                        let b = s.ConvertToContiguousBuffer()?;
                        let mut p = std::ptr::null_mut();
                        let mut len = 0;
                        b.Lock(&mut p, None, Some(&mut len))?;
                        let px = std::slice::from_raw_parts(p, len as usize);
                        // the placeholder's corner is the brand background
                        let c = (px[2], px[1], px[0]);
                        if c.0.abs_diff(0x1D) < 8 && c.1.abs_diff(0x2B) < 8 && c.2.abs_diff(0x4F) < 8 {
                            placeholder += 1;
                        }
                        if n == 60
                            && let Some(path) = &png_path
                            && px.len() >= w * h * 4
                        {
                            // RGB32 from the source reader is bottom-up.
                            let mut rgb = Vec::with_capacity(w * h * 3);
                            for y in (0..h).rev() {
                                for x in 0..w {
                                    let o = (y * w + x) * 4;
                                    rgb.extend_from_slice(&[px[o + 2], px[o + 1], px[o]]);
                                }
                            }
                            let f = std::fs::File::create(path)?;
                            let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
                            e.set_color(png::ColorType::Rgb);
                            e.write_header()?.write_image_data(&rgb)?;
                            println!("reader: saved picture {n} ({w}x{h}) to {path}");
                        }
                        b.Unlock()?;
                    }
                    let secs = t0.elapsed().as_secs_f64();
                    println!(
                        "reader: {n} pictures in {secs:.1} s ({:.1} fps), {placeholder} placeholder",
                        f64::from(n) / secs
                    );
                    let _ = source.Shutdown();
                }
                let _ = cam.remove();
                Ok(())
            };
            if let Err(e) = run() {
                println!("reader error: {e}");
            }
        })
    }

    fn forward(secs: u64, self_read: bool, png_path: Option<String>) -> R<()> {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = self_read.then(|| self_reader(stop.clone(), png_path));
        let vc = WinVirtualCamera::new();
        println!(
            "virtual camera available: {} {:?}",
            vc.available(),
            vc.unavailable_reason()
        );
        let (utx, mut urx) = tokio::sync::mpsc::unbounded_channel();
        vc.watch_usage(utx)?;
        vc.set_feed_state(CameraFeedState::Waiting);
        let mut handle = None;
        let (ftx, mut frx) = tokio::sync::mpsc::unbounded_channel::<Frame>();
        let t0 = Instant::now();
        let mut n = 0u64;
        while t0.elapsed() < Duration::from_secs(secs) {
            while let Ok(u) = urx.try_recv() {
                println!("usage: {u:?}");
                if u.in_use && handle.is_none() {
                    handle = Some(WinCameraCapture.start(
                        None,
                        u.width,
                        u.height,
                        u.fps,
                        ftx.clone(),
                    )?);
                } else if !u.in_use {
                    handle = None;
                    vc.set_feed_state(CameraFeedState::Waiting);
                }
            }
            match frx.try_recv() {
                Ok(f) => {
                    vc.set_feed_state(CameraFeedState::Streaming);
                    vc.write_frame(&f);
                    n += 1;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(3)),
            }
        }
        println!("{n} pictures forwarded");
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(r) = reader {
            let _ = r.join();
        }
        Ok(())
    }
}
