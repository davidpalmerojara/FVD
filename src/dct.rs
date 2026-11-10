//! DCT-II ortonormal separable para bloques cuadrados de 4 a 64.
//! Al ser ortonormal, el error cuadrático en el dominio transformado es igual
//! al del dominio de píxeles, lo que simplifica el RDO.

use std::sync::OnceLock;

pub const SIZES: [usize; 5] = [4, 8, 16, 32, 64];

pub fn size_index(n: usize) -> usize {
    match n {
        4 => 0,
        8 => 1,
        16 => 2,
        32 => 3,
        64 => 4,
        _ => panic!("tamaño de bloque no soportado: {n}"),
    }
}

/// Factores 1/(2·cos((i+½)π/n)) para cada nivel de la recursión de Lee
/// (n = 2, 4, …, 64), concatenados: el nivel n empieza en `n/2 - 1`.
fn lee_factors() -> &'static [f32] {
    static F: OnceLock<Vec<f32>> = OnceLock::new();
    F.get_or_init(|| {
        let mut v = Vec::new();
        let mut n = 2;
        while n <= 64 {
            for i in 0..n / 2 {
                let c = (std::f64::consts::PI * (i as f64 + 0.5) / n as f64).cos();
                v.push((1.0 / (2.0 * c)) as f32);
            }
            n *= 2;
        }
        v
    })
}

#[inline]
fn factors(n: usize) -> &'static [f32] {
    let f = lee_factors();
    &f[n / 2 - 1..n - 1]
}

/// DCT-II 1-D sin normalizar (algoritmo recursivo de Byeong Gi Lee, O(n log n)).
fn dct_1d(x: &mut [f32], tmp: &mut [f32]) {
    let n = x.len();
    if n == 1 {
        return;
    }
    let h = n / 2;
    let f = factors(n);
    for i in 0..h {
        let a = x[i];
        let b = x[n - 1 - i];
        tmp[i] = a + b;
        tmp[i + h] = (a - b) * f[i];
    }
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let (x0, x1) = x.split_at_mut(h);
        dct_1d(t0, x0);
        dct_1d(&mut t1[..h], x1);
    }
    for i in 0..h - 1 {
        x[2 * i] = tmp[i];
        x[2 * i + 1] = tmp[i + h] + tmp[i + h + 1];
    }
    x[n - 2] = tmp[h - 1];
    x[n - 1] = tmp[n - 1];
}

/// DCT-III 1-D: x_n = X_0 + Σ_{k≥1} X_k cos(π(2n+1)k/2N).
fn idct_1d(x: &mut [f32], tmp: &mut [f32]) {
    let n = x.len();
    if n == 1 {
        return;
    }
    let h = n / 2;
    tmp[0] = x[0];
    tmp[h] = x[1];
    for i in 1..h {
        tmp[i] = x[2 * i];
        tmp[i + h] = x[2 * i - 1] + x[2 * i + 1];
    }
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let (x0, x1) = x.split_at_mut(h);
        idct_1d(t0, x0);
        idct_1d(&mut t1[..h], x1);
    }
    let f = factors(n);
    for i in 0..h {
        let a = tmp[i];
        let b = tmp[i + h] * f[i];
        x[i] = a + b;
        x[n - 1 - i] = a - b;
    }
}

#[inline]
fn norm(n: usize) -> (f32, f32) {
    ((1.0 / n as f32).sqrt(), (2.0 / n as f32).sqrt())
}

/// Transformada directa ortonormal en el sitio: `blk` es N×N en orden fila.
pub fn forward(blk: &mut [f32], n: usize) {
    let (s0, s1) = norm(n);
    let mut tmp = [0f32; 64];
    let mut col = [0f32; 64];
    for y in 0..n {
        let row = &mut blk[y * n..y * n + n];
        dct_1d(row, &mut tmp[..n]);
        row[0] *= s0;
        for v in &mut row[1..] {
            *v *= s1;
        }
    }
    for x in 0..n {
        for y in 0..n {
            col[y] = blk[y * n + x];
        }
        dct_1d(&mut col[..n], &mut tmp[..n]);
        blk[x] = col[0] * s0;
        for y in 1..n {
            blk[y * n + x] = col[y] * s1;
        }
    }
}

/// Transformada inversa ortonormal en el sitio. Las filas de coeficientes
/// nulas (muy frecuentes tras cuantizar) se saltan.
pub fn inverse(blk: &mut [f32], n: usize) {
    let (s0, s1) = norm(n);
    let (a0, a1) = (s0, s1);
    let mut tmp = [0f32; 64];
    let mut col = [0f32; 64];
    // filas (frecuencia vertical fija): DCT-III sobre u
    for v in 0..n {
        let row = &mut blk[v * n..v * n + n];
        if row.iter().all(|&c| c == 0.0) {
            continue;
        }
        row[0] *= a0;
        for c in &mut row[1..] {
            *c *= a1;
        }
        idct_1d(row, &mut tmp[..n]);
    }
    // columnas: DCT-III sobre v
    for x in 0..n {
        col[0] = blk[x] * a0;
        for y in 1..n {
            col[y] = blk[y * n + x] * a1;
        }
        idct_1d(&mut col[..n], &mut tmp[..n]);
        for y in 0..n {
            blk[y * n + x] = col[y];
        }
    }
}

/// SATD con Hadamard 4×4 u 8×8 sobre un residuo N×N (para preselección de modos).
pub fn satd(res: &[f32], n: usize) -> f32 {
    let t = if n == 4 { 4 } else { 8 };
    let mut total = 0f32;
    let mut buf = [0f32; 64];
    for by in (0..n).step_by(t) {
        for bx in (0..n).step_by(t) {
            for y in 0..t {
                for x in 0..t {
                    buf[y * t + x] = res[(by + y) * n + bx + x];
                }
            }
            hadamard(&mut buf[..t * t], t);
            total += buf[..t * t].iter().map(|v| v.abs()).sum::<f32>() / t as f32;
        }
    }
    total
}

fn hadamard(b: &mut [f32], t: usize) {
    for y in 0..t {
        let r = &mut b[y * t..y * t + t];
        wht1d(r);
    }
    let mut col = [0f32; 8];
    for x in 0..t {
        for y in 0..t {
            col[y] = b[y * t + x];
        }
        wht1d(&mut col[..t]);
        for y in 0..t {
            b[y * t + x] = col[y];
        }
    }
}

fn wht1d(v: &mut [f32]) {
    let n = v.len();
    let mut h = 1;
    while h < n {
        for i in (0..n).step_by(h * 2) {
            for j in i..i + h {
                let a = v[j];
                let b = v[j + h];
                v[j] = a + b;
                v[j + h] = a - b;
            }
        }
        h *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for &n in &SIZES {
            let orig: Vec<f32> = (0..n * n)
                .map(|i| ((i * 37 + 11) % 255) as f32 - 128.0)
                .collect();
            let mut b = orig.clone();
            forward(&mut b, n);
            let e_in: f32 = orig.iter().map(|v| v * v).sum();
            let e_out: f32 = b.iter().map(|v| v * v).sum();
            assert!((e_in - e_out).abs() / e_in < 1e-4, "Parseval n={n}");
            inverse(&mut b, n);
            for (a, c) in orig.iter().zip(&b) {
                assert!((a - c).abs() < 1e-2, "n={n}");
            }
        }
    }
}
