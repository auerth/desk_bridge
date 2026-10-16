//! Fractional-delay windowed-sinc interpolation used for clock-drift compensation.
//!
//! The receiver keeps a ring of decoded stereo frames and reads it with a read pointer
//! that advances by `ratio` input frames per output frame (`ratio` ~ 1 +- 0.2 %). A
//! 32-tap Kaiser-windowed sinc table with 1024 phases keeps the interpolation smooth
//! (no clicks when the ratio changes) and bit-exact at phase 0.

use std::sync::OnceLock;

/// Number of filter taps.
pub const TAPS: usize = 32;
/// Taps before the centre sample.
pub const LOOKBEHIND: usize = TAPS / 2 - 1;
/// Taps after the centre sample.
pub const LOOKAHEAD: usize = TAPS / 2;
/// Number of fractional phases in the table.
pub const PHASES: usize = 1024;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (f64::from(k) * f64::from(k));
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

fn build_table() -> Box<[f32]> {
    const BETA: f64 = 9.0;
    let half = (TAPS / 2) as f64;
    let norm = bessel_i0(BETA);
    let mut t = vec![0f32; PHASES * TAPS];
    for p in 0..PHASES {
        let frac = p as f64 / PHASES as f64;
        let mut row = [0f64; TAPS];
        let mut sum = 0.0;
        for (k, h) in row.iter_mut().enumerate() {
            let d = k as f64 - LOOKBEHIND as f64 - frac;
            let sinc = if d.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d)
            };
            let x = d / half;
            let w = if x.abs() >= 1.0 {
                0.0
            } else {
                bessel_i0(BETA * (1.0 - x * x).sqrt()) / norm
            };
            *h = sinc * w;
            sum += *h;
        }
        for (k, h) in row.iter().enumerate() {
            t[p * TAPS + k] = (*h / sum) as f32;
        }
    }
    t.into_boxed_slice()
}

/// Shared filter table (`PHASES` rows of `TAPS` coefficients).
#[must_use]
pub fn table() -> &'static [f32] {
    static T: OnceLock<Box<[f32]>> = OnceLock::new();
    T.get_or_init(build_table)
}

/// Maps a fractional position in `[0, 1)` to `(carry, row)`; `carry` is 1 when the
/// nearest table row is the next integer position.
#[inline]
#[must_use]
pub fn phase_row(frac: f64) -> (u64, usize) {
    let idx = (frac * PHASES as f64 + 0.5) as usize;
    if idx >= PHASES { (1, 0) } else { (0, idx) }
}

/// Interpolates one stereo frame around integer position `i0` of the ring `buf`
/// (interleaved stereo, `mask + 1` frames). Needs `LOOKBEHIND` frames before and
/// `LOOKAHEAD` frames after `i0` to be valid.
#[inline]
#[must_use]
pub fn interp(buf: &[f32], mask: usize, i0: u64, table: &[f32], row: usize) -> (f32, f32) {
    let coef = &table[row * TAPS..row * TAPS + TAPS];
    let start = (i0 as usize).wrapping_sub(LOOKBEHIND) & mask;
    let (mut l, mut r) = (0f32, 0f32);
    if start + TAPS <= mask + 1 {
        let s = &buf[start * 2..(start + TAPS) * 2];
        for (k, c) in coef.iter().enumerate() {
            l += c * s[2 * k];
            r += c * s[2 * k + 1];
        }
    } else {
        for (k, c) in coef.iter().enumerate() {
            let i = (start + k) & mask;
            l += c * buf[2 * i];
            r += c * buf[2 * i + 1];
        }
    }
    (l, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_zero_is_identity() {
        let t = table();
        let row = &t[0..TAPS];
        for (k, c) in row.iter().enumerate() {
            let want = if k == LOOKBEHIND { 1.0 } else { 0.0 };
            assert!((c - want).abs() < 1e-6, "tap {k} = {c}");
        }
    }

    #[test]
    fn rows_have_unit_dc_gain() {
        let t = table();
        for p in (0..PHASES).step_by(37) {
            let s: f32 = t[p * TAPS..(p + 1) * TAPS].iter().sum();
            assert!((s - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn fractional_delay_of_a_sine_is_accurate() {
        // Delay a 3 kHz sine by 0.37 samples; the error must stay tiny.
        let cap = 4096usize;
        let mask = cap - 1;
        let f = 3000.0 / 48000.0;
        let mut buf = vec![0f32; cap * 2];
        for i in 0..cap {
            let s = (2.0 * std::f64::consts::PI * f * i as f64).sin() as f32;
            buf[2 * i] = s;
            buf[2 * i + 1] = -s;
        }
        let t = table();
        let (carry, row) = phase_row(0.37);
        let mut max_err = 0f32;
        for i0 in 100..1000u64 {
            let (l, r) = interp(&buf, mask, i0 + carry, t, row);
            let want = (2.0 * std::f64::consts::PI * f * (i0 as f64 + 0.37)).sin() as f32;
            max_err = max_err.max((l - want).abs()).max((r + want).abs());
        }
        assert!(max_err < 2e-3, "max err {max_err}");
    }
}
