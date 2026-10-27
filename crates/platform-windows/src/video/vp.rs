//! Colour-space conversion and scaling on the GPU with the D3D11 video processor
//! (BGRA -> NV12 for the encoder input, NV12 -> BGRA for the presenter).

use std::collections::HashMap;

use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Foundation::{BOOL, RECT};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEX2D_ARRAY_VPOV, D3D11_TEX2D_VPIV, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0,
    D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_COLOR_YCbCrA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2DARRAY, ID3D11Device, ID3D11DeviceContext, ID3D11Resource,
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_TYPE,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
    DXGI_RATIONAL,
};
use windows::core::Interface;

use super::common::os_err;

/// Direction of the conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvKind {
    /// Full-range RGB (desktop) to BT.709 limited-range NV12 (encoder input).
    BgraToNv12,
    /// BT.709 limited-range NV12 (decoder output) to full-range BGRA.
    Nv12ToBgra,
}

impl ConvKind {
    fn spaces(self) -> (DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_TYPE) {
        match self {
            Self::BgraToNv12 => (
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            ),
            Self::Nv12ToBgra => (
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            ),
        }
    }
}

/// A configured video processor for one input size / output size / direction.
pub struct VpConverter {
    kind: ConvKind,
    vctx: ID3D11VideoContext,
    venum: ID3D11VideoProcessorEnumerator,
    vdev: ID3D11VideoDevice,
    vp: ID3D11VideoProcessor,
    in_size: (u32, u32),
    out_size: (u32, u32),
    in_views: ViewCache<ID3D11VideoProcessorInputView>,
    out_views: ViewCache<ID3D11VideoProcessorOutputView>,
}

/// Views of the texture used last, per array slice. A view keeps its texture alive, so the views
/// of a texture are dropped as soon as another one shows up: the decoder re-creates its whole
/// surface pool whenever the stream parameters change (every bitrate change of the encoder), and a
/// cache keyed by texture kept each old pool (~120 MB at 2560x1440) until the GPU ran out of
/// memory and `Present` hung. The capture pool behind the encoder is re-created the same way.
struct ViewCache<V> {
    tex: usize,
    views: HashMap<u32, V>,
}

impl<V: Clone> ViewCache<V> {
    fn new() -> Self {
        Self {
            tex: 0,
            views: HashMap::new(),
        }
    }

    /// The cached view of `tex[slice]`; forgets the views of any other texture.
    fn get(&mut self, tex: usize, slice: u32) -> Option<V> {
        if self.tex != tex {
            self.views.clear();
            self.tex = tex;
        }
        self.views.get(&slice).cloned()
    }

    fn put(&mut self, slice: u32, view: V) {
        self.views.insert(slice, view);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.views.len()
    }
}

// SAFETY: all contained COM interfaces are free-threaded D3D11 objects; the device is created
// with multithread protection (see `create_device`) and `VpConverter` itself is used from one
// thread at a time (`&mut self`).
unsafe impl Send for VpConverter {}

impl VpConverter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        kind: ConvKind,
        in_size: (u32, u32),
        out_size: (u32, u32),
    ) -> Result<Self> {
        let vdev: ID3D11VideoDevice = device
            .cast()
            .map_err(|e| os_err("ID3D11VideoDevice (no video support)", &e))?;
        let vctx: ID3D11VideoContext = context
            .cast()
            .map_err(|e| os_err("ID3D11VideoContext", &e))?;
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: `desc` is valid for the call; all results are owned interfaces.
        let (venum, vp) = unsafe {
            let venum = vdev
                .CreateVideoProcessorEnumerator(&desc)
                .map_err(|e| os_err("CreateVideoProcessorEnumerator", &e))?;
            let (inf, outf) = match kind {
                ConvKind::BgraToNv12 => (DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12),
                ConvKind::Nv12ToBgra => (DXGI_FORMAT_NV12, DXGI_FORMAT_B8G8R8A8_UNORM),
            };
            let si = venum
                .CheckVideoProcessorFormat(inf)
                .map_err(|e| os_err("CheckVideoProcessorFormat(in)", &e))?;
            let so = venum
                .CheckVideoProcessorFormat(outf)
                .map_err(|e| os_err("CheckVideoProcessorFormat(out)", &e))?;
            if si & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0
                || so & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0
            {
                return Err(PlatformError::other(format!(
                    "video processor does not support {kind:?} on this GPU (in={si:#x} out={so:#x})"
                )));
            }
            let vp = vdev
                .CreateVideoProcessor(&venum, 0)
                .map_err(|e| os_err("CreateVideoProcessor", &e))?;
            (venum, vp)
        };
        let this = Self {
            kind,
            vctx,
            venum,
            vdev,
            vp,
            in_size,
            out_size,
            in_views: ViewCache::new(),
            out_views: ViewCache::new(),
        };
        this.configure_colour_spaces();
        this.set_black_background();
        Ok(this)
    }

    /// Fills the pixels outside the stream destination rectangle (the letterbox bars, when a
    /// picture is scaled with a dest rect smaller than the output) with opaque black in the
    /// output colour space. A no-op for a full-output blt, where no such pixels exist.
    fn set_black_background(&self) {
        // SAFETY: plain COM call on an interface we own.
        unsafe {
            let (ycbcr, color) = match self.kind {
                // Studio-range BT.709 black: Y=16, Cb=Cr=128 (normalized).
                ConvKind::BgraToNv12 => (
                    BOOL(1),
                    D3D11_VIDEO_COLOR {
                        Anonymous: D3D11_VIDEO_COLOR_0 {
                            YCbCr: D3D11_VIDEO_COLOR_YCbCrA {
                                Y: 16.0 / 255.0,
                                Cb: 128.0 / 255.0,
                                Cr: 128.0 / 255.0,
                                A: 1.0,
                            },
                        },
                    },
                ),
                ConvKind::Nv12ToBgra => (
                    BOOL(0),
                    D3D11_VIDEO_COLOR {
                        Anonymous: D3D11_VIDEO_COLOR_0 {
                            RGBA: D3D11_VIDEO_COLOR_RGBA {
                                R: 0.0,
                                G: 0.0,
                                B: 0.0,
                                A: 1.0,
                            },
                        },
                    },
                ),
            };
            self.vctx
                .VideoProcessorSetOutputBackgroundColor(&self.vp, ycbcr, &color);
        }
    }

    fn configure_colour_spaces(&self) {
        let (inp, outp) = self.kind.spaces();
        // SAFETY: plain COM calls on interfaces we own.
        unsafe {
            if let Ok(v1) = self.vctx.cast::<ID3D11VideoContext1>() {
                v1.VideoProcessorSetStreamColorSpace1(&self.vp, 0, inp);
                v1.VideoProcessorSetOutputColorSpace1(&self.vp, outp);
            }
            self.vctx.VideoProcessorSetStreamFrameFormat(
                &self.vp,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
        }
    }

    #[must_use]
    pub fn in_size(&self) -> (u32, u32) {
        self.in_size
    }

    #[must_use]
    pub fn out_size(&self) -> (u32, u32) {
        self.out_size
    }

    #[must_use]
    pub fn kind(&self) -> ConvKind {
        self.kind
    }

    fn input_view(
        &mut self,
        tex: &ID3D11Texture2D,
        slice: u32,
    ) -> Result<ID3D11VideoProcessorInputView> {
        if let Some(v) = self.in_views.get(tex.as_raw() as usize, slice) {
            return Ok(v);
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: slice,
                },
            },
        };
        let res: ID3D11Resource = tex.cast().map_err(|e| os_err("texture -> resource", &e))?;
        let mut view: Option<ID3D11VideoProcessorInputView> = None;
        // SAFETY: valid descriptor and out pointer.
        unsafe {
            self.vdev
                .CreateVideoProcessorInputView(&res, &self.venum, &desc, Some(&mut view))
        }
        .map_err(|e| os_err("CreateVideoProcessorInputView", &e))?;
        let view = view.ok_or_else(|| PlatformError::other("no input view"))?;
        self.in_views.put(slice, view.clone());
        Ok(view)
    }

    fn output_view(
        &mut self,
        tex: &ID3D11Texture2D,
        slice: u32,
    ) -> Result<ID3D11VideoProcessorOutputView> {
        if let Some(v) = self.out_views.get(tex.as_raw() as usize, slice) {
            return Ok(v);
        }
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_VPOV {
                    MipSlice: 0,
                    FirstArraySlice: slice,
                    ArraySize: 1,
                },
            },
        };
        let res: ID3D11Resource = tex.cast().map_err(|e| os_err("texture -> resource", &e))?;
        let mut view: Option<ID3D11VideoProcessorOutputView> = None;
        // SAFETY: valid descriptor and out pointer.
        unsafe {
            self.vdev
                .CreateVideoProcessorOutputView(&res, &self.venum, &desc, Some(&mut view))
        }
        .map_err(|e| os_err("CreateVideoProcessorOutputView", &e))?;
        let view = view.ok_or_else(|| PlatformError::other("no output view"))?;
        self.out_views.put(slice, view.clone());
        Ok(view)
    }

    /// Converts (and scales) `in_tex[in_slice]` into `out_tex[out_slice]`. `dest` is the target
    /// rectangle inside the output (`None` = whole output).
    pub fn blt(
        &mut self,
        in_tex: &ID3D11Texture2D,
        in_slice: u32,
        out_tex: &ID3D11Texture2D,
        out_slice: u32,
        dest: Option<RECT>,
    ) -> Result<()> {
        let iv = self.input_view(in_tex, in_slice)?;
        let ov = self.output_view(out_tex, out_slice)?;
        let full = RECT {
            left: 0,
            top: 0,
            right: self.in_size.0 as i32,
            bottom: self.in_size.1 as i32,
        };
        let dst = dest.unwrap_or(RECT {
            left: 0,
            top: 0,
            right: self.out_size.0 as i32,
            bottom: self.out_size.1 as i32,
        });
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            pInputSurface: std::mem::ManuallyDrop::new(Some(iv)),
            ..Default::default()
        };
        // SAFETY: rects are valid for the calls; `stream` holds a (manually dropped) interface
        // clone that is released right after the blt.
        let r = unsafe {
            self.vctx
                .VideoProcessorSetStreamSourceRect(&self.vp, 0, true, Some(&full));
            self.vctx
                .VideoProcessorSetStreamDestRect(&self.vp, 0, true, Some(&dst));
            self.vctx.VideoProcessorSetOutputTargetRect(
                &self.vp,
                true,
                Some(&RECT {
                    left: 0,
                    top: 0,
                    right: self.out_size.0 as i32,
                    bottom: self.out_size.1 as i32,
                }),
            );
            self.vctx
                .VideoProcessorBlt(&self.vp, &ov, 0, std::slice::from_ref(&stream))
        };
        // Release the extra reference held by the stream description.
        drop(std::mem::ManuallyDrop::into_inner(stream.pInputSurface));
        r.map_err(|e| os_err("VideoProcessorBlt", &e))
    }
}

#[cfg(test)]
mod tests {
    use super::ViewCache;

    #[test]
    fn view_cache_only_keeps_the_current_texture() {
        let mut c = ViewCache::<u32>::new();
        assert_eq!(c.get(0xA0, 0), None);
        c.put(0, 10);
        c.put(1, 11);
        assert_eq!(c.get(0xA0, 1), Some(11));
        assert_eq!(c.len(), 2);
        // a new decoder pool: the old texture's views (and with them the texture) are released
        assert_eq!(c.get(0xB0, 1), None);
        assert_eq!(c.len(), 0);
        c.put(1, 21);
        assert_eq!(c.get(0xB0, 1), Some(21));
    }
}
