//! DIB (`CF_DIB` / `CF_DIBV5`) <-> PNG conversion. Pure byte manipulation, no Win32 calls.
//!
//! Internal image representation is RGBA8, top-down, straight (non-premultiplied) alpha.

use std::io::Cursor;

/// Maximum accepted pixel count (guards against absurd headers): 256 Mpx.
const MAX_PIXELS: u64 = 256 * 1024 * 1024;

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_PNG: u32 = 5;
const BI_ALPHABITFIELDS: u32 = 6;
const PROFILE_EMBEDDED: u32 = 0x4D42_4544; // 'MBED'
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Decoded image: RGBA8, top-down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn i32_at(d: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// Channel extractor for a bit mask: scales to 8 bits.
#[derive(Clone, Copy)]
struct Channel {
    mask: u32,
    shift: u32,
    max: u32,
}

impl Channel {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Self {
                mask: 0,
                shift: 0,
                max: 0,
            };
        }
        let shift = mask.trailing_zeros();
        Self {
            mask,
            shift,
            max: mask >> shift,
        }
    }
    fn get(self, px: u32) -> u8 {
        if self.mask == 0 {
            return 0;
        }
        let v = (px & self.mask) >> self.shift;
        if self.max == 255 {
            v as u8
        } else {
            ((v * 255 + self.max / 2) / self.max) as u8
        }
    }
    fn present(self) -> bool {
        self.mask != 0
    }
}

/// Decodes a packed DIB (`BITMAPINFOHEADER`/`V4`/`V5` + masks + palette + pixels).
///
/// Alpha policy: only 32-bit pixels carry alpha. With a 40-byte header and `BI_RGB` the fourth byte
/// is unused (opaque). With V4/V5 headers or alpha bit masks the alpha channel is honoured, except
/// that an alpha channel that is zero everywhere is treated as opaque (many producers leave it
/// unset).
pub fn decode_dib(dib: &[u8]) -> Result<Rgba, String> {
    let hsize = u32_at(dib, 0).ok_or("DIB too short")? as usize;
    if hsize < 40 || dib.len() < hsize {
        return Err(format!("unsupported DIB header size {hsize}"));
    }
    let width = i32_at(dib, 4).ok_or("DIB too short")?;
    let height = i32_at(dib, 8).ok_or("DIB too short")?;
    let bpp = u32::from(u16_at(dib, 14).ok_or("DIB too short")?);
    let compression = u32_at(dib, 16).ok_or("DIB too short")?;
    let clr_used = u32_at(dib, 32).ok_or("DIB too short")? as usize;
    if width <= 0 || height == 0 || height == i32::MIN {
        return Err("invalid DIB dimensions".into());
    }
    let (w, h) = (width as u32, height.unsigned_abs());
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("DIB too large".into());
    }
    let top_down = height < 0;
    if matches!(compression, BI_RLE8 | BI_RLE4) {
        return Err("RLE-compressed DIBs are not supported".into());
    }
    if !matches!(compression, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) {
        return Err(format!("unsupported DIB compression {compression}"));
    }
    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(format!("unsupported DIB bit depth {bpp}"));
    }

    // Masks: inside the header for >= 52 bytes, otherwise trailing the 40-byte header.
    let mut off = hsize;
    let uses_masks =
        matches!(compression, BI_BITFIELDS | BI_ALPHABITFIELDS) && matches!(bpp, 16 | 32);
    let (mut rm, mut gm, mut bm, mut am) = (0u32, 0u32, 0u32, 0u32);
    if uses_masks {
        if hsize >= 52 {
            rm = u32_at(dib, 40).ok_or("DIB too short")?;
            gm = u32_at(dib, 44).ok_or("DIB too short")?;
            bm = u32_at(dib, 48).ok_or("DIB too short")?;
            if hsize >= 56 {
                am = u32_at(dib, 52).ok_or("DIB too short")?;
            }
        } else {
            rm = u32_at(dib, off).ok_or("DIB too short")?;
            gm = u32_at(dib, off + 4).ok_or("DIB too short")?;
            bm = u32_at(dib, off + 8).ok_or("DIB too short")?;
            let n = if compression == BI_ALPHABITFIELDS {
                4
            } else {
                3
            };
            if n == 4 {
                am = u32_at(dib, off + 12).ok_or("DIB too short")?;
            }
            off += n * 4;
        }
    } else if bpp == 16 {
        (rm, gm, bm) = (0x7C00, 0x03E0, 0x001F);
    } else if bpp == 32 {
        (rm, gm, bm) = (0x00FF_0000, 0x0000_FF00, 0x0000_00FF);
        // Alpha is in the top byte for V4/V5 headers (BI_RGB), unused for plain info headers.
        if hsize >= 108 {
            am = 0xFF00_0000;
        }
    }

    // Palette.
    let mut palette: Vec<[u8; 3]> = Vec::new();
    if bpp <= 8 {
        let n = if clr_used != 0 {
            clr_used
        } else {
            1usize << bpp
        }
        .min(1 << bpp);
        let raw = dib.get(off..off + n * 4).ok_or("DIB palette truncated")?;
        palette = raw.chunks_exact(4).map(|c| [c[2], c[1], c[0]]).collect();
        off += n * 4;
    }
    // Embedded ICC profile sits between palette and pixels in V5 DIBs.
    if hsize >= 124 && u32_at(dib, 56) == Some(PROFILE_EMBEDDED) {
        off += u32_at(dib, 116).unwrap_or(0) as usize;
    }

    let stride = ((w as usize * bpp as usize).div_ceil(32)) * 4;
    let need = stride
        .checked_mul(h as usize)
        .and_then(|n| n.checked_add(off))
        .ok_or("DIB too large")?;
    if dib.len() < need {
        return Err("DIB pixel data truncated".into());
    }
    let pixels = &dib[off..];

    let (rc, gc, bc, ac) = (
        Channel::new(rm),
        Channel::new(gm),
        Channel::new(bm),
        Channel::new(am),
    );
    let mut out = vec![0u8; w as usize * h as usize * 4];
    let mut any_alpha = false;
    for y in 0..h as usize {
        let src_row = if top_down { y } else { h as usize - 1 - y };
        let row = &pixels[src_row * stride..src_row * stride + stride];
        let dst = &mut out[y * w as usize * 4..(y + 1) * w as usize * 4];
        for x in 0..w as usize {
            let (r, g, b, a) = match bpp {
                24 => (row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255),
                32 | 16 => {
                    let px = if bpp == 32 {
                        u32::from_le_bytes([
                            row[x * 4],
                            row[x * 4 + 1],
                            row[x * 4 + 2],
                            row[x * 4 + 3],
                        ])
                    } else {
                        u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]))
                    };
                    let a = if ac.present() { ac.get(px) } else { 255 };
                    any_alpha |= ac.present() && a != 0;
                    (rc.get(px), gc.get(px), bc.get(px), a)
                }
                _ => {
                    let per_byte = 8 / bpp as usize;
                    let byte = row[x / per_byte];
                    let shift = 8 - bpp as usize * (x % per_byte + 1);
                    let idx = ((byte >> shift) & ((1u16 << bpp) - 1) as u8) as usize;
                    let c = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    (c[0], c[1], c[2], 255)
                }
            };
            dst[x * 4..x * 4 + 4].copy_from_slice(&[r, g, b, a]);
        }
    }
    if ac.present() && !any_alpha {
        // Alpha channel exists but is all zero: treat as opaque.
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Ok(Rgba {
        width: w,
        height: h,
        data: out,
    })
}

/// Encodes RGBA8 as PNG (RGB if fully opaque, RGBA otherwise).
pub fn encode_png(img: &Rgba) -> Result<Vec<u8>, String> {
    let opaque = img.data.chunks_exact(4).all(|p| p[3] == 255);
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let rgb;
        let data: &[u8] = if opaque {
            enc.set_color(png::ColorType::Rgb);
            rgb = img
                .data
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect::<Vec<u8>>();
            &rgb
        } else {
            enc.set_color(png::ColorType::Rgba);
            &img.data
        };
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(data).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decodes any PNG (palette, grayscale, 16-bit, interlaced) to RGBA8.
pub fn decode_png(data: &[u8]) -> Result<Rgba, String> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let (w, h) = (reader.info().width, reader.info().height);
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("PNG too large".into());
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let buf = &buf[..info.buffer_size()];
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    match info.color_type {
        png::ColorType::Rgba => out.extend_from_slice(buf),
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255])),
        png::ColorType::Grayscale => buf
            .iter()
            .for_each(|&g| out.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    }
    if out.len() != w as usize * h as usize * 4 {
        return Err("PNG decode size mismatch".into());
    }
    Ok(Rgba {
        width: w,
        height: h,
        data: out,
    })
}

/// Encodes RGBA8 as a packed `CF_DIBV5` (32 bpp, `BI_BITFIELDS` BGRA masks, sRGB, bottom-up,
/// straight alpha).
#[must_use]
pub fn encode_dibv5(img: &Rgba) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let size_image = w * h * 4;
    let mut out = Vec::with_capacity(124 + size_image);
    out.extend_from_slice(&124u32.to_le_bytes()); // bV5Size
    out.extend_from_slice(&(img.width as i32).to_le_bytes());
    out.extend_from_slice(&(img.height as i32).to_le_bytes()); // positive: bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bit count
    out.extend_from_slice(&BI_BITFIELDS.to_le_bytes());
    out.extend_from_slice(&(size_image as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // x ppm
    out.extend_from_slice(&0u32.to_le_bytes()); // y ppm
    out.extend_from_slice(&0u32.to_le_bytes()); // clr used
    out.extend_from_slice(&0u32.to_le_bytes()); // clr important
    out.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // red mask
    out.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // green mask
    out.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // blue mask
    out.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // alpha mask
    out.extend_from_slice(&0x7352_4742u32.to_le_bytes()); // LCS_sRGB ('sRGB')
    out.extend_from_slice(&[0u8; 36 + 12]); // endpoints + gamma
    out.extend_from_slice(&4u32.to_le_bytes()); // LCS_GM_IMAGES
    out.extend_from_slice(&[0u8; 12]); // profile data/size, reserved
    debug_assert_eq!(out.len(), 124);
    for y in (0..h).rev() {
        for p in img.data[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    out
}

/// `CF_DIB`/`CF_DIBV5` bytes to PNG bytes.
pub fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>, String> {
    // BI_PNG DIBs embed a PNG stream (printer drivers); pass it through.
    if let (Some(hsize), Some(BI_PNG)) = (u32_at(dib, 0), u32_at(dib, 16))
        && let Some(rest) = dib.get(hsize as usize..)
        && rest.starts_with(&PNG_SIGNATURE)
    {
        return Ok(rest.to_vec());
    }
    encode_png(&decode_dib(dib)?)
}

/// PNG bytes to a packed `CF_DIBV5`.
pub fn png_to_dibv5(png_data: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode_dibv5(&decode_png(png_data)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, alpha: bool) -> Rgba {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = if alpha {
                    (x * 40 + y * 13 + 1) as u8
                } else {
                    255
                };
                data.extend_from_slice(&[
                    (x * 30 + 5) as u8,
                    (y * 50 + 7) as u8,
                    (x * y + 3) as u8,
                    a,
                ]);
            }
        }
        Rgba {
            width: w,
            height: h,
            data,
        }
    }

    fn header(w: i32, h: i32, bpp: u16, comp: u32, extra: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&bpp.to_le_bytes());
        v.extend_from_slice(&comp.to_le_bytes());
        v.extend_from_slice(&[0u8; 20]);
        v.extend(std::iter::repeat_n(0, extra));
        v
    }

    #[test]
    fn dibv5_png_roundtrip_with_alpha_and_odd_width() {
        for (w, h) in [(1, 1), (3, 2), (5, 7), (17, 3)] {
            for alpha in [false, true] {
                let img = gradient(w, h, alpha);
                let png = encode_png(&img).unwrap();
                assert_eq!(&png[..8], &PNG_SIGNATURE);
                assert_eq!(decode_png(&png).unwrap(), img, "png {w}x{h} alpha={alpha}");
                let dib = png_to_dibv5(&png).unwrap();
                assert_eq!(dib.len(), 124 + (w * h * 4) as usize);
                assert_eq!(
                    decode_dib(&dib).unwrap(),
                    img,
                    "dibv5 {w}x{h} alpha={alpha}"
                );
                let png2 = dib_to_png(&dib).unwrap();
                assert_eq!(decode_png(&png2).unwrap(), img);
            }
        }
    }

    #[test]
    fn opaque_image_uses_rgb_png() {
        let png = encode_png(&gradient(4, 4, false)).unwrap();
        let dec = png::Decoder::new(Cursor::new(&png[..]));
        assert_eq!(
            dec.read_info().unwrap().info().color_type,
            png::ColorType::Rgb
        );
        let png = encode_png(&gradient(4, 4, true)).unwrap();
        let dec = png::Decoder::new(Cursor::new(&png[..]));
        assert_eq!(
            dec.read_info().unwrap().info().color_type,
            png::ColorType::Rgba
        );
    }

    #[test]
    fn bgr24_bottom_up_and_top_down_with_padding() {
        // 3x2 24 bpp: stride = 12 (9 bytes + 3 padding).
        let rows = [
            [(1u8, 2u8, 3u8), (4, 5, 6), (7, 8, 9)],
            [(10, 11, 12), (13, 14, 15), (16, 17, 18)],
        ]; // R,G,B, top row first
        let build = |h: i32, order: [usize; 2]| {
            let mut d = header(3, h, 24, BI_RGB, 0);
            for r in order {
                for &(rr, gg, bb) in &rows[r] {
                    d.extend_from_slice(&[bb, gg, rr]);
                }
                d.extend_from_slice(&[0xEE; 3]);
            }
            d
        };
        let expect: Vec<u8> = rows
            .iter()
            .flatten()
            .flat_map(|&(r, g, b)| [r, g, b, 255])
            .collect();
        let bottom_up = decode_dib(&build(2, [1, 0])).unwrap();
        let top_down = decode_dib(&build(-2, [0, 1])).unwrap();
        assert_eq!(bottom_up.data, expect);
        assert_eq!(top_down.data, expect);
        assert_eq!((bottom_up.width, bottom_up.height), (3, 2));
    }

    #[test]
    fn bgra32_bi_rgb_ignores_alpha_for_info_header_but_not_v5() {
        let mut d = header(1, 1, 32, BI_RGB, 0);
        d.extend_from_slice(&[10, 20, 30, 0x80]); // B G R A
        assert_eq!(decode_dib(&d).unwrap().data, vec![30, 20, 10, 255]);

        // V5 header, BI_RGB, alpha present.
        let mut v5 = encode_dibv5(&Rgba {
            width: 1,
            height: 1,
            data: vec![30, 20, 10, 0x80],
        });
        v5[16..20].copy_from_slice(&BI_RGB.to_le_bytes());
        v5[40..56].fill(0);
        assert_eq!(decode_dib(&v5).unwrap().data, vec![30, 20, 10, 0x80]);
        // ... but an all-zero alpha channel means "opaque".
        let mut v5 = encode_dibv5(&Rgba {
            width: 1,
            height: 1,
            data: vec![30, 20, 10, 0],
        });
        v5[16..20].copy_from_slice(&BI_RGB.to_le_bytes());
        assert_eq!(decode_dib(&v5).unwrap().data, vec![30, 20, 10, 255]);
    }

    #[test]
    fn bitfields_info_header_trailing_masks() {
        // 565 16-bit, 2x1, trailing 3 masks. stride = 4.
        let mut d = header(2, 1, 16, BI_BITFIELDS, 0);
        for m in [0xF800u32, 0x07E0, 0x001F] {
            d.extend_from_slice(&m.to_le_bytes());
        }
        let red = 0xF800u16;
        let blue = 0x001Fu16;
        d.extend_from_slice(&red.to_le_bytes());
        d.extend_from_slice(&blue.to_le_bytes());
        let img = decode_dib(&d).unwrap();
        assert_eq!(img.data, vec![255, 0, 0, 255, 0, 0, 255, 255]);

        // 32-bit BI_BITFIELDS (RGBX masks swapped: R at low byte).
        let mut d = header(1, 1, 32, BI_BITFIELDS, 0);
        for m in [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000] {
            d.extend_from_slice(&m.to_le_bytes());
        }
        d.extend_from_slice(&[1, 2, 3, 0]);
        assert_eq!(decode_dib(&d).unwrap().data, vec![1, 2, 3, 255]);
    }

    #[test]
    fn palette_8bpp_and_1bpp() {
        // 8bpp, 2 colours used, width 3 -> stride 4.
        let mut d = header(3, 1, 8, BI_RGB, 0);
        d[32..36].copy_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]); // BGRX: red, green
        d.extend_from_slice(&[0, 1, 0, 0]);
        assert_eq!(
            decode_dib(&d).unwrap().data,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
        // 1bpp, width 9 -> stride 4, bits 1,0,1,0,0,0,0,0,1.
        let mut d = header(9, 1, 1, BI_RGB, 0);
        d.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]); // black, white
        d.extend_from_slice(&[0b1010_0000, 0b1000_0000, 0, 0]);
        let img = decode_dib(&d).unwrap();
        let first: Vec<u8> = img.data.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(first, vec![255, 0, 255, 0, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_dib(&[]).is_err());
        assert!(decode_dib(&[0; 40]).is_err());
        let mut d = header(2, 2, 24, BI_RGB, 0);
        d.extend_from_slice(&[0; 5]); // truncated pixels
        assert!(decode_dib(&d).is_err());
        assert!(decode_dib(&header(1, 1, 24, BI_RLE8, 8)).is_err());
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn png_variants_decode() {
        // Grayscale+alpha 16-bit and palette PNGs via the encoder.
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::GrayscaleAlpha);
            enc.set_depth(png::BitDepth::Sixteen);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0x80, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x40, 0x00])
                .unwrap();
        }
        let img = decode_png(&out).unwrap();
        assert_eq!(
            img.data,
            vec![0x80, 0x80, 0x80, 0xFF, 0x00, 0x00, 0x00, 0x40]
        );

        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_palette(vec![255, 0, 0, 0, 0, 255]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[1, 0]).unwrap();
        }
        assert_eq!(
            decode_png(&out).unwrap().data,
            vec![0, 0, 255, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn bi_png_dib_passes_through() {
        let png = encode_png(&gradient(2, 2, true)).unwrap();
        let mut d = header(2, 2, 0, BI_PNG, 0);
        d.extend_from_slice(&png);
        assert_eq!(dib_to_png(&d).unwrap(), png);
    }
}
