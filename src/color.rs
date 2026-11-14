//! Conversión RGB ↔ YCbCr (BT.601, rango completo) y submuestreo 4:2:0
//! con muestras de croma centradas.

/// `v.round().clamp(0.0, 255.0) as u8` sin la llamada a `roundf` (sin SSE4.1
/// es una rutina por software). NaN da 0, igual que antes. Se acota primero
/// (no cambia el resultado) y, ya en [0, 255], el redondeo "mitad hacia
/// fuera" es el suelo más 1 si la parte fraccionaria (exacta) es ≥ ½.
///
/// En x86 se usa una versión sin conversión float→entero con saturación (el
/// compilador no sabe vectorizarla): el suelo sale de sumar y restar 2²³ y el
/// byte se lee de la mantisa. En WebAssembly (sin SIMD) la conversión
/// saturante es una sola instrucción y se usa esa.
#[inline(always)]
pub fn round_u8(v: f32) -> u8 {
    #[cfg(target_arch = "wasm32")]
    {
        round_u8_trunc(v)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        round_u8_bits(v)
    }
}

#[inline(always)]
#[allow(dead_code)]
fn round_u8_trunc(v: f32) -> u8 {
    let c = v.clamp(0.0, 255.0);
    let t = c as i32;
    (t + ((c - t as f32) >= 0.5) as i32) as u8
}

#[inline(always)]
#[allow(dead_code)]
fn round_u8_bits(v: f32) -> u8 {
    // c en [0, 255] (NaN → 0)
    let c = if v > 0.0 {
        if v < 255.0 { v } else { 255.0 }
    } else {
        0.0
    };
    const M: f32 = 8_388_608.0; // 2^23
    let r = (c + M) - M; // redondeo al par
    let fl = r - if r > c { 1.0 } else { 0.0 };
    let res = fl + if c - fl >= 0.5 { 1.0 } else { 0.0 };
    // res es un entero en [0, 255]: sumando 2^23 queda en la mantisa.
    ((res + M).to_bits() & 0xFF) as u8
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

/// Primera pasada (horizontal) de la interpolación bilineal 2× de un plano de
/// croma (muestras centradas): `ch` filas de `w` valores.
fn upsample_h(c: &[u8], stride: usize, cw: usize, ch: usize, w: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * ch];
    let mut cf = vec![0f32; cw];
    for y in 0..ch {
        for (d, &v) in cf.iter_mut().zip(&c[y * stride..y * stride + cw]) {
            *d = v as f32;
        }
        let row = &mut tmp[y * w..y * w + w];
        for i in 0..cw {
            // Columna par 2i (vecino i-1) e impar 2i+1 (vecino i+1), con
            // los extremos replicados.
            let m = cf[i];
            row[2 * i] = 0.75 * m + 0.25 * cf[i.saturating_sub(1)];
            if 2 * i + 1 < w {
                row[2 * i + 1] = 0.75 * m + 0.25 * cf[(i + 1).min(cw - 1)];
            }
        }
    }
    tmp
}

/// Segunda pasada (vertical) para la fila de salida `y`.
fn upsample_v(tmp: &[f32], w: usize, ch: usize, y: usize, out: &mut [f32]) {
    let i = y / 2;
    let o = if y % 2 == 0 {
        i.saturating_sub(1)
    } else {
        (i + 1).min(ch - 1)
    };
    let (a, b) = (&tmp[i * w..i * w + w], &tmp[o * w..o * w + w]);
    for ((d, &p), &q) in out.iter_mut().zip(a).zip(b) {
        *d = 0.75 * p + 0.25 * q;
    }
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
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    // Con submuestreo, la croma se interpola en horizontal de una vez y en
    // vertical fila a fila (sin planos de croma completos en f32).
    let (tb, tr) = if sub {
        (
            upsample_h(cb, cstride, cw, ch, w),
            upsample_h(cr, cstride, cw, ch, w),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let mut out = vec![0u8; w * h * 3];
    let (mut ub, mut vb) = (vec![0f32; w], vec![0f32; w]);
    for yy in 0..h {
        if sub {
            upsample_v(&tb, w, ch, yy, &mut ub);
            upsample_v(&tr, w, ch, yy, &mut vb);
        } else {
            for (d, &v) in ub.iter_mut().zip(&cb[yy * cstride..yy * cstride + w]) {
                *d = v as f32;
            }
            for (d, &v) in vb.iter_mut().zip(&cr[yy * cstride..yy * cstride + w]) {
                *d = v as f32;
            }
        }
        let yrow = &y[yy * ystride..yy * ystride + w];
        let orow = &mut out[3 * yy * w..3 * (yy + 1) * w];
        for (((px, &l), &cbv), &crv) in orow.chunks_exact_mut(3).zip(yrow).zip(&ub).zip(&vb) {
            let l = l as f32;
            let u = cbv - 128.0;
            let v = crv - 128.0;
            let r = l + 1.402 * v;
            let g = l - 0.344_136 * u - 0.714_136 * v;
            let b = l + 1.772 * u;
            px[0] = round_u8(r);
            px[1] = round_u8(g);
            px[2] = round_u8(b);
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
            let e = v.round().clamp(0.0, 255.0) as u8;
            assert_eq!(round_u8_bits(v), e, "{v}");
            assert_eq!(round_u8_trunc(v), e, "{v}");
        }
    }
}
