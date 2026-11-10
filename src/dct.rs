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

/// Matriz DCT N×N en orden fila = frecuencia, columna = muestra.
fn matrix(n: usize) -> &'static [f32] {
    static M: OnceLock<[Vec<f32>; 5]> = OnceLock::new();
    let all = M.get_or_init(|| {
        SIZES.map(|n| {
            let mut m = vec![0f32; n * n];
            for k in 0..n {
                let s = if k == 0 {
                    (1.0 / n as f64).sqrt()
                } else {
                    (2.0 / n as f64).sqrt()
                };
                for i in 0..n {
                    let a = std::f64::consts::PI * (2 * i + 1) as f64 * k as f64 / (2 * n) as f64;
                    m[k * n + i] = (s * a.cos()) as f32;
                }
            }
            m
        })
    });
    &all[size_index(n)]
}

/// Transformada directa en el sitio: `blk` es N×N en orden fila.
pub fn forward(blk: &mut [f32], n: usize) {
    let m = matrix(n);
    let mut tmp = [0f32; 64 * 64];
    // filas: tmp[y][k] = sum_x m[k][x] * blk[y][x]
    for y in 0..n {
        let row = &blk[y * n..y * n + n];
        for k in 0..n {
            let mk = &m[k * n..k * n + n];
            let mut s = 0f32;
            for x in 0..n {
                s += mk[x] * row[x];
            }
            tmp[y * n + k] = s;
        }
    }
    // columnas: blk[k][x] = sum_y m[k][y] * tmp[y][x]
    for k in 0..n {
        let mk = &m[k * n..k * n + n];
        let out = &mut blk[k * n..k * n + n];
        out.fill(0.0);
        for y in 0..n {
            let w = mk[y];
            let tr = &tmp[y * n..y * n + n];
            for x in 0..n {
                out[x] += w * tr[x];
            }
        }
    }
}

/// Transformada inversa en el sitio.
pub fn inverse(blk: &mut [f32], n: usize) {
    let m = matrix(n);
    let mut tmp = [0f32; 64 * 64];
    // columnas: tmp[y][u] = sum_v m[v][y] * blk[v][u]
    for v in 0..n {
        let row = &blk[v * n..v * n + n];
        for y in 0..n {
            let w = m[v * n + y];
            if w == 0.0 {
                continue;
            }
            let t = &mut tmp[y * n..y * n + n];
            for u in 0..n {
                t[u] += w * row[u];
            }
        }
    }
    // filas: blk[y][x] = sum_u m[u][x] * tmp[y][u]
    for y in 0..n {
        let t = &tmp[y * n..y * n + n];
        let out = &mut blk[y * n..y * n + n];
        for x in 0..n {
            let mut s = 0f32;
            for u in 0..n {
                s += m[u * n + x] * t[u];
            }
            out[x] = s;
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
