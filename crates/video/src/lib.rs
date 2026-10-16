use ffmpeg_next as ffmpeg;
pub fn version() -> u32 { unsafe { ffmpeg::ffi::avcodec_version() } }
