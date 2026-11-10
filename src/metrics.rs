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

fn luma(rgb: &[u8]) -> Vec<f64> {
    rgb.chunks_exact(3)
        .map(|p| 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64)
        .collect()
}

pub fn psnr_y(a: &[u8], b: &[u8]) -> f64 {
    let (ya, yb) = (luma(a), luma(b));
    let mse: f64 = ya
        .iter()
        .zip(&yb)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f64>()
        / ya.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// SSIM de luma con ventana gaussiana 11×11 (σ = 1.5).
pub fn ssim_y(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
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
