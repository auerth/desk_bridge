//! Raw PCM packing: interleaved `f32` <-> little-endian `i16`.

/// Appends `samples` as little-endian `i16` (rounded, clipped, no dithering).
pub fn pcm_encode(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for &s in samples {
        let v = (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// Appends the decoded `f32` samples of `bytes`; a trailing odd byte is ignored.
pub fn pcm_decode(bytes: &[u8], out: &mut Vec<f32>) {
    out.reserve(bytes.len() / 2);
    for c in bytes.chunks_exact(2) {
        out.push(f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_clipping() {
        let src = [0.0, 0.5, -0.5, 1.5, -1.5, 0.123_456];
        let mut b = Vec::new();
        pcm_encode(&src, &mut b);
        assert_eq!(b.len(), 12);
        let mut o = Vec::new();
        pcm_decode(&b, &mut o);
        assert_eq!(o.len(), 6);
        for (a, b) in src.iter().zip(&o) {
            let want = a.clamp(-1.0, 32767.0 / 32768.0);
            assert!((want - b).abs() <= 1.0 / 32768.0, "{a} -> {b}");
        }
        assert_eq!(o[4], -1.0);
    }

    #[test]
    fn odd_trailing_byte_ignored() {
        let mut o = Vec::new();
        pcm_decode(&[1, 0, 9], &mut o);
        assert_eq!(o.len(), 1);
    }
}
