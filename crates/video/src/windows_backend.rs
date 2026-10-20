//! Windows implementation of the engine seam [`VideoBackend`]: DXGI capture, FFmpeg
//! encode/decode (NVENC zero-copy first) and the native D3D11 presenter.

use std::sync::Arc;

use deskbridge_platform::screen::ScreenProvider;
use deskbridge_platform::video::{
    EncoderConfig, ScreenCapturer, VideoDecoder, VideoEncoder, VideoPresenter,
};
use deskbridge_platform::{PlatformError, Result};
use deskbridge_platform_windows::screen::WinScreenProvider;
use deskbridge_platform_windows::video::{
    D3dDevice, D3dPresenter, DxgiCapturer, PresentMode, PxRect, adapter_by_index, create_device,
    list_outputs,
};
use deskbridge_proto::pb;
use parking_lot::Mutex;

use crate::backend::{PresenterMode, VideoBackend};
use crate::decoder::FfmpegDecoder;
use crate::encoder::FfmpegEncoder;
use crate::probe::{probe_decoders, probe_encoders};

/// The real video stack on Windows.
pub struct WindowsVideoBackend {
    screens: Arc<dyn ScreenProvider>,
    /// Device shared by decoder and presenter so decoded textures can be sampled without copies.
    view_device: Mutex<Option<D3dDevice>>,
    enc_caps: Mutex<Option<pb::VideoCaps>>,
    dec_caps: Mutex<Option<pb::VideoCaps>>,
}

impl WindowsVideoBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::with_screens(Arc::new(WinScreenProvider::new()))
    }

    /// Uses the engine's screen provider so `screen_id`s resolve identically.
    #[must_use]
    pub fn with_screens(screens: Arc<dyn ScreenProvider>) -> Self {
        Self {
            screens,
            view_device: Mutex::new(None),
            enc_caps: Mutex::new(None),
            dec_caps: Mutex::new(None),
        }
    }

    /// Device for decoding/presenting: on the adapter that owns the given desktop rectangle, or the
    /// adapter of the first attached output.
    fn view_device(&self, rect: Option<(i32, i32, u32, u32)>) -> Result<D3dDevice> {
        let mut g = self.view_device.lock();
        if let Some(d) = g.as_ref() {
            // SAFETY: plain COM call on a live device.
            match unsafe { d.device.GetDeviceRemovedReason() } {
                Ok(()) => return Ok(d.clone()),
                Err(e) => {
                    // Driver reset / TDR: everything built on this device is dead.
                    tracing::warn!("viewer D3D11 device was removed ({e}); creating a new one");
                    *g = None;
                }
            }
        }
        let outputs = list_outputs()?;
        let target = rect
            .and_then(|(x, y, w, h)| {
                deskbridge_platform_windows::video::find_output_by_rect(&outputs, x, y, w, h)
            })
            .or_else(|| outputs.iter().find(|o| o.attached && !o.software));
        let adapter = match target {
            Some(o) => Some(adapter_by_index(o.adapter_index)?),
            None => None,
        };
        let d = create_device(adapter.as_ref())?;
        *g = Some(d.clone());
        Ok(d)
    }
}

impl Default for WindowsVideoBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoBackend for WindowsVideoBackend {
    fn encoder_caps(&self) -> pb::VideoCaps {
        self.enc_caps
            .lock()
            .get_or_insert_with(probe_encoders)
            .clone()
    }

    fn decoder_caps(&self) -> pb::VideoCaps {
        self.dec_caps
            .lock()
            .get_or_insert_with(|| probe_decoders(None))
            .clone()
    }

    fn create_capturer(&self) -> Result<Box<dyn ScreenCapturer>> {
        Ok(Box::new(DxgiCapturer::new(self.screens.clone())))
    }

    fn create_encoder(&self, cfg: &EncoderConfig, adapter: &str) -> Result<Box<dyn VideoEncoder>> {
        tracing::info!(
            "creating {:?} encoder for capture adapter '{adapter}'",
            cfg.codec
        );
        // Opened on the first frame, on the capturer's own device (zero-copy requires it).
        Ok(Box::new(FfmpegEncoder::new(cfg.clone(), None)?))
    }

    fn create_decoder(&self, cfg: &EncoderConfig) -> Result<Box<dyn VideoDecoder>> {
        let dev = self.view_device(None)?;
        Ok(Box::new(FfmpegDecoder::new(cfg.codec, Some(&dev.device))?))
    }

    fn create_presenter(
        &self,
        mode: PresenterMode,
        _cfg: &EncoderConfig,
    ) -> Result<Box<dyn VideoPresenter>> {
        let (x, y, w, h) = mode.rect();
        let rect = match mode {
            PresenterMode::Window { .. } => None,
            _ => Some((x, y, w, h)),
        };
        let dev = self.view_device(rect)?;
        let pm = match mode {
            PresenterMode::Fullscreen { x, y, w, h } => PresentMode::Fullscreen {
                monitor_rect: PxRect::new(x, y, w, h),
            },
            PresenterMode::Region { x, y, w, h, appbar } => PresentMode::Region {
                rect: PxRect::new(x, y, w, h),
                appbar,
            },
            PresenterMode::Window { w, h } => PresentMode::Window {
                width: w,
                height: h,
            },
        };
        let p = D3dPresenter::new(pm, Some(&dev.device))
            .map_err(|e| PlatformError::other(format!("video window: {e}")))?;
        Ok(Box::new(p))
    }
}
