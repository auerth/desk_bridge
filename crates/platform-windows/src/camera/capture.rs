//! PC side: the webcam through a Media Foundation source reader (shared with other apps through
//! the Frame Server), converted to BGRA by the reader.

use super::ComGuard;
use deskbridge_camera::VCAM_FRIENDLY_NAME;
use deskbridge_platform::audio::StreamHandle;
use deskbridge_platform::camera::{CameraCapture, CameraDeviceInfo};
use deskbridge_platform::video::{Frame, FrameData};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, Interface, PWSTR};

fn err(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::other(format!("{what}: {e}"))
}

fn mf_startup() -> Result<()> {
    // SAFETY: may be called repeatedly (reference counted).
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| err("MFStartup", e))
}

fn string_attr(a: &IMFActivate, key: &GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: the string is allocated by Media Foundation and freed below.
    unsafe {
        a.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0.cast()));
        s
    }
}

/// All video capture devices (activation objects), ours excluded.
fn devices() -> Result<Vec<(CameraDeviceInfo, IMFActivate)>> {
    mf_startup()?;
    // SAFETY: plain Media Foundation enumeration; the array is freed after taking the items.
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| err("MFCreateAttributes", e))?;
        let attrs = attrs.ok_or_else(|| PlatformError::other("MFCreateAttributes"))?;
        attrs
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .map_err(|e| err("SetGUID", e))?;
        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFEnumDeviceSources(&attrs, &mut arr, &mut n).map_err(|e| err("MFEnumDeviceSources", e))?;
        let mut out = Vec::new();
        for i in 0..n as usize {
            if let Some(a) = (*arr.add(i)).take() {
                let name =
                    string_attr(&a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
                let id = string_attr(&a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)
                    .unwrap_or_default();
                if name == VCAM_FRIENDLY_NAME {
                    continue;
                }
                out.push((CameraDeviceInfo { id, name }, a));
            }
        }
        CoTaskMemFree(Some(arr.cast()));
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy)]
struct Native {
    index: u32,
    width: u32,
    height: u32,
    fps: f32,
    subtype: GUID,
}

fn natives(reader: &IMFSourceReader) -> Vec<Native> {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let mut v = Vec::new();
    for index in 0.. {
        // SAFETY: plain COM getters.
        let Ok(mt) = (unsafe { reader.GetNativeMediaType(stream, index) }) else {
            break;
        };
        let (Ok(size), subtype) = (
            unsafe { mt.GetUINT64(&MF_MT_FRAME_SIZE) },
            unsafe { mt.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default(),
        ) else {
            continue;
        };
        let rate = unsafe { mt.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or((30u64 << 32) | 1);
        let (num, den) = ((rate >> 32) as f32, (rate as u32).max(1) as f32);
        v.push(Native {
            index,
            width: (size >> 32) as u32,
            height: size as u32,
            fps: num / den,
            subtype,
        });
    }
    v
}

/// The native format closest to what the app on the laptop asked for: same aspect ratio, not
/// smaller than asked (else as large as possible), at least the asked rate, then uncompressed
/// before MJPEG (no decoder), then the smallest that fits.
fn pick(list: &[Native], w: u32, h: u32, fps: u32) -> Option<Native> {
    let want_aspect = w as f32 / h.max(1) as f32;
    list.iter().copied().min_by_key(|n| {
        let aspect = n.width as f32 / n.height.max(1) as f32;
        let aspect_off = ((aspect - want_aspect).abs() * 100.0) as u32;
        let too_small = u32::from(n.width < w || n.height < h);
        let slow = u32::from(n.fps + 0.5 < fps as f32);
        let compressed = u32::from(n.subtype == MFVideoFormat_MJPG);
        let area_off =
            (i64::from(n.width) * i64::from(n.height) - i64::from(w) * i64::from(h)).unsigned_abs();
        (
            u32::from(aspect_off > 2),
            too_small,
            slow,
            compressed,
            area_off,
        )
    })
}

struct Handle {
    stop: Arc<AtomicBool>,
}

impl StreamHandle for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // The reader thread notices within one picture; it is not joined (a hanging driver
        // must not block the engine).
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// `CameraCapture` with Media Foundation.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinCameraCapture;

impl CameraCapture for WinCameraCapture {
    fn list(&self) -> Result<Vec<CameraDeviceInfo>> {
        let _com = ComGuard::init();
        Ok(devices()?.into_iter().map(|(i, _)| i).collect())
    }

    fn start(
        &self,
        device: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        sink: UnboundedSender<Frame>,
    ) -> Result<Box<dyn StreamHandle>> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String>>();
        let device = device.map(str::to_string);
        let st = stop.clone();
        std::thread::Builder::new()
            .name("webcam".into())
            .spawn(move || {
                let _com = ComGuard::init();
                let reader = match open(device.as_deref(), width.max(2), height.max(2), fps.max(1))
                {
                    Ok((r, desc)) => {
                        let _ = ready_tx.send(Ok(desc));
                        r
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                read_loop(&reader, &st, &sink);
            })
            .map_err(|e| PlatformError::other(format!("webcam thread: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(desc)) => {
                tracing::info!("webcam open: {desc}");
                Ok(Box::new(Handle { stop }))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(PlatformError::other("Die Webcam reagiert nicht"))
            }
        }
    }
}

fn open(device: Option<&str>, w: u32, h: u32, fps: u32) -> Result<(IMFSourceReader, String)> {
    let list = devices()?;
    let (info, act) = match device {
        Some(id) => list.into_iter().find(|(i, _)| i.id == id),
        None => list.into_iter().next(),
    }
    .ok_or_else(|| PlatformError::DeviceNotFound(device.unwrap_or("Webcam").to_string()))?;
    // SAFETY: plain Media Foundation calls.
    unsafe {
        let source: IMFMediaSource = act.ActivateObject().map_err(|e| {
            if e.code() == windows::Win32::Foundation::E_ACCESSDENIED {
                PlatformError::other("Kein Zugriff auf die Webcam (Windows-Einstellungen → Datenschutz → Kamera: Desktop-Apps erlauben)")
            } else {
                err("Webcam öffnen", e)
            }
        })?;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2).map_err(|e| err("MFCreateAttributes", e))?;
        let attrs = attrs.ok_or_else(|| PlatformError::other("MFCreateAttributes"))?;
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| err("attrs", e))?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)
            .map_err(|e| err("MFCreateSourceReaderFromMediaSource", e))?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let nat = natives(&reader);
        let chosen = pick(&nat, w, h, fps);
        if let Some(n) = chosen {
            let mt = reader
                .GetNativeMediaType(stream, n.index)
                .map_err(|e| err("GetNativeMediaType", e))?;
            reader
                .SetCurrentMediaType(stream, None, &mt)
                .map_err(|e| err("SetCurrentMediaType(native)", e))?;
        }
        let out = MFCreateMediaType().map_err(|e| err("MFCreateMediaType", e))?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| err("out type", e))?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| err("out type", e))?;
        reader
            .SetCurrentMediaType(stream, None, &out)
            .map_err(|e| err("SetCurrentMediaType(RGB32)", e))?;
        let _ = reader.SetStreamSelection(stream, true);
        let desc = match chosen {
            Some(n) => format!(
                "{} {}x{} @ {:.0} fps ({}), asked {w}x{h} @ {fps}",
                info.name,
                n.width,
                n.height,
                n.fps,
                if n.subtype == MFVideoFormat_MJPG {
                    "MJPEG"
                } else {
                    "raw"
                }
            ),
            None => format!("{} (default format)", info.name),
        };
        Ok((reader, desc))
    }
}

fn read_loop(reader: &IMFSourceReader, stop: &AtomicBool, sink: &UnboundedSender<Frame>) {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let mut size = (0u32, 0u32);
    let mut bottom_up = false;
    let mut errors = 0u32;
    let mut last_log = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        let mut flags = 0u32;
        let mut sample = None;
        // SAFETY: plain blocking read with out pointers to locals.
        let r = unsafe {
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
        };
        if let Err(e) = r {
            errors += 1;
            if errors > 20 {
                tracing::warn!("webcam: reading failed repeatedly, giving up: {e}");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 | MF_SOURCE_READERF_ERROR.0 as u32) != 0
        {
            tracing::warn!(flags, "webcam: stream ended");
            break;
        }
        if size == (0, 0) || flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            // SAFETY: plain COM getters.
            if let Ok(mt) = unsafe { reader.GetCurrentMediaType(stream) } {
                if let Ok(v) = unsafe { mt.GetUINT64(&MF_MT_FRAME_SIZE) } {
                    size = ((v >> 32) as u32, v as u32);
                }
                let stride = unsafe { mt.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
                    .ok()
                    .map(|s| s as i32);
                bottom_up = stride_is_bottom_up(stride);
                tracing::info!(
                    "webcam picture {}x{}, default stride {stride:?}",
                    size.0,
                    size.1
                );
            }
        }
        let Some(sample) = sample else { continue };
        errors = 0;
        match to_bgra(&sample, size, bottom_up) {
            Some(frame) => {
                if sink.send(frame).is_err() {
                    break;
                }
            }
            None => {
                if last_log.elapsed() > Duration::from_secs(10) {
                    last_log = Instant::now();
                    tracing::warn!("webcam: picture could not be read");
                }
            }
        }
    }
    tracing::info!("webcam closed");
}

/// Row order of the reader's RGB32 output. The video processor of the source reader writes
/// top-down pictures and says so with a positive `MF_MT_DEFAULT_STRIDE`; only a negative stride
/// means bottom-up. (Treating a missing stride as bottom-up, as the RGB default in the docs
/// suggests, turned the picture upside down.)
fn stride_is_bottom_up(stride: Option<i32>) -> bool {
    stride.is_some_and(|s| s < 0)
}

fn to_bgra(sample: &IMFSample, (w, h): (u32, u32), bottom_up: bool) -> Option<Frame> {
    if w == 0 || h == 0 {
        return None;
    }
    let row = w as usize * 4;
    let mut data = vec![0u8; row * h as usize];
    // SAFETY: the buffer is locked while rows inside its pitch are copied.
    unsafe {
        // The sample's own buffer first: a 2D buffer states the row order itself (Lock2D hands
        // out the top row and a signed pitch); the contiguous copy may lose that interface.
        let buf = match sample.GetBufferCount() {
            Ok(1) => sample.GetBufferByIndex(0).ok()?,
            _ => sample.ConvertToContiguousBuffer().ok()?,
        };
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            b2.Lock2D(&mut scan0, &mut pitch).ok()?;
            if !scan0.is_null() && pitch.unsigned_abs() as usize >= row {
                for y in 0..h as usize {
                    let src = scan0.offset(y as isize * pitch as isize);
                    std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(y * row), row);
                }
            }
            let _ = b2.Unlock2D();
        } else {
            let mut p = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut p, None, Some(&mut len)).ok()?;
            if !p.is_null() && len as usize >= data.len() {
                for y in 0..h as usize {
                    let src_y = if bottom_up { h as usize - 1 - y } else { y };
                    let src = p.add(src_y * row);
                    std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(y * row), row);
                }
            }
            let _ = buf.Unlock();
        }
    }
    Some(Frame {
        width: w,
        height: h,
        ts_us: crate::camera::now_us(),
        data: FrameData::Bgra { data, stride: row },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(width: u32, height: u32, fps: f32, mjpg: bool) -> Native {
        Native {
            index: 0,
            width,
            height,
            fps,
            subtype: if mjpg {
                MFVideoFormat_MJPG
            } else {
                MFVideoFormat_YUY2
            },
        }
    }

    #[test]
    fn picks_matching_format() {
        let list = [
            n(640, 480, 30.0, false),
            n(1280, 720, 10.0, false),
            n(1280, 720, 30.0, true),
            n(1920, 1080, 30.0, true),
        ];
        let p = pick(&list, 1280, 720, 30).unwrap();
        assert_eq!(
            (p.width, p.height, p.subtype),
            (1280, 720, MFVideoFormat_MJPG),
            "720p30 MJPEG beats 720p10 raw"
        );
        let p = pick(&list, 1920, 1080, 30).unwrap();
        assert_eq!((p.width, p.height), (1920, 1080));
        let p = pick(&list, 640, 360, 30).unwrap();
        assert_eq!((p.width, p.height), (1280, 720), "16:9 before 4:3");
    }

    #[test]
    fn only_a_negative_stride_means_bottom_up() {
        // The reader on PC-TA reports +5120 for 1280x720 RGB32 (top-down).
        assert!(!stride_is_bottom_up(Some(5120)));
        assert!(!stride_is_bottom_up(None));
        assert!(stride_is_bottom_up(Some(-5120)));
    }
}
