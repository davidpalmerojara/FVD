//! DCT-II ortonormal separable para bloques cuadrados de 4 a 64.
//! Al ser ortonormal, el error cuadrático en el dominio transformado es igual
//! al del dominio de píxeles, lo que simplifica el RDO.

use std::sync::OnceLock;

pub const SIZES: [usize; 5] = [4, 8, 16, 32, 64];

#[inline]
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
/// Matriz DCT-II sin normalizar de 8 puntos: cos(π(2i+1)k/16).
fn cos8() -> &'static [f32; 64] {
    static M: OnceLock<[f32; 64]> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = [0f32; 64];
        for k in 0..8 {
            for i in 0..8 {
                m[k * 8 + i] =
                    (std::f64::consts::PI * (2 * i + 1) as f64 * k as f64 / 16.0).cos() as f32;
            }
        }
        m
    })
}

fn dct_1d(x: &mut [f32], tmp: &mut [f32]) {
    let n = x.len();
    if n == 8 {
        // Caso base directo: evita tres niveles de recursión (solo lo usa el
        // codificador; la inversa del decodificador no cambia).
        let m = cos8();
        let mut out = [0f32; 8];
        for k in 0..8 {
            let row = &m[k * 8..k * 8 + 8];
            out[k] = row[0] * x[0]
                + row[1] * x[1]
                + row[2] * x[2]
                + row[3] * x[3]
                + row[4] * x[4]
                + row[5] * x[5]
                + row[6] * x[6]
                + row[7] * x[7];
        }
        x.copy_from_slice(&out);
        return;
    }
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

/// Matriz DCT ortonormal N×N (fila = frecuencia) para N = 4 u 8.
fn small_matrix(n: usize) -> &'static [f32; 64] {
    static M: OnceLock<[[f32; 64]; 2]> = OnceLock::new();
    let all = M.get_or_init(|| {
        [4usize, 8].map(|n| {
            let mut m = [0f32; 64];
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
    &all[(n == 8) as usize]
}

/// DCT 2-D por producto de matrices (más rápida que la recursión en 4 y 8).
fn small_transform(blk: &mut [f32], n: usize, inverse: bool) {
    let m = small_matrix(n);
    // a(k, i): coeficiente de la base; directa usa M, inversa usa Mᵀ.
    let a = |k: usize, i: usize| if inverse { m[i * n + k] } else { m[k * n + i] };
    let mut tmp = [0f32; 64];
    for y in 0..n {
        for k in 0..n {
            let mut s = 0f32;
            for x in 0..n {
                s += a(k, x) * blk[y * n + x];
            }
            tmp[y * n + k] = s;
        }
    }
    for k in 0..n {
        for x in 0..n {
            let mut s = 0f32;
            for y in 0..n {
                s += a(k, y) * tmp[y * n + x];
            }
            blk[k * n + x] = s;
        }
    }
}

/// Transformada directa ortonormal en el sitio: `blk` es N×N en orden fila.
pub fn forward(blk: &mut [f32], n: usize) {
    if n <= 8 {
        return small_transform(blk, n, false);
    }
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
    if n <= 8 {
        return small_transform(blk, n, true);
    }
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

/// DCT 1-D ortonormal de un vector de longitud 4..64 (directa o inversa).
fn dct1_ortho(v: &mut [f32], inverse: bool) {
    let n = v.len();
    if n <= 8 {
        let m = small_matrix(n);
        let mut out = [0f32; 8];
        for k in 0..n {
            let mut s = 0f32;
            for i in 0..n {
                s += if inverse { m[i * n + k] } else { m[k * n + i] } * v[i];
            }
            out[k] = s;
        }
        v.copy_from_slice(&out[..n]);
        return;
    }
    let (s0, s1) = norm(n);
    let mut tmp = [0f32; 64];
    if inverse {
        v[0] *= s0;
        for c in &mut v[1..] {
            *c *= s1;
        }
        idct_1d(v, &mut tmp[..n]);
    } else {
        dct_1d(v, &mut tmp[..n]);
        v[0] *= s0;
        for c in &mut v[1..] {
            *c *= s1;
        }
    }
}

/// DCT 2-D ortonormal de un bloque `w × h` (orden fila). Los bloques
/// cuadrados usan exactamente el mismo camino que `forward`/`inverse`.
pub fn transform_rect(blk: &mut [f32], w: usize, h: usize, inverse: bool) {
    if w == h {
        if inverse {
            inverse_sq(blk, w)
        } else {
            forward(blk, w)
        }
        return;
    }
    let mut col = [0f32; 64];
    for y in 0..h {
        dct1_ortho(&mut blk[y * w..y * w + w], inverse);
    }
    for x in 0..w {
        for y in 0..h {
            col[y] = blk[y * w + x];
        }
        dct1_ortho(&mut col[..h], inverse);
        for y in 0..h {
            blk[y * w + x] = col[y];
        }
    }
}

fn inverse_sq(blk: &mut [f32], n: usize) {
    inverse(blk, n)
}

/// SATD de un bloque `w × h` (teselas Hadamard de 8×8, o 4×4 si un lado es 4).
pub fn satd_rect(res: &[f32], w: usize, h: usize) -> f32 {
    if w == h {
        return satd(res, w);
    }
    let t = if w.min(h) == 4 { 4 } else { 8 };
    let mut total = 0f32;
    let mut b16 = [0f32; 16];
    let mut b64 = [0f32; 64];
    for by in (0..h).step_by(t) {
        for bx in (0..w).step_by(t) {
            if t == 4 {
                for y in 0..4 {
                    b16[y * 4..y * 4 + 4]
                        .copy_from_slice(&res[(by + y) * w + bx..(by + y) * w + bx + 4]);
                }
                total += had4(&mut b16) / 4.0;
            } else {
                for y in 0..8 {
                    b64[y * 8..y * 8 + 8]
                        .copy_from_slice(&res[(by + y) * w + bx..(by + y) * w + bx + 8]);
                }
                total += had8(&mut b64) / 8.0;
            }
        }
    }
    total
}

/// SATD con Hadamard 4×4 u 8×8 sobre un residuo N×N (para preselección de modos).
pub fn satd(res: &[f32], n: usize) -> f32 {
    if n == 4 {
        let mut b = [0f32; 16];
        b.copy_from_slice(&res[..16]);
        return had4(&mut b) / 4.0;
    }
    let mut total = 0f32;
    let mut b = [0f32; 64];
    let mut by = 0;
    while by < n {
        let mut bx = 0;
        while bx < n {
            for y in 0..8 {
                b[y * 8..y * 8 + 8].copy_from_slice(&res[(by + y) * n + bx..(by + y) * n + bx + 8]);
            }
            total += had8(&mut b) / 8.0;
            bx += 8;
        }
        by += 8;
    }
    total
}

#[inline(always)]
fn wht4(a: [f32; 4]) -> [f32; 4] {
    let (s0, d0, s1, d1) = (a[0] + a[1], a[0] - a[1], a[2] + a[3], a[2] - a[3]);
    [s0 + s1, d0 + d1, s0 - s1, d0 - d1]
}

#[inline(always)]
fn wht8(a: [f32; 8]) -> [f32; 8] {
    let l = wht4([a[0] + a[1], a[2] + a[3], a[4] + a[5], a[6] + a[7]]);
    let r = wht4([a[0] - a[1], a[2] - a[3], a[4] - a[5], a[6] - a[7]]);
    [l[0], r[0], l[1], r[1], l[2], r[2], l[3], r[3]]
}

/// Suma de valores absolutos de la Hadamard 4×4.
fn had4(b: &mut [f32; 16]) -> f32 {
    for r in 0..4 {
        let v = wht4([b[r * 4], b[r * 4 + 1], b[r * 4 + 2], b[r * 4 + 3]]);
        b[r * 4..r * 4 + 4].copy_from_slice(&v);
    }
    let mut s = 0f32;
    for c in 0..4 {
        let v = wht4([b[c], b[4 + c], b[8 + c], b[12 + c]]);
        s += v[0].abs() + v[1].abs() + v[2].abs() + v[3].abs();
    }
    s
}

/// Suma de valores absolutos de la Hadamard 8×8 (el orden de las salidas
/// no importa para la suma).
fn had8(b: &mut [f32; 64]) -> f32 {
    for r in 0..8 {
        let row: [f32; 8] = b[r * 8..r * 8 + 8].try_into().unwrap();
        b[r * 8..r * 8 + 8].copy_from_slice(&wht8(row));
    }
    let mut s = 0f32;
    for c in 0..8 {
        let col = [
            b[c],
            b[8 + c],
            b[16 + c],
            b[24 + c],
            b[32 + c],
            b[40 + c],
            b[48 + c],
            b[56 + c],
        ];
        for v in wht8(col) {
            s += v.abs();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Referencia ingenua de la Hadamard para comprobar la versión rápida.
    fn satd_ref(res: &[f32], n: usize) -> f32 {
        let t = if n == 4 { 4 } else { 8 };
        let h = |k: usize, i: usize| {
            if (k & i).count_ones() % 2 == 0 {
                1.0
            } else {
                -1.0
            }
        };
        let mut total = 0.0;
        for by in (0..n).step_by(t) {
            for bx in (0..n).step_by(t) {
                for u in 0..t {
                    for v in 0..t {
                        let mut s = 0f32;
                        for y in 0..t {
                            for x in 0..t {
                                s += h(u, y) * h(v, x) * res[(by + y) * n + bx + x];
                            }
                        }
                        total += s.abs() / t as f32;
                    }
                }
            }
        }
        total
    }

    #[test]
    fn satd_matches_reference() {
        for &n in &SIZES {
            let r: Vec<f32> = (0..n * n)
                .map(|i| ((i * 73 + 5) % 41) as f32 - 20.0)
                .collect();
            let (a, b) = (satd(&r, n), satd_ref(&r, n));
            assert!((a - b).abs() / b.max(1.0) < 1e-4, "n={n}: {a} vs {b}");
        }
    }

    #[test]
    fn rect_roundtrip() {
        for (w, h) in [
            (8usize, 4usize),
            (4, 8),
            (16, 8),
            (8, 16),
            (32, 16),
            (16, 32),
            (64, 32),
            (32, 64),
        ] {
            let orig: Vec<f32> = (0..w * h)
                .map(|i| ((i * 37 + 11) % 255) as f32 - 128.0)
                .collect();
            let mut b = orig.clone();
            transform_rect(&mut b, w, h, false);
            let e_in: f32 = orig.iter().map(|v| v * v).sum();
            let e_out: f32 = b.iter().map(|v| v * v).sum();
            assert!((e_in - e_out).abs() / e_in < 1e-4, "Parseval {w}x{h}");
            transform_rect(&mut b, w, h, true);
            for (a, c) in orig.iter().zip(&b) {
                assert!((a - c).abs() < 1e-2, "{w}x{h}");
            }
        }
    }

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
