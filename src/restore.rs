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
            for (x, o) in out.iter_mut().enumerate() {
                *o = row[x.saturating_sub(PAD).min(w - 1)] as i16;
            }
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

    /// Clase del bloque 4×4 (bx, by) según actividad y dirección del gradiente.
    fn class(&self, bx: usize, by: usize) -> u8 {
        let (mut sh, mut sv) = (0i32, 0i32);
        let pw = self.pw;
        for y in by * 4..by * 4 + 4 {
            let mut i = self.idx(bx * 4, y);
            for _ in 0..4 {
                let c = 2 * self.d[i] as i32;
                sh += (c - self.d[i - 1] as i32 - self.d[i + 1] as i32).abs();
                sv += (c - self.d[i - pw] as i32 - self.d[i + pw] as i32).abs();
                i += 1;
            }
        }
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
        (a * 3 + dir) as u8
    }

    fn classify_all(&self, w: usize, h: usize) -> Vec<u8> {
        let (gw, gh) = (w / 4, h / 4);
        let mut out = vec![0u8; gw * gh];
        for by in 0..gh {
            for bx in 0..gw {
                out[by * gw + bx] = self.class(bx, by);
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
    for (i, &on) in d.flags.iter().enumerate() {
        if !on {
            continue;
        }
        let (bx, by) = (i % bw, i / bw);
        for gy in by * BLK / 4..(by + 1) * BLK / 4 {
            for gx in bx * BLK / 4..(bx + 1) * BLK / 4 {
                let Some(f) = d.filters[pad.class(gx, gy) as usize] else {
                    continue;
                };
                for y in gy * 4..gy * 4 + 4 {
                    let mut pi = pad.idx(gx * 4, y);
                    for x in gx * 4..gx * 4 + 4 {
                        let (c, g) = pad.grads(pi);
                        p.data[y * w + x] = filter_px(c, &g, &f);
                        pi += 1;
                    }
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
