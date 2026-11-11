//! Métricas de calidad: PSNR (RGB y luma), SSIM (luma) y SSIMULACRA2.

use ssimulacra2::{ColorPrimaries, Rgb, TransferCharacteristic, compute_frame_ssimulacra2};

pub fn psnr_rgb(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[cfg(test)]
fn luma(rgb: &[u8]) -> Vec<f64> {
    rgb.chunks_exact(3)
        .map(|p| 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64)
        .collect()
}

pub fn psnr_y(a: &[u8], b: &[u8]) -> f64 {
    let l = |p: &[u8]| 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
    let mse: f64 = a
        .chunks_exact(3)
        .zip(b.chunks_exact(3))
        .map(|(x, y)| (l(x) - l(y)).powi(2))
        .sum::<f64>()
        / (a.len() / 3) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// SSIM de luma con ventana gaussiana 11×11 (σ = 1.5).
///
/// Se calcula por filas: el desenfoque horizontal de cada fila se guarda en
/// un anillo de 11 filas y el vertical se hace al vuelo. Mismas operaciones
/// y en el mismo orden que con imágenes completas (mismo resultado exacto),
/// pero con memoria proporcional al ancho en lugar de ~96 bytes por píxel.
pub fn ssim_y(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    let g: Vec<f64> = (-5..=5)
        .map(|i: i32| (-(i * i) as f64 / (2.0 * 1.5 * 1.5)).exp())
        .collect();
    let gs: f64 = g.iter().sum();
    let g: Vec<f64> = g.iter().map(|v| v / gs).collect();
    let lum = |rgb: &[u8], i: usize| {
        0.299 * rgb[3 * i] as f64 + 0.587 * rgb[3 * i + 1] as f64 + 0.114 * rgb[3 * i + 2] as f64
    };
    // Anillo: fila de origen y sus 5 magnitudes desenfocadas en horizontal
    // (x, y, x², y², xy), cada una de ancho w.
    const R: usize = 11;
    let mut ring_row = [usize::MAX; R];
    let mut ring = vec![0f64; R * 5 * w];
    let (mut x, mut y) = (vec![0f64; w], vec![0f64; w]);
    let (mut xx, mut yy, mut xy) = (vec![0f64; w], vec![0f64; w], vec![0f64; w]);
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let mut acc = 0.0;
    let mut out = [
        vec![0f64; w],
        vec![0f64; w],
        vec![0f64; w],
        vec![0f64; w],
        vec![0f64; w],
    ];
    for oy in 0..h {
        for k in 0..g.len() {
            let sy = (oy as isize + k as isize - 5).clamp(0, h as isize - 1) as usize;
            let slot = sy % R;
            if ring_row[slot] == sy {
                continue;
            }
            ring_row[slot] = sy;
            for c in 0..w {
                x[c] = lum(a, sy * w + c);
                y[c] = lum(b, sy * w + c);
                xx[c] = x[c] * x[c];
                yy[c] = y[c] * y[c];
                xy[c] = x[c] * y[c];
            }
            for (q, src) in [&x, &y, &xx, &yy, &xy].into_iter().enumerate() {
                let dst = &mut ring[(slot * 5 + q) * w..(slot * 5 + q + 1) * w];
                for c in 0..w {
                    let mut s = 0.0;
                    for (k, &gk) in g.iter().enumerate() {
                        let sx = (c as isize + k as isize - 5).clamp(0, w as isize - 1) as usize;
                        s += gk * src[sx];
                    }
                    dst[c] = s;
                }
            }
        }
        for (q, o) in out.iter_mut().enumerate() {
            for (c, v) in o.iter_mut().enumerate() {
                let mut s = 0.0;
                for (k, &gk) in g.iter().enumerate() {
                    let sy = (oy as isize + k as isize - 5).clamp(0, h as isize - 1) as usize;
                    s += gk * ring[((sy % R) * 5 + q) * w + c];
                }
                *v = s;
            }
        }
        let [mx, my, sxx, syy, sxy] = &out;
        for c in 0..w {
            let (vx, vy, cxy) = (
                sxx[c] - mx[c] * mx[c],
                syy[c] - my[c] * my[c],
                sxy[c] - mx[c] * my[c],
            );
            acc += ((2.0 * mx[c] * my[c] + c1) * (2.0 * cxy + c2))
                / ((mx[c] * mx[c] + my[c] * my[c] + c1) * (vx + vy + c2));
        }
    }
    acc / (w * h) as f64
}

/// Versión con imágenes completas (la original), para comprobar la de filas.
#[cfg(test)]
fn ssim_y_full(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    let (x, y) = (luma(a), luma(b));
    let g: Vec<f64> = (-5..=5)
        .map(|i: i32| (-(i * i) as f64 / (2.0 * 1.5 * 1.5)).exp())
        .collect();
    let gs: f64 = g.iter().sum();
    let g: Vec<f64> = g.iter().map(|v| v / gs).collect();
    let blur = |src: &[f64]| -> Vec<f64> {
        let mut t = vec![0f64; w * h];
        for yy in 0..h {
            for xx in 0..w {
                let mut s = 0.0;
                for (k, &gk) in g.iter().enumerate() {
                    let sx = (xx as isize + k as isize - 5).clamp(0, w as isize - 1) as usize;
                    s += gk * src[yy * w + sx];
                }
                t[yy * w + xx] = s;
            }
        }
        let mut o = vec![0f64; w * h];
        for yy in 0..h {
            for xx in 0..w {
                let mut s = 0.0;
                for (k, &gk) in g.iter().enumerate() {
                    let sy = (yy as isize + k as isize - 5).clamp(0, h as isize - 1) as usize;
                    s += gk * t[sy * w + xx];
                }
                o[yy * w + xx] = s;
            }
        }
        o
    };
    let xx: Vec<f64> = x.iter().map(|v| v * v).collect();
    let yy: Vec<f64> = y.iter().map(|v| v * v).collect();
    let xy: Vec<f64> = x.iter().zip(&y).map(|(a, b)| a * b).collect();
    let (mx, my, sxx, syy, sxy) = (blur(&x), blur(&y), blur(&xx), blur(&yy), blur(&xy));
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let mut acc = 0.0;
    for i in 0..w * h {
        let (vx, vy, cxy) = (
            sxx[i] - mx[i] * mx[i],
            syy[i] - my[i] * my[i],
            sxy[i] - mx[i] * my[i],
        );
        acc += ((2.0 * mx[i] * my[i] + c1) * (2.0 * cxy + c2))
            / ((mx[i] * mx[i] + my[i] * my[i] + c1) * (vx + vy + c2));
    }
    acc / (w * h) as f64
}

pub fn ssimulacra2(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    let conv = |d: &[u8]| {
        let px: Vec<[f32; 3]> = d
            .chunks_exact(3)
            .map(|p| {
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]
            })
            .collect();
        Rgb::new(
            px,
            w,
            h,
            TransferCharacteristic::SRGB,
            ColorPrimaries::BT709,
        )
        .expect("rgb")
    };
    compute_frame_ssimulacra2(conv(a), conv(b)).unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ssim_por_filas_igual_que_completo() {
        for (w, h) in [(1, 1), (7, 3), (16, 16), (37, 23)] {
            let a: Vec<u8> = (0..w * h * 3).map(|i| (i * 37 % 251) as u8).collect();
            let b: Vec<u8> = a
                .iter()
                .enumerate()
                .map(|(i, &v)| v.wrapping_add((i % 7) as u8))
                .collect();
            assert_eq!(
                super::ssim_y(&a, &b, w, h).to_bits(),
                super::ssim_y_full(&a, &b, w, h).to_bits()
            );
        }
    }
}
