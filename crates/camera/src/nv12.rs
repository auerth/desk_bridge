//! NV12 helpers (BT.709, limited range: what Media Foundation assumes for HD camera pictures).

/// Bytes of an NV12 picture with stride = width.
#[must_use]
pub const fn size(width: u32, height: u32) -> usize {
    let px = width as usize * height as usize;
    px + px / 2
}

#[inline]
fn y_of(r: i32, g: i32, b: i32) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8
}

#[inline]
fn uv_of(r: i32, g: i32, b: i32) -> (u8, u8) {
    let u = ((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

/// Converts a BGRA picture (`stride` bytes per row) to NV12 of `dw` x `dh` (both even) into
/// `dst` (at least [`size`] bytes), scaling with nearest-neighbour sampling when the sizes
/// differ (the stream is normally already captured at the size the camera app asked for).
pub fn from_bgra(src: &[u8], sw: u32, sh: u32, stride: usize, dst: &mut [u8], dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || dst.len() < size(dw as u32, dh as u32) {
        return;
    }
    let xs: Vec<usize> = (0..dw).map(|x| (x * sw / dw).min(sw - 1) * 4).collect();
    let px = |row: usize, xo: usize| -> (i32, i32, i32) {
        let o = row * stride + xo;
        match src.get(o..o + 3) {
            Some(p) => (i32::from(p[2]), i32::from(p[1]), i32::from(p[0])),
            None => (0, 0, 0),
        }
    };
    let (luma, chroma) = dst.split_at_mut(dw * dh);
    for y in (0..dh).step_by(2) {
        let r0 = (y * sh / dh).min(sh - 1);
        let r1 = ((y + 1) * sh / dh).min(sh - 1);
        let (l0, rest) = luma[y * dw..].split_at_mut(dw);
        let l1 = &mut rest[..dw];
        let c = &mut chroma[(y / 2) * dw..(y / 2) * dw + dw];
        for x in (0..dw).step_by(2) {
            let a = px(r0, xs[x]);
            let b = px(r0, xs[x + 1]);
            let cc = px(r1, xs[x]);
            let d = px(r1, xs[x + 1]);
            l0[x] = y_of(a.0, a.1, a.2);
            l0[x + 1] = y_of(b.0, b.1, b.2);
            l1[x] = y_of(cc.0, cc.1, cc.2);
            l1[x + 1] = y_of(d.0, d.1, d.2);
            let (u, v) = uv_of(
                (a.0 + b.0 + cc.0 + d.0 + 2) >> 2,
                (a.1 + b.1 + cc.1 + d.1 + 2) >> 2,
                (a.2 + b.2 + cc.2 + d.2 + 2) >> 2,
            );
            c[x] = u;
            c[x + 1] = v;
        }
    }
}

/// Scales an NV12 picture (nearest neighbour) from `sw` x `sh` to `dw` x `dh` (all even).
pub fn scale(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw == 0
        || sh == 0
        || src.len() < size(sw as u32, sh as u32)
        || dst.len() < size(dw as u32, dh as u32)
    {
        return;
    }
    if (sw, sh) == (dw, dh) {
        let n = size(dw as u32, dh as u32);
        dst[..n].copy_from_slice(&src[..n]);
        return;
    }
    let (sl, sc) = src.split_at(sw * sh);
    let (dl, dc) = dst.split_at_mut(dw * dh);
    let xs: Vec<usize> = (0..dw).map(|x| (x * sw / dw).min(sw - 1)).collect();
    for y in 0..dh {
        let sy = (y * sh / dh).min(sh - 1);
        let srow = &sl[sy * sw..sy * sw + sw];
        let drow = &mut dl[y * dw..y * dw + dw];
        for (d, &sx) in drow.iter_mut().zip(&xs) {
            *d = srow[sx];
        }
    }
    let (cw, ch, scw, sch) = (dw / 2, dh / 2, sw / 2, sh / 2);
    for y in 0..ch {
        let sy = (y * sch / ch).min(sch - 1);
        for x in 0..cw {
            let sx = (x * scw / cw).min(scw - 1);
            dc[y * dw + x * 2] = sc[sy * sw + sx * 2];
            dc[y * dw + x * 2 + 1] = sc[sy * sw + sx * 2 + 1];
        }
    }
}

/// Fills an NV12 picture with one colour (given as RGB).
pub fn fill_rgb(dst: &mut [u8], w: u32, h: u32, rgb: (u8, u8, u8)) {
    let n = (w * h) as usize;
    if dst.len() < size(w, h) {
        return;
    }
    let (r, g, b) = (i32::from(rgb.0), i32::from(rgb.1), i32::from(rgb.2));
    let yv = y_of(r, g, b);
    let (u, v) = uv_of(r, g, b);
    dst[..n].fill(yv);
    for c in dst[n..size(w, h)].chunks_exact_mut(2) {
        c[0] = u;
        c[1] = v;
    }
}

/// Fills a rectangle (even coordinates) of an NV12 picture with one colour.
pub fn fill_rect_rgb(
    dst: &mut [u8],
    w: u32,
    h: u32,
    rect: (u32, u32, u32, u32),
    rgb: (u8, u8, u8),
) {
    let (w, h) = (w as usize, h as usize);
    if dst.len() < size(w as u32, h as u32) {
        return;
    }
    let x0 = (rect.0 as usize & !1).min(w);
    let y0 = (rect.1 as usize & !1).min(h);
    let x1 = ((rect.0 + rect.2) as usize & !1).min(w);
    let y1 = ((rect.1 + rect.3) as usize & !1).min(h);
    let (r, g, b) = (i32::from(rgb.0), i32::from(rgb.1), i32::from(rgb.2));
    let yv = y_of(r, g, b);
    let (u, v) = uv_of(r, g, b);
    let (luma, chroma) = dst.split_at_mut(w * h);
    for y in y0..y1 {
        luma[y * w + x0..y * w + x1].fill(yv);
    }
    for y in (y0 / 2)..(y1 / 2) {
        for x in (x0 / 2)..(x1 / 2) {
            chroma[y * w + x * 2] = u;
            chroma[y * w + x * 2 + 1] = v;
        }
    }
}

/// Brand colours of the placeholder.
pub const DEEP_TRUST: (u8, u8, u8) = (0x1D, 0x2B, 0x4F);
pub const SIGNAL_ORANGE: (u8, u8, u8) = (0xF1, 0x85, 0x57);
const LIGHT: (u8, u8, u8) = (0xC9, 0xD2, 0xE3);

/// The picture the virtual camera shows without a stream: a simple camera symbol on the brand
/// background, with a dot that is orange while the PC's webcam is starting and grey otherwise.
pub fn placeholder(dst: &mut [u8], w: u32, h: u32, waiting: bool) {
    fill_rgb(dst, w, h, DEEP_TRUST);
    let u = (h / 12).max(2);
    let (cx, cy) = (w / 2, h / 2);
    // body
    fill_rect_rgb(dst, w, h, (cx - 3 * u, cy - 2 * u, 5 * u, 4 * u), LIGHT);
    // lens opening
    fill_rect_rgb(dst, w, h, (cx - 2 * u, cy - u, 3 * u, 2 * u), DEEP_TRUST);
    // viewfinder flap
    fill_rect_rgb(dst, w, h, (cx + 2 * u, cy - u, 2 * u, 2 * u), LIGHT);
    // status dot
    let dot = if waiting {
        SIGNAL_ORANGE
    } else {
        (0x6B, 0x75, 0x8A)
    };
    fill_rect_rgb(dst, w, h, (cx - u / 2, cy + 3 * u, u, u), dot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        // white, black and the three primaries (BT.709 limited range)
        assert_eq!(y_of(255, 255, 255), 235);
        assert_eq!(y_of(0, 0, 0), 16);
        assert_eq!(uv_of(128, 128, 128), (128, 128));
        let (u, v) = uv_of(255, 0, 0);
        assert!(u < 128 && v > 230, "red: u {u} v {v}");
        let (u, _) = uv_of(0, 0, 255);
        assert!(u > 230, "blue: u {u}");
    }

    #[test]
    fn bgra_same_size() {
        let (w, h) = (4u32, 2u32);
        // BGRA white everywhere
        let src = vec![255u8; (w * h * 4) as usize];
        let mut dst = vec![0u8; size(w, h)];
        from_bgra(&src, w, h, (w * 4) as usize, &mut dst, w, h);
        assert!(dst[..8].iter().all(|&y| y == 235));
        assert!(dst[8..].iter().all(|&c| c == 128));
    }

    #[test]
    fn bgra_scaled_and_padded_stride() {
        let (sw, sh, stride) = (8u32, 4u32, 40usize);
        let mut src = vec![0u8; stride * sh as usize];
        // left half red, right half blue
        for y in 0..sh as usize {
            for x in 0..sw as usize {
                let p = &mut src[y * stride + x * 4..y * stride + x * 4 + 4];
                if x < 4 {
                    p.copy_from_slice(&[0, 0, 255, 255]);
                } else {
                    p.copy_from_slice(&[255, 0, 0, 255]);
                }
            }
        }
        let mut dst = vec![0u8; size(4, 2)];
        from_bgra(&src, sw, sh, stride, &mut dst, 4, 2);
        let red_y = y_of(255, 0, 0);
        let blue_y = y_of(0, 0, 255);
        assert_eq!(&dst[..4], &[red_y, red_y, blue_y, blue_y]);
    }

    #[test]
    fn scale_down_and_identity() {
        let (sw, sh) = (4u32, 4u32);
        let mut src = vec![0u8; size(sw, sh)];
        for (i, b) in src.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut same = vec![0u8; size(sw, sh)];
        scale(&src, sw, sh, &mut same, sw, sh);
        assert_eq!(same, src);
        let mut half = vec![0u8; size(2, 2)];
        scale(&src, sw, sh, &mut half, 2, 2);
        assert_eq!(&half[..4], &[0, 2, 8, 10]);
        assert_eq!(&half[4..], &[16, 17]);
    }

    #[test]
    fn placeholder_draws() {
        let (w, h) = (128u32, 72u32);
        let mut dst = vec![0u8; size(w, h)];
        placeholder(&mut dst, w, h, true);
        let bg = y_of(0x1D, 0x2B, 0x4F);
        assert_eq!(dst[0], bg);
        let centre = (h / 2 - 2 * (h / 12) + 1) * w + w / 2 - 3 * (h / 12) + 1;
        assert_ne!(dst[centre as usize], bg, "the camera body is drawn");
    }
}
