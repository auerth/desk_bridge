//! Small safe-ish layer over the raw FFmpeg API (`ffmpeg_next::ffi`): logging bridge, RAII
//! wrappers for frames / packets / buffer references and the hand-written D3D11 hwcontext structs
//! (`ffmpeg-sys-next` does not generate bindings for `hwcontext_d3d11va.h`).

use std::ffi::{CStr, CString, c_void};
use std::os::raw::{c_char, c_int};
use std::sync::Once;

use deskbridge_platform::PlatformError;
use ffmpeg_next::ffi;

/// `AVD3D11VADeviceContext` (libavutil/hwcontext_d3d11va.h). Only the leading fields are used;
/// FFmpeg 8.x appended two more that we never touch.
#[repr(C)]
pub struct AvD3d11vaDeviceContext {
    pub device: *mut c_void,
    pub device_context: *mut c_void,
    pub video_device: *mut c_void,
    pub video_context: *mut c_void,
    pub lock: Option<unsafe extern "C" fn(*mut c_void)>,
    pub unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    pub lock_ctx: *mut c_void,
}

/// `AVD3D11VAFramesContext`.
#[repr(C)]
pub struct AvD3d11vaFramesContext {
    pub texture: *mut c_void,
    pub bind_flags: u32,
    pub misc_flags: u32,
    pub texture_infos: *mut c_void,
}

static INIT: Once = Once::new();

unsafe extern "C" fn log_callback(
    avcl: *mut c_void,
    level: c_int,
    fmt: *const c_char,
    vl: ffi::va_list,
) {
    if level > ffi::AV_LOG_WARNING {
        return;
    }
    let mut line = [0 as c_char; 1024];
    let mut prefix = 1;
    // SAFETY: arguments are forwarded unchanged from FFmpeg; the buffer is 1024 bytes.
    unsafe {
        ffi::av_log_format_line2(avcl, level, fmt, vl, line.as_mut_ptr(), 1024, &mut prefix);
        let s = CStr::from_ptr(line.as_ptr()).to_string_lossy();
        let s = s.trim_end();
        if level <= ffi::AV_LOG_ERROR {
            tracing::warn!(target: "ffmpeg", "{s}");
        } else {
            tracing::debug!(target: "ffmpeg", "{s}");
        }
    }
}

/// Routes FFmpeg logging into `tracing` (once per process).
pub fn init() {
    INIT.call_once(|| {
        // SAFETY: plain global configuration calls.
        unsafe {
            ffi::av_log_set_level(ffi::AV_LOG_WARNING);
            ffi::av_log_set_callback(Some(log_callback));
        }
    });
}

/// Human-readable text for an FFmpeg error code.
#[must_use]
pub fn err_str(code: c_int) -> String {
    let mut buf = [0 as c_char; 160];
    // SAFETY: buffer size is passed along; av_strerror always NUL-terminates.
    unsafe {
        ffi::av_strerror(code, buf.as_mut_ptr(), buf.len());
        format!("{} ({code})", CStr::from_ptr(buf.as_ptr()).to_string_lossy())
    }
}

pub fn ff_err(what: &str, code: c_int) -> PlatformError {
    PlatformError::Other(format!("{what}: {}", err_str(code)))
}

/// `AVERROR(EAGAIN)`.
pub const EAGAIN: c_int = -11;
/// `AVERROR_EOF`.
pub const AVERROR_EOF: c_int = -0x2046_4F45; // FFERRTAG('E','O','F',' ') negated

/// Owned `AVBufferRef` (hardware device / frames context).
pub struct BufRef(pub *mut ffi::AVBufferRef);

// SAFETY: AVBufferRef is reference counted with atomics; ownership moves with the wrapper.
unsafe impl Send for BufRef {}

impl BufRef {
    pub fn null() -> Self {
        Self(std::ptr::null_mut())
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// New reference to the same buffer.
    #[must_use]
    pub fn new_ref(&self) -> *mut ffi::AVBufferRef {
        // SAFETY: valid (or null) buffer reference.
        unsafe { ffi::av_buffer_ref(self.0) }
    }
}

impl Drop for BufRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this reference.
            unsafe { ffi::av_buffer_unref(&mut self.0) };
        }
    }
}

/// Owned `AVFrame`.
pub struct Frame(pub *mut ffi::AVFrame);

// SAFETY: exclusive ownership; the buffers behind it are reference counted atomically.
unsafe impl Send for Frame {}

impl Frame {
    pub fn alloc() -> Result<Self, PlatformError> {
        // SAFETY: plain allocation.
        let p = unsafe { ffi::av_frame_alloc() };
        if p.is_null() {
            Err(PlatformError::other("av_frame_alloc failed"))
        } else {
            Ok(Self(p))
        }
    }

    pub fn unref(&mut self) {
        // SAFETY: valid frame.
        unsafe { ffi::av_frame_unref(self.0) };
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: we own the frame.
        unsafe { ffi::av_frame_free(&mut self.0) };
    }
}

/// Owned `AVPacket`.
pub struct Packet(pub *mut ffi::AVPacket);

// SAFETY: exclusive ownership.
unsafe impl Send for Packet {}

impl Packet {
    pub fn alloc() -> Result<Self, PlatformError> {
        // SAFETY: plain allocation.
        let p = unsafe { ffi::av_packet_alloc() };
        if p.is_null() {
            Err(PlatformError::other("av_packet_alloc failed"))
        } else {
            Ok(Self(p))
        }
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        // SAFETY: we own the packet.
        unsafe { ffi::av_packet_free(&mut self.0) };
    }
}

/// Owned `AVCodecContext`.
pub struct CodecCtx(pub *mut ffi::AVCodecContext);

// SAFETY: exclusive ownership; used from one thread at a time.
unsafe impl Send for CodecCtx {}

impl Drop for CodecCtx {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own the context.
            unsafe { ffi::avcodec_free_context(&mut self.0) };
        }
    }
}

/// Owned `AVDictionary` used to pass private options to `avcodec_open2`.
pub struct Dict(pub *mut ffi::AVDictionary);

impl Dict {
    pub fn new() -> Self {
        Self(std::ptr::null_mut())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let (k, v) = (CString::new(key).expect("no NUL"), CString::new(value).expect("no NUL"));
        // SAFETY: valid C strings; the dictionary copies them.
        unsafe { ffi::av_dict_set(&mut self.0, k.as_ptr(), v.as_ptr(), 0) };
    }

    /// Keys that `avcodec_open2` did not consume (unknown options).
    pub fn leftover(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut e: *const ffi::AVDictionaryEntry = std::ptr::null();
        let empty = c"";
        loop {
            // SAFETY: iterating a valid dictionary with the documented API.
            e = unsafe { ffi::av_dict_iterate(self.0, e) };
            if e.is_null() {
                break;
            }
            // SAFETY: entries hold valid C strings.
            out.push(unsafe { CStr::from_ptr((*e).key) }.to_string_lossy().into_owned());
        }
        let _ = empty;
        out
    }
}

impl Drop for Dict {
    fn drop(&mut self) {
        // SAFETY: freeing our (possibly null) dictionary.
        unsafe { ffi::av_dict_free(&mut self.0) };
    }
}

/// `FFERRTAG`-free check for "would block".
#[must_use]
pub fn is_again(code: c_int) -> bool {
    code == EAGAIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_constant_matches_ffmpeg() {
        assert_eq!(AVERROR_EOF, ffi::AVERROR_EOF);
        assert_eq!(EAGAIN, ffi::AVERROR(ffi::EAGAIN));
    }

    #[test]
    fn error_text() {
        init();
        assert!(err_str(AVERROR_EOF).contains("End of file"));
    }
}
