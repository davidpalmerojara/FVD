//! Filtro de restauración Wiener (post-proceso con parámetros transmitidos).
//!
//! Al estilo del ALF de VVC: cada bloque 4×4 se clasifica (actividad y
//! dirección del gradiente) en una de 12 clases. Para cada clase el
//! codificador calcula, por mínimos cuadrados, un filtro simétrico de 15 taps
//! (cruz de radio 3 + diagonales) que acerca la reconstrucción al original, y
//! decide por superbloque de 64×64 si se aplica. El decodificador solo aplica
//! los filtros, en aritmética entera.

use crate::predict::Plane;
use crate::rc::{Coder, Model, SintCtx, Visit};

const BLK: usize = 64;
const PREC: i32 = 128; // coeficientes en 1/128
const CMAX: i32 = 64;
/// Nº de coeficientes: horizontal ±1..3, vertical ±1..3, diagonal y antidiagonal.
const NC: usize = 8;
const OFFS: [(isize, isize); NC] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 1),
    (1, -1),
];
/// Clases: 4 niveles de actividad × 3 direcciones.
pub const NCLASS: usize = 12;

pub type Filter = [i32; NC];

/// Contextos de la sintaxis de restauración (por clase de plano).
#[derive(Clone)]
pub struct RestCtx {
    on: [Model; 2],
    class_on: [Model; 2],
    coef: [[SintCtx; NC]; 2],
    flag: [[Model; 3]; 2],
}

impl Default for RestCtx {
    fn default() -> Self {
        RestCtx {
            on: Default::default(),
            class_on: Default::default(),
            coef: Default::default(),
            flag: Default::default(),
        }
    }
}

/// Decisión de restauración de un plano.
#[derive(Clone, Default)]
pub struct Decision {
    pub filters: Vec<Option<Filter>>,
    pub flags: Vec<bool>,
}

impl Decision {
    fn active(&self) -> bool {
        self.filters.iter().any(|f| f.is_some()) && self.flags.iter().any(|&b| b)
    }
}

impl Visit for RestCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.on.visit(f);
        self.class_on.visit(f);
        self.coef.visit(f);
        self.flag.visit(f);
    }
}

impl RestCtx {
    pub fn code<C: Coder>(
        &mut self,
        c: &mut C,
        pc: usize,
        bw: usize,
        bh: usize,
        d: &Decision,
    ) -> Decision {
        let off = Decision {
            filters: vec![None; NCLASS],
            flags: vec![false; bw * bh],
        };
        if !c.bit(&mut self.on[pc], d.active()) {
            return off;
        }
        let mut filters = vec![None; NCLASS];
        for (k, slot) in filters.iter_mut().enumerate() {
            let enc = d.filters.get(k).copied().flatten();
            if c.bit(&mut self.class_on[pc], enc.is_some()) {
                let e = enc.unwrap_or_default();
                let mut f = [0i32; NC];
                for j in 0..NC {
                    f[j] = self.coef[pc][j].code(c, e[j]).clamp(-CMAX, CMAX);
                }
                *slot = Some(f);
            }
        }
        let mut flags = vec![false; bw * bh];
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let ctx = (bx > 0 && flags[i - 1]) as usize + (by > 0 && flags[i - bw]) as usize;
                flags[i] = c.bit(
                    &mut self.flag[pc][ctx],
                    d.flags.get(i).copied().unwrap_or(false),
                );
            }
        }
        Decision { filters, flags }
    }
}

const PAD: usize = 3;

/// Copia del plano con un borde replicado de 3 píxeles: permite leer los
/// vecinos del filtro sin recortar coordenadas (mismo resultado que recortar).
struct Padded {
    d: Vec<i16>,
    pw: usize,
    offs: [isize; NC],
}

impl Padded {
    fn new(p: &Plane) -> Self {
        let (w, h) = (p.w, p.h);
        let pw = w + 2 * PAD;
        let ph = h + 2 * PAD;
        let mut d = vec![0i16; pw * ph];
        for y in 0..ph {
            let sy = y.saturating_sub(PAD).min(h - 1);
            let row = &p.data[sy * w..sy * w + w];
            let out = &mut d[y * pw..y * pw + pw];
            let (izq, resto) = out.split_at_mut(PAD);
            let (centro, der) = resto.split_at_mut(w);
            izq.fill(row[0] as i16);
            for (o, &v) in centro.iter_mut().zip(row) {
                *o = v as i16;
            }
            der.fill(row[w - 1] as i16);
        }
        let offs = OFFS.map(|(dx, dy)| dx + dy * pw as isize);
        Padded { d, pw, offs }
    }

    #[inline(always)]
    fn idx(&self, x: usize, y: usize) -> usize {
        (y + PAD) * self.pw + x + PAD
    }

    /// Valor central y las 8 diferencias simétricas (vecino+ + vecino− − 2·centro).
    /// `i` debe venir de `idx(x, y)` con (x, y) dentro del plano: el borde
    /// de `PAD` píxeles garantiza que todos los vecinos están en el búfer.
    #[inline(always)]
    fn grads(&self, i: usize) -> (i32, [i32; NC]) {
        let d = &self.d;
        assert!(i >= 3 * self.pw + 3 && i + 3 * self.pw + 3 < d.len());
        let c = d[i] as i32;
        let mut g = [0i32; NC];
        for k in 0..NC {
            let o = self.offs[k];
            // SEGURIDAD: |o| ≤ 3·pw + 3 y el assert de arriba acota i.
            let (p, m) = unsafe {
                (
                    *d.get_unchecked((i as isize + o) as usize),
                    *d.get_unchecked((i as isize - o) as usize),
                )
            };
            g[k] = p as i32 + m as i32 - 2 * c;
        }
        (c, g)
    }

    /// Clases de los bloques 4×4 `gx0..gx0 + out.len()` de la fila de bloques
    /// `gy` (según actividad y dirección del gradiente). Hasta 16 bloques por
    /// llamada. Se acumulan las diferencias de las 4 filas píxel a píxel
    /// (vectorizable) y luego se suman de 4 en 4.
    fn classes_row(&self, gy: usize, gx0: usize, out: &mut [u8]) {
        let n = out.len() * 4;
        assert!(n <= BLK);
        let pw = self.pw;
        // |2c − a − b| ≤ 510; cuatro filas ≤ 2040: cabe en i16.
        let (mut ah, mut av) = ([0i16; BLK], [0i16; BLK]);
        let (ah, av) = (&mut ah[..n], &mut av[..n]);
        for y in 0..4 {
            let i = self.idx(gx0 * 4, gy * 4 + y);
            let mid = &self.d[i - 1..i + n + 1];
            let up = &self.d[i - pw..i - pw + n];
            let dn = &self.d[i + pw..i + pw + n];
            for x in 0..n {
                let c = 2 * mid[x + 1];
                ah[x] += (c - mid[x] - mid[x + 2]).abs();
                av[x] += (c - up[x] - dn[x]).abs();
            }
        }
        for (b, o) in out.iter_mut().enumerate() {
            let sum = |v: &[i16]| v[b * 4..b * 4 + 4].iter().map(|&t| t as i32).sum::<i32>();
            let (sh, sv) = (sum(ah), sum(av));
            let act = (sh + sv) / 16;
            let a = match act {
                0..=1 => 0,
                2..=4 => 1,
                5..=11 => 2,
                _ => 3,
            };
            let dir = if sh > 2 * sv {
                1
            } else if sv > 2 * sh {
                2
            } else {
                0
            };
            *o = (a * 3 + dir) as u8;
        }
    }

    fn classify_all(&self, w: usize, h: usize) -> Vec<u8> {
        let (gw, gh) = (w / 4, h / 4);
        let mut out = vec![0u8; gw * gh];
        for by in 0..gh {
            for (j, seg) in out[by * gw..(by + 1) * gw].chunks_mut(BLK / 4).enumerate() {
                self.classes_row(by, j * (BLK / 4), seg);
            }
        }
        out
    }
}

#[inline(always)]
fn filter_px(c: i32, g: &[i32; NC], f: &Filter) -> u8 {
    let mut s = 0i32;
    for k in 0..NC {
        s += f[k] * g[k];
    }
    (c + ((s + 64) >> 7)).clamp(0, 255) as u8
}

/// Aplica el filtro a los superbloques marcados.
pub fn apply(p: &mut Plane, d: &Decision) {
    if !d.active() {
        return;
    }
    let w = p.w;
    let bw = w / BLK;
    let pad = Padded::new(p);
    let nb = BLK / 4;
    for (i, &on) in d.flags.iter().enumerate() {
        if !on {
            continue;
        }
        let (bx, by) = (i % bw, i / bw);
        for gy in by * nb..(by + 1) * nb {
            let mut cls = [0u8; BLK / 4];
            pad.classes_row(gy, bx * nb, &mut cls);
            // Coeficientes de cada columna de píxeles (los bloques sin filtro
            // llevan ceros: el resultado es el píxel sin cambios).
            let mut coef = [[0i16; BLK]; NC];
            let mut any = false;
            for (b, &c) in cls.iter().enumerate() {
                if let Some(f) = d.filters[c as usize] {
                    any = true;
                    for k in 0..NC {
                        coef[k][b * 4..b * 4 + 4].fill(f[k] as i16);
                    }
                }
            }
            if !any {
                continue;
            }
            for y in gy * 4..gy * 4 + 4 {
                let i0 = pad.idx(bx * BLK, y);
                let c: &[i16; BLK] = pad.d[i0..i0 + BLK].try_into().unwrap();
                let mut s = [0i32; BLK];
                for k in 0..NC {
                    let o = pad.offs[k];
                    let pp: &[i16; BLK] =
                        pad.d[(i0 as isize + o) as usize..][..BLK].try_into().unwrap();
                    let mm: &[i16; BLK] =
                        pad.d[(i0 as isize - o) as usize..][..BLK].try_into().unwrap();
                    let ck = &coef[k];
                    for x in 0..BLK {
                        // Píxeles en 0..=255: |g| ≤ 510 cabe en i16 y |f| ≤ 64
                        // (multiplicación 16×16→32, barata sin SSE4.1).
                        let g = pp[x] + mm[x] - 2 * c[x];
                        s[x] += ck[x] as i32 * g as i32;
                    }
                }
                let out = &mut p.data[y * w + bx * BLK..y * w + (bx + 1) * BLK];
                for x in 0..BLK {
                    out[x] = (c[x] as i32 + ((s[x] + 64) >> 7)).clamp(0, 255) as u8;
                }
            }
        }
    }
}

// ------------------------------------------------------------ Codificador

fn solve(mut a: [[f64; NC]; NC], mut b: [f64; NC]) -> Option<[f64; NC]> {
    let tr: f64 = (0..NC).map(|i| a[i][i]).sum::<f64>() / NC as f64;
    for i in 0..NC {
        a[i][i] += 1e-4 * tr + 1e-6;
    }
    for col in 0..NC {
        let piv =
            (col..NC).max_by(|&x, &y| a[x][col].abs().partial_cmp(&a[y][col].abs()).unwrap())?;
        a.swap(col, piv);
        b.swap(col, piv);
        if a[col][col].abs() < 1e-12 {
            return None;
        }
        for r in 0..NC {
            if r != col {
                let f = a[r][col] / a[col][col];
                for k in col..NC {
                    a[r][k] -= f * a[col][k];
                }
                b[r] -= f * b[col];
            }
        }
    }
    Some(std::array::from_fn(|i| b[i] / a[i][i]))
}

/// Elige filtros por clase y superbloques activos (criterio D + λ·R).
pub fn decide(p: &Plane, src: &[f32], lambda: f32) -> Decision {
    let (w, h) = (p.w, p.h);
    let (bw, bh) = (w / BLK, h / BLK);
    let gw = w / 4;
    let pad = Padded::new(p);
    let cls = pad.classify_all(w, h);
    let mut mask = vec![true; bw * bh];
    let mut best = Decision {
        filters: vec![None; NCLASS],
        flags: vec![false; bw * bh],
    };
    let mut best_net = 0.0;
    let lam = lambda as f64;
    for _round in 0..2 {
        if !mask.iter().any(|&m| m) {
            break;
        }
        // Ecuaciones normales por clase.
        let mut a = vec![[[0f64; NC]; NC]; NCLASS];
        let mut b = vec![[0f64; NC]; NCLASS];
        for y in 0..h {
            for x in 0..w {
                if !mask[(y / BLK) * bw + x / BLK] {
                    continue;
                }
                let k = cls[(y / 4) * gw + x / 4] as usize;
                let (c, g) = pad.grads(pad.idx(x, y));
                let r = src[y * w + x] as f64 - c as f64;
                for i in 0..NC {
                    let gi = g[i] as f64;
                    b[k][i] += gi * r;
                    for j in i..NC {
                        a[k][i][j] += gi * g[j] as f64;
                    }
                }
            }
        }
        let mut filters: Vec<Option<Filter>> = vec![None; NCLASS];
        for k in 0..NCLASS {
            for i in 0..NC {
                for j in 0..i {
                    a[k][i][j] = a[k][j][i];
                }
            }
            if a[k][0][0] <= 0.0 {
                continue;
            }
            if let Some(sol) = solve(a[k], b[k]) {
                let f: Filter = sol.map(|v| ((v * PREC as f64).round() as i32).clamp(-CMAX, CMAX));
                if f.iter().any(|&v| v != 0) {
                    filters[k] = Some(f);
                }
            }
        }
        // Ganancia por clase y por superbloque con los filtros cuantizados.
        let mut cls_gain = [0f64; NCLASS];
        let mut blk_gain = vec![[0f64; NCLASS]; bw * bh];
        for y in 0..h {
            for x in 0..w {
                let k = cls[(y / 4) * gw + x / 4] as usize;
                let Some(f) = filters[k] else { continue };
                let (c, g) = pad.grads(pad.idx(x, y));
                let v = filter_px(c, &g, &f) as f64;
                let t = src[y * w + x] as f64;
                let gain = (c as f64 - t).powi(2) - (v - t).powi(2);
                blk_gain[(y / BLK) * bw + x / BLK][k] += gain;
                if mask[(y / BLK) * bw + x / BLK] {
                    cls_gain[k] += gain;
                }
            }
        }
        for k in 0..NCLASS {
            if filters[k].is_some() && cls_gain[k] < lam * 45.0 {
                filters[k] = None;
            }
        }
        let mut flags = vec![false; bw * bh];
        let mut gain = 0.0;
        for i in 0..bw * bh {
            let gsum: f64 = (0..NCLASS)
                .filter(|&k| filters[k].is_some())
                .map(|k| blk_gain[i][k])
                .sum();
            if gsum > lam * 1.5 {
                flags[i] = true;
                gain += gsum;
            }
        }
        let nf = filters.iter().filter(|f| f.is_some()).count() as f64;
        let net = gain - lam * (NCLASS as f64 + nf * 40.0 + (bw * bh) as f64 * 0.8);
        if net > best_net {
            best_net = net;
            best = Decision {
                filters: filters.clone(),
                flags: flags.clone(),
            };
        }
        mask = flags;
    }
    best
}

// ------------------------------------------------- Croma guiada por la luma (v2)
//
// En 4:2:0, la croma de salida se interpola con la luma como guía (filtro
// guiado de He et al.): en cada muestra de croma se ajusta un modelo local
// c ≈ a·Y + b con las medias de 3×3 de la croma y de la luma reducida, los
// parámetros se promedian en 3×3, se interpolan como la croma (bilineal
// centrada) y se evalúan con la luma completa. La salida mezcla esa
// predicción con la interpolación bilineal en k/4 (k lo elige el
// codificador por plano). Todo en enteros; con k = 0 coincide exactamente con
// la interpolación bilineal.

/// Bits fraccionarios de a y b.
const GA: u32 = 10;
/// ε = 30 (varianza de luma) en la escala de `var9` (1296 = 81 · 16).
const GEPS: i64 = 1296 * 30;
/// |a| ≤ 4.
const GAMAX: i64 = 4 << GA;

/// División redondeada (hacia +∞ en el empate) con divisor positivo.
#[inline(always)]
fn rdiv(n: i64, d: i64) -> i64 {
    (2 * n + d).div_euclid(2 * d)
}

/// ⌊2^20 / 36⌉ y ⌊2^20 / 9⌉: divisiones por 36 y 9 como producto y desplazamiento.
const INV36: i64 = 29127;
const INV9: i64 = 116508;

/// `(n·m + 2^19) >> 20`.
#[inline(always)]
fn mulsh(n: i32, m: i64) -> i32 {
    ((n as i64 * m + (1 << 19)) >> 20) as i32
}

/// Fila `y` (de ancho `w`) de una rejilla.
#[inline(always)]
fn row(v: &[i32], y: usize, w: usize) -> &[i32] {
    &v[y * w..y * w + w]
}

/// Suma horizontal de 3 con coordenadas recortadas, fila a fila (`w × h`).
fn hsum3(v: &[i32], w: usize) -> Vec<i32> {
    let mut out = vec![0i32; v.len()];
    for (r, o) in v.chunks_exact(w).zip(out.chunks_exact_mut(w)) {
        o[0] = r[0] + r[0] + r[1.min(w - 1)];
        if w > 1 {
            for (o, t) in o[1..].iter_mut().zip(r.windows(3)) {
                *o = t[0] + t[1] + t[2];
            }
            o[w - 1] = r[w - 2] + r[w - 1] + r[w - 1];
        }
    }
    out
}

/// Filas vecinas (arriba, abajo) recortadas a `0..h`.
#[inline(always)]
fn vnb(y: usize, h: usize) -> (usize, usize) {
    (y.saturating_sub(1), (y + 1).min(h - 1))
}

/// Parámetros del modelo local sobre la rejilla de croma (`cw × ch`): ā y b̄
/// (escala 2^GA) y la croma.
struct Guide {
    cw: usize,
    ch: usize,
    a: Vec<i32>,
    b: Vec<i32>,
    c: Vec<i32>,
}

fn guided_params(luma: &[u8], ys: usize, c: &[u8], cs: usize, w: usize, h: usize) -> Guide {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let n = cw * ch;
    // Luma reducida D (suma de 2×2) y croma.
    let mut d = vec![0i32; n];
    let mut cv = vec![0i32; n];
    for y in 0..ch {
        let r0 = &luma[2 * y * ys..2 * y * ys + w];
        let y1 = (2 * y + 1).min(h - 1);
        let r1 = &luma[y1 * ys..y1 * ys + w];
        let dr = &mut d[y * cw..y * cw + cw];
        for ((o, p), q) in dr
            .iter_mut()
            .zip(r0.chunks_exact(2))
            .zip(r1.chunks_exact(2))
        {
            *o = p[0] as i32 + p[1] as i32 + q[0] as i32 + q[1] as i32;
        }
        if w % 2 == 1 {
            dr[cw - 1] = 2 * (r0[w - 1] as i32 + r1[w - 1] as i32);
        }
        for (o, &v) in cv[y * cw..y * cw + cw]
            .iter_mut()
            .zip(&c[y * cs..y * cs + cw])
        {
            *o = v as i32;
        }
    }
    let dd: Vec<i32> = d.iter().map(|&v| v * v).collect();
    let dc: Vec<i32> = d.iter().zip(&cv).map(|(&a, &b)| a * b).collect();
    let (hd, hdd, hc, hdc) = (
        hsum3(&d, cw),
        hsum3(&dd, cw),
        hsum3(&cv, cw),
        hsum3(&dc, cw),
    );
    // Modelo local c ≈ a·Y + b en cada muestra (ventana de 3×3).
    let mut a = vec![0i32; n];
    let mut b = vec![0i32; n];
    for y in 0..ch {
        let (u, l) = vnb(y, ch);
        let (d0, d1, d2) = (row(&hd, u, cw), row(&hd, y, cw), row(&hd, l, cw));
        let (e0, e1, e2) = (row(&hdd, u, cw), row(&hdd, y, cw), row(&hdd, l, cw));
        let (c0, c1, c2) = (row(&hc, u, cw), row(&hc, y, cw), row(&hc, l, cw));
        let (f0, f1, f2) = (row(&hdc, u, cw), row(&hdc, y, cw), row(&hdc, l, cw));
        let ar = &mut a[y * cw..y * cw + cw];
        let br = &mut b[y * cw..y * cw + cw];
        for x in 0..cw {
            let sd = d0[x] + d1[x] + d2[x];
            let sdd = e0[x] + e1[x] + e2[x];
            let sc = c0[x] + c1[x] + c2[x];
            let sdc = f0[x] + f1[x] + f2[x];
            let var = 9 * sdd - sd * sd;
            let cov = 9 * sdc - sd * sc;
            let ai = rdiv((4 * cov as i64) << GA, var as i64 + GEPS).clamp(-GAMAX, GAMAX) as i32;
            ar[x] = ai;
            br[x] = mulsh(((4 * sc) << GA) - ai * sd, INV36);
        }
    }
    // Promedio de 3×3 de a y b.
    let (hd, hc) = (hsum3(&a, cw), hsum3(&b, cw));
    for y in 0..ch {
        let (u, l) = vnb(y, ch);
        let (d0, d1, d2) = (row(&hd, u, cw), row(&hd, y, cw), row(&hd, l, cw));
        let (c0, c1, c2) = (row(&hc, u, cw), row(&hc, y, cw), row(&hc, l, cw));
        let ar = &mut a[y * cw..y * cw + cw];
        let br = &mut b[y * cw..y * cw + cw];
        for x in 0..cw {
            ar[x] = mulsh(d0[x] + d1[x] + d2[x], INV9);
            br[x] = mulsh(c0[x] + c1[x] + c2[x], INV9);
        }
    }
    Guide {
        cw,
        ch,
        a,
        b,
        c: cv,
    }
}

/// Evalúa la mezcla `k`/4 fila a fila: para cada fila `y` de la salida
/// (`w × h`) llama a `f(y, o)` con `o = (4 − k)·(u << GA) + k·(Â·Y + B̂)`
/// (escala 2^(GA+6)), donde u es la interpolación bilineal de la croma.
/// Por linealidad, se interpolan (4 − k)·(c << GA) + k·b̄ y k·ā.
fn guided_eval(
    gd: &Guide,
    k: i32,
    luma: &[u8],
    ys: usize,
    w: usize,
    h: usize,
    mut f: impl FnMut(usize, &[i32]),
) {
    let (cw, ch) = (gd.cw, gd.ch);
    let fa: Vec<i32> = gd.a.iter().map(|&v| k * v).collect();
    let fb: Vec<i32> =
        gd.b.iter()
            .zip(&gd.c)
            .map(|(&b, &c)| (4 - k) * (c << GA) + k * b)
            .collect();
    let (mut va, mut vb) = (vec![0i32; cw], vec![0i32; cw]);
    let (mut ha, mut hb) = (vec![0i32; 2 * cw], vec![0i32; 2 * cw]);
    for y in 0..h {
        let i = y / 2;
        let o = if y % 2 == 0 {
            i.saturating_sub(1)
        } else {
            (i + 1).min(ch - 1)
        };
        for (src, dst) in [(&fa, &mut va), (&fb, &mut vb)] {
            let (ri, ro) = (&src[i * cw..i * cw + cw], &src[o * cw..o * cw + cw]);
            for ((v, &p), &q) in dst.iter_mut().zip(ri).zip(ro) {
                *v = 3 * p + q;
            }
        }
        hup(&va, &mut ha);
        hup(&vb, &mut hb);
        let yr = &luma[y * ys..y * ys + w];
        for ((av, &bv), &yv) in ha.iter_mut().zip(&hb).zip(yr) {
            *av = *av * yv as i32 + bv;
        }
        f(y, &ha[..w]);
    }
}

/// Interpolación horizontal 2× centrada (escala 4): la salida 2x es
/// 3·v[x] + v[x−1] y la 2x+1, 3·v[x] + v[x+1] (vecinos recortados).
fn hup(v: &[i32], out: &mut [i32]) {
    let n = v.len();
    out[0] = 4 * v[0];
    out[1] = 3 * v[0] + v[1.min(n - 1)];
    if n > 1 {
        for (o, t) in out[2..].chunks_exact_mut(2).zip(v.windows(3)) {
            o[0] = 3 * t[1] + t[0];
            o[1] = 3 * t[1] + t[2];
        }
        out[2 * n - 2] = 3 * v[n - 1] + v[n - 2];
        out[2 * n - 1] = 4 * v[n - 1];
    }
}

/// Croma de salida a resolución completa (`w × h`, en f32 con 4 bits
/// fraccionarios) con la mezcla `k`/4 de la predicción guiada.
pub fn guided_chroma(
    luma: &[u8],
    ys: usize,
    c: &[u8],
    cs: usize,
    w: usize,
    h: usize,
    k: u8,
) -> Vec<f32> {
    let gd = guided_params(luma, ys, c, cs, w, h);
    let mut out = Vec::with_capacity(w * h);
    guided_eval(&gd, k as i32, luma, ys, w, h, |_, o| {
        out.extend(
            o.iter()
                .map(|&o| ((o + (1 << (GA + 1))) >> (GA + 2)).clamp(0, 255 * 16) as f32 / 16.0),
        );
    });
    out
}

/// Codificador: mezcla k ∈ 0..=4 que minimiza el error cuadrático de la croma
/// completa `src` (`w × h`).
pub fn guided_choose(
    luma: &[u8],
    ys: usize,
    c: &[u8],
    cs: usize,
    w: usize,
    h: usize,
    src: &[f32],
) -> u8 {
    let gd = guided_params(luma, ys, c, cs, w, h);
    // u (k = 0) y g (k = 4), en escala 2^(GA+6).
    let mut u = Vec::with_capacity(w * h);
    guided_eval(&gd, 0, luma, ys, w, h, |_, o| u.extend_from_slice(o));
    let sc = (1u32 << (GA + 2)) as f64;
    let (mut num, mut den) = (0f64, 0f64);
    guided_eval(&gd, 4, luma, ys, w, h, |y, g| {
        for x in 0..w {
            let uu = u[y * w + x] as f64 / sc;
            let dlt = g[x] as f64 / sc - uu;
            num += (src[y * w + x] as f64 * 16.0 - uu) * dlt;
            den += dlt * dlt;
        }
    });
    if den <= 0.0 {
        return 0;
    }
    (4.0 * num / den).round().clamp(0.0, 4.0) as u8
}
