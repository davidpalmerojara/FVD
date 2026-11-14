//! Conversión RGB ↔ YCbCr (BT.601, rango completo) y submuestreo 4:2:0
//! con muestras de croma centradas.

/// `v.round().clamp(0.0, 255.0) as u8` sin la llamada a `roundf` (sin SSE4.1
/// es una rutina por software): se acota primero (no cambia el resultado) y,
/// ya en [0, 255], el redondeo "mitad hacia fuera" es truncar y sumar 1 si la
/// parte fraccionaria (exacta) es ≥ ½. NaN da 0, igual que antes.
#[inline(always)]
pub fn round_u8(v: f32) -> u8 {
    let c = v.clamp(0.0, 255.0);
    let t = c as i32;
    (t + ((c - t as f32) >= 0.5) as i32) as u8
}

pub struct Ycc {
    /// (datos, ancho, alto) para Y, Cb, Cr.
    pub planes: Vec<(Vec<f32>, usize, usize)>,
}

pub fn rgb_to_ycbcr(rgb: &[u8], w: usize, h: usize, sub: bool) -> Ycc {
    let n = w * h;
    let mut y = vec![0f32; n];
    let mut cb = vec![0f32; n];
    let mut cr = vec![0f32; n];
    for i in 0..n {
        let r = rgb[3 * i] as f32;
        let g = rgb[3 * i + 1] as f32;
        let b = rgb[3 * i + 2] as f32;
        y[i] = 0.299 * r + 0.587 * g + 0.114 * b;
        cb[i] = 128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b;
        cr[i] = 128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    }
    if !sub {
        return Ycc {
            planes: vec![(y, w, h), (cb, w, h), (cr, w, h)],
        };
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let down = |p: &[f32]| {
        let mut o = vec![0f32; cw * ch];
        for yy in 0..ch {
            for xx in 0..cw {
                let mut s = 0f32;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (2 * xx + dx).min(w - 1);
                    let sy = (2 * yy + dy).min(h - 1);
                    s += p[sy * w + sx];
                }
                o[yy * cw + xx] = s * 0.25;
            }
        }
        o
    };
    let cb = down(&cb);
    let cr = down(&cr);
    Ycc {
        planes: vec![(y, w, h), (cb, cw, ch), (cr, cw, ch)],
    }
}

/// Interpolación bilineal 2× de un plano de croma (muestras centradas).
fn upsample(c: &[u8], stride: usize, cw: usize, ch: usize, w: usize, h: usize) -> Vec<f32> {
    let get = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, cw as isize - 1) as usize;
        let y = y.clamp(0, ch as isize - 1) as usize;
        c[y * stride + x] as f32
    };
    // horizontal
    let mut tmp = vec![0f32; w * ch];
    for y in 0..ch {
        for x in 0..w {
            let i = (x / 2) as isize;
            let o = if x % 2 == 0 { i - 1 } else { i + 1 };
            tmp[y * w + x] = 0.75 * get(i, y as isize) + 0.25 * get(o, y as isize);
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let i = y / 2;
        let o = if y % 2 == 0 {
            i.saturating_sub(1)
        } else {
            (i + 1).min(ch - 1)
        };
        for x in 0..w {
            out[y * w + x] = 0.75 * tmp[i * w + x] + 0.25 * tmp[o * w + x];
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn ycbcr_to_rgb(
    y: &[u8],
    ystride: usize,
    cb: &[u8],
    cr: &[u8],
    cstride: usize,
    w: usize,
    h: usize,
    sub: bool,
) -> Vec<u8> {
    let (cbf, crf): (Vec<f32>, Vec<f32>) = if sub {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        (
            upsample(cb, cstride, cw, ch, w, h),
            upsample(cr, cstride, cw, ch, w, h),
        )
    } else {
        let mut a = vec![0f32; w * h];
        let mut b = vec![0f32; w * h];
        for yy in 0..h {
            for xx in 0..w {
                a[yy * w + xx] = cb[yy * cstride + xx] as f32;
                b[yy * w + xx] = cr[yy * cstride + xx] as f32;
            }
        }
        (a, b)
    };
    let mut out = vec![0u8; w * h * 3];
    for yy in 0..h {
        for xx in 0..w {
            let i = yy * w + xx;
            let l = y[yy * ystride + xx] as f32;
            let u = cbf[i] - 128.0;
            let v = crf[i] - 128.0;
            let r = l + 1.402 * v;
            let g = l - 0.344_136 * u - 0.714_136 * v;
            let b = l + 1.772 * u;
            out[3 * i] = round_u8(r);
            out[3 * i + 1] = round_u8(g);
            out[3 * i + 2] = round_u8(b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `round_u8` coincide con `round().clamp()` en un barrido fino, los
    /// empates .5 y los valores extremos.
    #[test]
    fn round_u8_exacto() {
        let mut vals = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0, 1e30, -1e30];
        let near = |v: f32| [v, f32::from_bits(v.to_bits() + 1), f32::from_bits(v.to_bits() - 1)];
        for i in 1..=52000i32 {
            vals.extend(near(i as f32 * 0.005));
            vals.extend(near(-(i as f32) * 0.005));
        }
        for k in 0..=256 {
            vals.extend(near(k as f32 + 0.5));
        }
        for v in vals {
            assert_eq!(round_u8(v), v.round().clamp(0.0, 255.0) as u8, "{v}");
        }
    }
}
