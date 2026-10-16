//! Read-back helpers: sample the luma of a source BGRA texture and of a decoded NV12 texture on a
//! coarse grid, to prove that the picture survived encode/decode with the right colours.

use anyhow::{Context, Result};
use deskbridge_platform_windows::video::GpuFrame;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, ID3D11Device, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::core::Interface;

pub const GRID_X: usize = 64;
pub const GRID_Y: usize = 32;

fn staging(dev: &ID3D11Device, w: u32, h: u32, fmt: DXGI_FORMAT) -> Result<ID3D11Texture2D> {
    let d = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: fmt,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut t = None;
    // SAFETY: valid descriptor and out pointer.
    unsafe { dev.CreateTexture2D(&d, None, Some(&mut t))? };
    t.context("null staging texture")
}

/// BT.709 limited-range luma of (r,g,b) bytes.
pub fn luma709(r: u8, g: u8, b: u8) -> f32 {
    16.0 + 219.0 * (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}

/// Expected luma on the grid from a BGRA GPU frame.
pub fn grid_from_bgra(g: &GpuFrame) -> Result<Vec<f32>> {
    let ctx = unsafe { g.device.GetImmediateContext()? };
    let st = staging(&g.device, g.width, g.height, g.format)?;
    let dst: ID3D11Resource = st.cast()?;
    let src: ID3D11Resource = g.texture.cast()?;
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    let mut out = Vec::with_capacity(GRID_X * GRID_Y);
    // SAFETY: the staging texture has the same size as the source; reads stay within the pitch.
    unsafe {
        ctx.CopySubresourceRegion(&dst, 0, 0, 0, 0, &src, g.subresource, None);
        ctx.Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
        let base = m.pData.cast::<u8>();
        for gy in 0..GRID_Y {
            for gx in 0..GRID_X {
                let x = (gx * 2 + 1) * g.width as usize / (GRID_X * 2);
                let y = (gy * 2 + 1) * g.height as usize / (GRID_Y * 2);
                let p = base.add(y * m.RowPitch as usize + x * 4);
                out.push(luma709(*p.add(2), *p.add(1), *p));
            }
        }
        ctx.Unmap(&dst, 0);
    }
    Ok(out)
}

/// Decoded luma on the grid from an NV12 GPU frame.
pub fn grid_from_nv12(g: &GpuFrame) -> Result<Vec<f32>> {
    let ctx = unsafe { g.device.GetImmediateContext()? };
    let st = staging(&g.device, g.width, g.height, g.format)?;
    let dst: ID3D11Resource = st.cast()?;
    let src: ID3D11Resource = g.texture.cast()?;
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    let mut out = Vec::with_capacity(GRID_X * GRID_Y);
    // SAFETY: as above; the Y plane occupies the first `height` rows of the mapped NV12 surface.
    unsafe {
        ctx.CopySubresourceRegion(&dst, 0, 0, 0, 0, &src, g.subresource, None);
        ctx.Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
        let base = m.pData.cast::<u8>();
        for gy in 0..GRID_Y {
            for gx in 0..GRID_X {
                let x = (gx * 2 + 1) * g.width as usize / (GRID_X * 2);
                let y = (gy * 2 + 1) * g.height as usize / (GRID_Y * 2);
                out.push(f32::from(*base.add(y * m.RowPitch as usize + x)));
            }
        }
        ctx.Unmap(&dst, 0);
    }
    Ok(out)
}

/// (mean absolute error, max absolute error) between two grids.
pub fn compare(a: &[f32], b: &[f32]) -> (f32, f32) {
    let n = a.len().min(b.len()).max(1);
    let mut sum = 0.0;
    let mut max = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        let d = (x - y).abs();
        sum += d;
        max = max.max(d);
    }
    (sum / n as f32, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_extremes() {
        assert!((luma709(0, 0, 0) - 16.0).abs() < 0.01);
        assert!((luma709(255, 255, 255) - 235.0).abs() < 0.01);
    }

    #[test]
    fn compare_grids() {
        assert_eq!(compare(&[1.0, 2.0], &[1.0, 4.0]), (1.0, 2.0));
    }
}
