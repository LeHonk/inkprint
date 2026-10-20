//! Integer 8×8 inverse DCT, after stb_image's `stbi__idct_block` (public domain):
//! fixed point with 12 fractional bits, columns first with two extra bits of precision.

/// Zigzag position → natural (row-major) position.
pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47,
    55, 62, 63,
];

const fn f2f(x: f32) -> i32 {
    (x * 4096.0 + 0.5) as i32
}

/// One 1-D pass; returns (x0..x3, t0..t3) as in stb.
#[inline(always)]
fn idct_1d(s: [i32; 8]) -> ([i32; 4], [i32; 4]) {
    let p1 = (s[2] + s[6]) * f2f(0.541_196_1);
    let t2 = p1 + s[6] * f2f(-1.847_759_1);
    let t3 = p1 + s[2] * f2f(0.765_366_86);
    let t0 = (s[0] + s[4]) * 4096;
    let t1 = (s[0] - s[4]) * 4096;
    let x = [t0 + t3, t1 + t2, t1 - t2, t0 - t3];

    let (mut t0, mut t1, mut t2, mut t3) = (s[7], s[5], s[3], s[1]);
    let p3 = t0 + t2;
    let p4 = t1 + t3;
    let p1 = t0 + t3;
    let p2 = t1 + t2;
    let p5 = (p3 + p4) * f2f(1.175_875_6);
    t0 *= f2f(0.298_631_34);
    t1 *= f2f(2.053_12);
    t2 *= f2f(3.072_711);
    t3 *= f2f(1.501_321_1);
    let p1 = p5 + p1 * f2f(-0.899_976_2);
    let p2 = p5 + p2 * f2f(-2.562_915_3);
    let p3 = p3 * f2f(-1.961_570_6);
    let p4 = p4 * f2f(-0.390_180_64);
    t3 += p1 + p4;
    t2 += p2 + p3;
    t1 += p2 + p4;
    t0 += p1 + p3;
    (x, [t0, t1, t2, t3])
}

/// Dequantized coefficients (natural order) → 8×8 pixels written to `out` with `stride`.
pub(crate) fn idct_block(coef: &[i32; 64], out: &mut [u8], stride: usize) {
    let mut v = [0i32; 64];
    for col in 0..8 {
        let c = |row: usize| coef[row * 8 + col];
        if (1..8).all(|row| c(row) == 0) {
            let dc = c(0) * 4;
            for row in 0..8 {
                v[row * 8 + col] = dc;
            }
            continue;
        }
        let ([x0, x1, x2, x3], [t0, t1, t2, t3]) = idct_1d([c(0), c(1), c(2), c(3), c(4), c(5), c(6), c(7)]);
        let (x0, x1, x2, x3) = (x0 + 512, x1 + 512, x2 + 512, x3 + 512);
        v[col] = (x0 + t3) >> 10;
        v[56 + col] = (x0 - t3) >> 10;
        v[8 + col] = (x1 + t2) >> 10;
        v[48 + col] = (x1 - t2) >> 10;
        v[16 + col] = (x2 + t1) >> 10;
        v[40 + col] = (x2 - t1) >> 10;
        v[24 + col] = (x3 + t0) >> 10;
        v[32 + col] = (x3 - t0) >> 10;
    }
    for row in 0..8 {
        let r = &v[row * 8..row * 8 + 8];
        let ([x0, x1, x2, x3], [t0, t1, t2, t3]) = idct_1d([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]);
        // Undo 1<<17 of scaling with rounding, and the level shift of 128.
        let bias = 65_536 + (128 << 17);
        let (x0, x1, x2, x3) = (x0 + bias, x1 + bias, x2 + bias, x3 + bias);
        let clamp = |v: i32| (v >> 17).clamp(0, 255) as u8;
        let o = &mut out[row * stride..row * stride + 8];
        o[0] = clamp(x0 + t3);
        o[7] = clamp(x0 - t3);
        o[1] = clamp(x1 + t2);
        o[6] = clamp(x1 - t2);
        o[2] = clamp(x2 + t1);
        o[5] = clamp(x2 - t1);
        o[3] = clamp(x3 + t0);
        o[4] = clamp(x3 - t0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference: the textbook floating-point IDCT.
    fn reference(coef: &[i32; 64]) -> [u8; 64] {
        let mut out = [0u8; 64];
        let c = |u: usize| if u == 0 { core::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0.0;
                for v in 0..8 {
                    for u in 0..8 {
                        let cos = |a: usize, b: usize| (((2 * a + 1) * b) as f64 * core::f64::consts::PI / 16.0).cos();
                        s += c(u) * c(v) * f64::from(coef[v * 8 + u]) * cos(x, u) * cos(y, v);
                    }
                }
                out[y * 8 + x] = (s / 4.0 + 128.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        out
    }

    #[test]
    fn matches_float_idct() {
        // A deterministic spread of coefficient patterns, including DC-only columns.
        let mut seed = 12345u32;
        for case in 0..200 {
            let mut coef = [0i32; 64];
            for (i, c) in coef.iter_mut().enumerate() {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let keep = case % 4 == 0 || i < 10 + case % 30;
                *c = if keep { ((seed >> 16) % 400) as i32 - 200 } else { 0 };
            }
            coef[0] = (case as i32 * 37) % 1600 - 800;
            let mut out = [0u8; 64];
            idct_block(&coef, &mut out, 8);
            let want = reference(&coef);
            for i in 0..64 {
                assert!(out[i].abs_diff(want[i]) <= 1, "case {case} pixel {i}: {} vs {}", out[i], want[i]);
            }
        }
    }
}
