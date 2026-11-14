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
/// Devuelve la matriz transpuesta (`t[i * 8 + k] = cos(π(2i+1)k/16)`), de modo
/// que para un `i` fijo los 8 valores de `k` son contiguos.
fn cos8_t() -> &'static [f32; 64] {
    static M: OnceLock<[f32; 64]> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = [0f32; 64];
        for k in 0..8 {
            for i in 0..8 {
                m[i * 8 + k] =
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
        // Vectorizado sobre k; cada salida suma en el orden i = 0..7.
        let m = cos8_t();
        let mut out = [0f32; 8];
        for (o, &c) in out.iter_mut().zip(&m[..8]) {
            *o = c * x[0];
        }
        for i in 1..8 {
            let xi = x[i];
            for (o, &c) in out.iter_mut().zip(&m[i * 8..i * 8 + 8]) {
                *o += c * xi;
            }
        }
        x.copy_from_slice(&out);
        return;
    }
    if n == 1 {
        return;
    }
    let h = n / 2;
    let f = factors(n);
    {
        // Rebanadas de longitud h conocida: sin comprobaciones de límites.
        let (xa, xb) = x.split_at(h);
        let (t0, t1) = tmp.split_at_mut(h);
        let (t1, f) = (&mut t1[..h], &f[..h]);
        for i in 0..h {
            let (a, b) = (xa[i], xb[h - 1 - i]);
            t0[i] = a + b;
            t1[i] = (a - b) * f[i];
        }
    }
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let (x0, x1) = x.split_at_mut(h);
        dct_1d(t0, x0);
        dct_1d(&mut t1[..h], x1);
    }
    let (t0, t1) = tmp.split_at(h);
    let t1 = &t1[..h];
    for i in 0..h - 1 {
        x[2 * i] = t0[i];
        x[2 * i + 1] = t1[i] + t1[i + 1];
    }
    x[n - 2] = t0[h - 1];
    x[n - 1] = t1[h - 1];
}

/// DCT-III 1-D: x_n = X_0 + Σ_{k≥1} X_k cos(π(2n+1)k/2N).
fn idct_1d(x: &mut [f32], tmp: &mut [f32]) {
    let n = x.len();
    if n == 1 {
        return;
    }
    let h = n / 2;
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let t1 = &mut t1[..h];
        t0[0] = x[0];
        t1[0] = x[1];
        for i in 1..h {
            t0[i] = x[2 * i];
            t1[i] = x[2 * i - 1] + x[2 * i + 1];
        }
    }
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let (x0, x1) = x.split_at_mut(h);
        idct_1d(t0, x0);
        idct_1d(&mut t1[..h], x1);
    }
    let f = &factors(n)[..h];
    let (t0, t1) = tmp.split_at(h);
    let t1 = &t1[..h];
    let (xl, xr) = x.split_at_mut(h);
    for i in 0..h {
        let a = t0[i];
        let b = t1[i] * f[i];
        xl[i] = a + b;
        xr[h - 1 - i] = a - b;
    }
}

/// DCT-III por lotes de columnas: `x` son `n` filas de `W` valores y la
/// recursión de `idct_1d` opera sobre filas enteras (cada columna hace
/// exactamente las mismas operaciones, en el mismo orden, que `idct_1d`; el
/// bucle sobre `W` vectoriza). `tmp` tiene también `n` filas.
fn idct_cols<const W: usize>(x: &mut [[f32; W]], tmp: &mut [[f32; W]]) {
    let n = x.len();
    if n == 1 {
        return;
    }
    let h = n / 2;
    {
        let (t0, t1) = tmp.split_at_mut(h);
        t0[0] = x[0];
        t1[0] = x[1];
        for i in 1..h {
            let (a, b, c) = (&x[2 * i], &x[2 * i - 1], &x[2 * i + 1]);
            t0[i] = *a;
            for j in 0..W {
                t1[i][j] = b[j] + c[j];
            }
        }
    }
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let (x0, x1) = x.split_at_mut(h);
        idct_cols(t0, x0);
        idct_cols(t1, x1);
    }
    let f = &factors(n)[..h];
    let (t0, t1) = tmp.split_at(h);
    let (xl, xr) = x.split_at_mut(h);
    for i in 0..h {
        let (a, t, fi) = (&t0[i], &t1[i], f[i]);
        for j in 0..W {
            let b = t[j] * fi;
            xl[i][j] = a[j] + b;
            xr[h - 1 - i][j] = a[j] - b;
        }
    }
}

/// DCT-II por lotes de columnas (ver `idct_cols`); `n` ≥ 8.
fn dct_cols<const W: usize>(x: &mut [[f32; W]], tmp: &mut [[f32; W]]) {
    let n = x.len();
    if n == 8 {
        let m = cos8_t();
        let mut out = [[0f32; W]; 8];
        for k in 0..8 {
            let c = m[k];
            for j in 0..W {
                out[k][j] = c * x[0][j];
            }
        }
        for i in 1..8 {
            for k in 0..8 {
                let c = m[i * 8 + k];
                for j in 0..W {
                    out[k][j] += c * x[i][j];
                }
            }
        }
        x.copy_from_slice(&out);
        return;
    }
    let h = n / 2;
    let f = &factors(n)[..h];
    {
        let (xa, xb) = x.split_at(h);
        let (t0, t1) = tmp.split_at_mut(h);
        for i in 0..h {
            let (a, b, fi) = (&xa[i], &xb[h - 1 - i], f[i]);
            for j in 0..W {
                t0[i][j] = a[j] + b[j];
                t1[i][j] = (a[j] - b[j]) * fi;
            }
        }
    }
    {
        let (t0, t1) = tmp.split_at_mut(h);
        let (x0, x1) = x.split_at_mut(h);
        dct_cols(t0, x0);
        dct_cols(t1, x1);
    }
    let (t0, t1) = tmp.split_at(h);
    for i in 0..h - 1 {
        x[2 * i] = t0[i];
        for j in 0..W {
            x[2 * i + 1][j] = t1[i][j] + t1[i + 1][j];
        }
    }
    x[n - 2] = t0[h - 1];
    x[n - 1] = t1[h - 1];
}

/// Transformada 1-D ortonormal de todas las columnas de un bloque de `W`
/// columnas y `h` filas (orden fila), todas a la vez. Mismo resultado, bit a
/// bit, que aplicar `dct1_ortho` a cada columna.
fn cols_w<const W: usize>(blk: &mut [f32], h: usize, inverse: bool) {
    let x = blk[..h * W].as_chunks_mut::<W>().0;
    if h <= 8 {
        let (m, t) = small_matrices(h);
        let at = if inverse { m } else { t };
        let mut out = [[0f32; W]; 8];
        for i in 0..h {
            for k in 0..h {
                let c = at[i * h + k];
                for j in 0..W {
                    out[k][j] += c * x[i][j];
                }
            }
        }
        x.copy_from_slice(&out[..h]);
        return;
    }
    let (s0, s1) = norm(h);
    let mut tmp = [[0f32; W]; 64];
    let tmp = &mut tmp[..h];
    if inverse {
        for v in &mut x[0] {
            *v *= s0;
        }
        for row in &mut x[1..] {
            for v in row {
                *v *= s1;
            }
        }
        idct_cols(x, tmp);
    } else {
        dct_cols(x, tmp);
        for v in &mut x[0] {
            *v *= s0;
        }
        for row in &mut x[1..] {
            for v in row {
                *v *= s1;
            }
        }
    }
}

/// Pasada de columnas de un bloque `w × h` (ver `cols_w`).
fn cols_pass(blk: &mut [f32], w: usize, h: usize, inverse: bool) {
    match w {
        4 => cols_w::<4>(blk, h, inverse),
        8 => cols_w::<8>(blk, h, inverse),
        16 => cols_w::<16>(blk, h, inverse),
        32 => cols_w::<32>(blk, h, inverse),
        _ => cols_w::<64>(blk, h, inverse),
    }
}

#[inline]
fn norm(n: usize) -> (f32, f32) {
    ((1.0 / n as f32).sqrt(), (2.0 / n as f32).sqrt())
}

/// Matriz DCT ortonormal y su transpuesta (`t[i * n + k] = m[k * n + i]`).
fn small_matrices(n: usize) -> &'static ([f32; 64], [f32; 64]) {
    static M: OnceLock<[([f32; 64], [f32; 64]); 2]> = OnceLock::new();
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
            let mut t = [0f32; 64];
            for k in 0..n {
                for i in 0..n {
                    t[i * n + k] = m[k * n + i];
                }
            }
            (m, t)
        })
    });
    &all[(n == 8) as usize]
}

/// DCT 2-D por producto de matrices (más rápida que la recursión en 4 y 8).
/// Cada coeficiente suma sus productos en orden creciente; el bucle interior
/// recorre `N` coeficientes independientes a la vez (vectorizable).
fn small_transform(blk: &mut [f32], n: usize, inverse: bool) {
    let (m, t) = small_matrices(n);
    // a(k, i): coeficiente de la base; directa usa M, inversa usa Mᵀ.
    // `at[i * n + k]` = a(k, i).
    let at = if inverse { m } else { t };
    match n {
        4 => small_transform_n::<4>(blk, at),
        _ => small_transform_n::<8>(blk, at),
    }
}

#[inline(always)]
fn small_transform_n<const N: usize>(blk: &mut [f32], at: &[f32; 64]) {
    let mut tmp = [0f32; 64];
    // tmp[y][k] = Σ_x a(k, x) · blk[y][x]
    for y in 0..N {
        let mut s = [0f32; N];
        for x in 0..N {
            let b = blk[y * N + x];
            for (sk, &c) in s.iter_mut().zip(&at[x * N..x * N + N]) {
                *sk += c * b;
            }
        }
        tmp[y * N..y * N + N].copy_from_slice(&s);
    }
    // blk[k][x] = Σ_y a(k, y) · tmp[y][x]
    for k in 0..N {
        let mut s = [0f32; N];
        for y in 0..N {
            let c = at[y * N + k];
            for (sx, &v) in s.iter_mut().zip(&tmp[y * N..y * N + N]) {
                *sx += c * v;
            }
        }
        blk[k * N..k * N + N].copy_from_slice(&s);
    }
}

/// Pasada de filas de un bloque `w × h` (w ≥ 16): transformada 1-D
/// ortonormal de cada fila, de 8 filas a la vez (se transponen a un búfer de
/// `w × 8` y se usa la recursión por lotes). Mismo resultado, bit a bit, que
/// `dct1_ortho` fila a fila. En la inversa se saltan las filas nulas (su
/// resultado es cero).
fn rows_pass(blk: &mut [f32], w: usize, h: usize, inverse: bool) {
    let (s0, s1) = norm(w);
    let mut rows = [0usize; 64];
    let mut nr = 0;
    for y in 0..h {
        if !inverse || blk[y * w..y * w + w].iter().any(|&c| c != 0.0) {
            rows[nr] = y;
            nr += 1;
        }
    }
    let mut t = [[0f32; 8]; 64];
    let mut tmp = [[0f32; 8]; 64];
    for chunk in rows[..nr].chunks(8) {
        let (t, tmp) = (&mut t[..w], &mut tmp[..w]);
        for (r, &y) in chunk.iter().enumerate() {
            let row = &blk[y * w..y * w + w];
            for u in 0..w {
                t[u][r] = row[u];
            }
        }
        for r in chunk.len()..8 {
            for u in 0..w {
                t[u][r] = 0.0;
            }
        }
        if inverse {
            for v in &mut t[0] {
                *v *= s0;
            }
            for row in &mut t[1..] {
                for v in row {
                    *v *= s1;
                }
            }
            idct_cols(t, tmp);
        } else {
            dct_cols(t, tmp);
            for v in &mut t[0] {
                *v *= s0;
            }
            for row in &mut t[1..] {
                for v in row {
                    *v *= s1;
                }
            }
        }
        for (r, &y) in chunk.iter().enumerate() {
            let row = &mut blk[y * w..y * w + w];
            for u in 0..w {
                row[u] = t[u][r];
            }
        }
    }
}

/// Transformada directa ortonormal en el sitio: `blk` es N×N en orden fila.
pub fn forward(blk: &mut [f32], n: usize) {
    if n <= 8 {
        return small_transform(blk, n, false);
    }
    rows_pass(blk, n, n, false);
    cols_pass(blk, n, n, false);
}

/// Transformada inversa ortonormal en el sitio. Las filas de coeficientes
/// nulas (muy frecuentes tras cuantizar) se saltan.
pub fn inverse(blk: &mut [f32], n: usize) {
    if n <= 8 {
        return small_transform(blk, n, true);
    }
    // filas (frecuencia vertical fija): DCT-III sobre u
    rows_pass(blk, n, n, true);
    // columnas: DCT-III sobre v (todas a la vez)
    cols_pass(blk, n, n, true);
}

/// DCT 1-D ortonormal de un vector de longitud 4..64 (directa o inversa).
fn dct1_ortho(v: &mut [f32], inverse: bool) {
    let n = v.len();
    if n <= 8 {
        let (m, t) = small_matrices(n);
        let at = if inverse { m } else { t };
        let mut out = [0f32; 8];
        for i in 0..n {
            let vi = v[i];
            for (o, &c) in out[..n].iter_mut().zip(&at[i * n..i * n + n]) {
                *o += c * vi;
            }
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
    if w <= 8 {
        for y in 0..h {
            dct1_ortho(&mut blk[y * w..y * w + w], inverse);
        }
    } else {
        rows_pass(blk, w, h, inverse);
    }
    cols_pass(blk, w, h, inverse);
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
