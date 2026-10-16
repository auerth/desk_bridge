//! DXGI adapter enumeration for the render-GPU choice.

use deskbridge_display::vdd::AdapterInfo;
use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
};

/// All adapters DXGI knows (hardware and software), in enumeration order.
pub fn enumerate_adapters() -> Result<Vec<AdapterInfo>> {
    // SAFETY: plain DXGI factory use; COM objects are released by their wrappers.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| PlatformError::Os {
            code: i64::from(e.code().0),
            msg: format!("CreateDXGIFactory1: {e}"),
        })?;
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let adapter = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => {
                    return Err(PlatformError::Os {
                        code: i64::from(e.code().0),
                        msg: format!("EnumAdapters1: {e}"),
                    });
                }
            };
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            out.push(AdapterInfo {
                name: super::ccd::wide(&desc.Description),
                vendor_id: desc.VendorId,
                dedicated_vram: desc.DedicatedVideoMemory as u64,
                software: desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_adapters() {
        let a = enumerate_adapters().expect("DXGI");
        eprintln!("adapters: {a:#?}");
        // Every Windows machine has at least WARP / the basic render driver.
        assert!(!a.is_empty());
    }
}
