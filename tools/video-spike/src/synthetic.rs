//! Synthetic moving picture rendered on the GPU into BGRA textures (same format and ownership
//! model as the DXGI capturer: a small texture pool, frames are `GpuFrame`s).
//!
//! The content is deliberately hard: a full-frame horizontally scrolling "text" band with sharp
//! edges (like scrolling code), a smooth moving gradient and a fast moving box. Every pixel of the
//! band changes each frame, which is a worst case for the encoder (real desktop content is
//! usually much easier).

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use deskbridge_platform::video::{Frame, FrameData};
use deskbridge_platform_windows::video::{D3dDevice, GpuFrame, now_us};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BUFFER_DESC, D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_RASTERIZER_DESC,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT, ID3D11Buffer, ID3D11PixelShader,
    ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::{Interface, PCSTR};

const HLSL: &str = r"
cbuffer C : register(b0) { float time; float w; float h; float pad; };
float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * 2.0 - 1.0, 0, 1);
}
float hash(float2 p) {
    p = frac(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return frac(p.x * p.y);
}
float4 ps_main(float4 pos : SV_Position) : SV_Target {
    float2 uv = pos.xy;
    float scroll = time * 600.0;
    float2 q = float2(uv.x + scroll, uv.y);
    float3 col = 0.5 + 0.5 * float3(sin(q.x * 0.002 + 1.0), sin(q.y * 0.004 + 2.0), sin((q.x + q.y) * 0.001));
    float2 cell = float2(q.x / 7.0, q.y / 14.0);
    float2 fc = floor(cell);
    float2 inc = frac(cell);
    float on = step(0.55, hash(fc));
    float glyph = on * step(0.15, inc.x) * step(inc.x, 0.8) * step(0.2, inc.y) * step(inc.y, 0.8);
    float band = step(0.2 * h, uv.y) * step(uv.y, 0.8 * h);
    col = lerp(col, lerp(float3(0.95, 0.95, 0.95), float3(0.05, 0.05, 0.05), glyph), band);
    float2 b = float2(fmod(time * 900.0, w), h * 0.5 + 300.0 * sin(time * 3.0));
    if (abs(uv.x - b.x) < 120.0 && abs(uv.y - b.y) < 80.0) col = float3(1.0, 0.2, 0.1);
    return float4(col, 1.0);
}
";

fn compile(entry: &str, target: &str) -> Result<ID3DBlob> {
    let e = std::ffi::CString::new(entry)?;
    let t = std::ffi::CString::new(target)?;
    let mut code: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;
    // SAFETY: valid source and out pointers; C strings outlive the call.
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(e.as_ptr().cast()),
            PCSTR(t.as_ptr().cast()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut err),
        )
    };
    if let Err(e) = r {
        let msg = err
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer().cast::<u8>(),
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        return Err(anyhow!("D3DCompile: {e} {msg}"));
    }
    code.ok_or_else(|| anyhow!("no shader code"))
}

struct Pool {
    free: Mutex<Vec<(ID3D11Texture2D, ID3D11RenderTargetView)>>,
}

struct Slot {
    item: Option<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    pool: Arc<Pool>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let (Some(i), Ok(mut f)) = (self.item.take(), self.pool.free.lock()) {
            f.push(i);
        }
    }
}

pub struct SynthSource {
    dev: D3dDevice,
    width: u32,
    height: u32,
    pool: Arc<Pool>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    raster: ID3D11RasterizerState,
}

// SAFETY: free-threaded D3D11 objects on a multithread-protected device, used from one thread.
unsafe impl Send for SynthSource {}

impl SynthSource {
    pub fn new(dev: D3dDevice, width: u32, height: u32) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ..Default::default()
        };
        let mut v = Vec::new();
        for _ in 0..4 {
            let mut t: Option<ID3D11Texture2D> = None;
            let mut rtv = None;
            // SAFETY: valid descriptors and out pointers.
            unsafe {
                dev.device.CreateTexture2D(&desc, None, Some(&mut t))?;
                let tex = t.context("null texture")?;
                dev.device
                    .CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
                v.push((tex, rtv.context("null rtv")?));
            }
        }
        let vsb = compile("vs_main", "vs_5_0")?;
        let psb = compile("ps_main", "ps_5_0")?;
        let (mut vs, mut ps, mut cb, mut raster) = (None, None, None, None);
        // SAFETY: bytecode slices are valid for the calls.
        unsafe {
            let bytes = |b: &ID3DBlob| {
                std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize())
            };
            dev.device
                .CreateVertexShader(bytes(&vsb), None, Some(&mut vs))?;
            dev.device
                .CreatePixelShader(bytes(&psb), None, Some(&mut ps))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            dev.device.CreateBuffer(&bd, None, Some(&mut cb))?;
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            };
            dev.device.CreateRasterizerState(&rd, Some(&mut raster))?;
        }
        Ok(Self {
            dev,
            width,
            height,
            pool: Arc::new(Pool {
                free: Mutex::new(v),
            }),
            vs: vs.context("vs")?,
            ps: ps.context("ps")?,
            cb: cb.context("cb")?,
            raster: raster.context("raster")?,
        })
    }

    /// Renders picture number `n` at animation time `t` seconds. `None` = all pool textures busy.
    pub fn render(&self, t: f32) -> Option<Frame> {
        let item = self.pool.free.lock().ok()?.pop()?;
        let slot = Slot {
            item: Some(item),
            pool: self.pool.clone(),
        };
        let (tex, rtv) = slot.item.as_ref().expect("just set").clone();
        let ctx = &self.dev.context;
        let params = [t, self.width as f32, self.height as f32, 0.0f32];
        // SAFETY: plain COM calls on objects owned here; `params` is 16 bytes of plain data.
        unsafe {
            ctx.UpdateSubresource(&self.cb, 0, None, params.as_ptr().cast::<c_void>(), 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.width as f32,
                Height: self.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.Draw(3, 0);
            ctx.OMSetRenderTargets(None, None);
            ctx.Flush();
        }
        Some(Frame {
            width: self.width,
            height: self.height,
            ts_us: now_us(),
            data: FrameData::Gpu(Box::new(GpuFrame {
                texture: tex.cast().ok()?,
                device: self.dev.device.clone(),
                adapter_luid: self.dev.adapter_luid,
                subresource: 0,
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                width: self.width,
                height: self.height,
                hold: Some(Box::new(slot)),
            })),
        })
    }
}

// `Slot` must be `Send` to live inside `GpuFrame::hold`.
// SAFETY: the contained D3D11 interfaces are free-threaded.
unsafe impl Send for Slot {}
