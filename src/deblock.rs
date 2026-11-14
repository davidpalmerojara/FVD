//! Filtro de desbloqueo (post-proceso, fuera del bucle de predicción).
//! Suaviza las fronteras de bloques de transformada cuando la discontinuidad
//! es pequeña respecto al paso de cuantización (es decir, probablemente un
//! artefacto y no un borde real de la imagen).

use crate::predict::Plane;

pub fn filter(p: &mut Plane, q: f32, strength: u8) {
    let s = strength as f32 / 8.0;
    let alpha = (q * 1.5 * s).max(1.0);
    let beta = (q * 0.4 * s).max(0.5);
    let tc = (q * 0.25 * s).max(0.5);
    let (w, h) = (p.w, p.h);
    let mut d: Vec<f32> = p.data.iter().map(|&v| v as f32).collect();
    let is_edge_x = |p: &Plane, gx: usize, gy: usize| {
        let x = gx * 4;
        let a = p.sizes[gy * p.gw + gx] as usize;
        let b = p.sizes[gy * p.gw + gx - 1] as usize;
        x % a == 0 || x % b == 0
    };
    let is_edge_y = |p: &Plane, gx: usize, gy: usize| {
        let y = gy * 4;
        let a = p.sizes[gy * p.gw + gx] as usize;
        let b = p.sizes[(gy - 1) * p.gw + gx] as usize;
        y % a == 0 || y % b == 0
    };
    // Bordes verticales
    for gy in 0..p.gh {
        for gx in 1..p.gw {
            if !is_edge_x(p, gx, gy) {
                continue;
            }
            let big = p.sizes[gy * p.gw + gx].min(p.sizes[gy * p.gw + gx - 1]) >= 16;
            for y in gy * 4..gy * 4 + 4 {
                let base = y * w + gx * 4;
                filter_line(&mut d, base, 1, alpha, beta, tc, big);
            }
        }
    }
    // Bordes horizontales
    for gy in 1..p.gh {
        for gx in 0..p.gw {
            if !is_edge_y(p, gx, gy) {
                continue;
            }
            let big = p.sizes[gy * p.gw + gx].min(p.sizes[(gy - 1) * p.gw + gx]) >= 16;
            for x in gx * 4..gx * 4 + 4 {
                let base = gy * 4 * w + x;
                filter_line(&mut d, base, w, alpha, beta, tc, big);
            }
        }
    }
    let _ = h;
    for (o, v) in p.data.iter_mut().zip(&d) {
        *o = crate::color::round_u8(*v);
    }
}

#[inline]
fn filter_line(d: &mut [f32], q0i: usize, step: usize, alpha: f32, beta: f32, tc: f32, big: bool) {
    let p0 = d[q0i - step];
    let p1 = d[q0i - 2 * step];
    let p2 = d[q0i - 3 * step];
    let q0 = d[q0i];
    let q1 = d[q0i + step];
    let q2 = d[q0i + 2 * step];
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if big && (p2 - p0).abs() < beta && (q2 - q0).abs() < beta && (p0 - q0).abs() < alpha * 0.5 {
        // Filtro fuerte para bloques grandes y planos.
        d[q0i - step] = (p2 + 2.0 * p1 + 2.0 * p0 + 2.0 * q0 + q1) / 8.0;
        d[q0i - 2 * step] = (p2 + p1 + p0 + q0) / 4.0;
        d[q0i] = (p1 + 2.0 * p0 + 2.0 * q0 + 2.0 * q1 + q2) / 8.0;
        d[q0i + step] = (p0 + q0 + q1 + q2) / 4.0;
        return;
    }
    let delta = ((4.0 * (q0 - p0) + (p1 - q1)) / 8.0).clamp(-tc, tc);
    d[q0i - step] = p0 + delta;
    d[q0i] = q0 - delta;
}
