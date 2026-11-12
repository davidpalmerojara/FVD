//! Codificación entrópica de coeficientes y cuantización con optimización
//! tasa-distorsión (RDOQ).
//!
//! Sintaxis por bloque transformado:
//! 1. `cbf`: ¿hay algún coeficiente distinto de cero?
//! 2. `eob`: posición del último coeficiente no nulo en el orden de barrido.
//! 3. Barrido diagonal inverso: significancia, >1, >2, resto Exp-Golomb y
//!    signo. Los contextos dependen del tamaño, del plano, de la clase de
//!    frecuencia y de la magnitud de los vecinos de mayor frecuencia ya
//!    codificados (plantilla de 5 posiciones).

use crate::dct::size_index;
use crate::rc::{Coder, Model, UintCtx, Visit};
use std::sync::OnceLock;

const NF: usize = 5; // clases de frecuencia
const NM: usize = 6; // clases de magnitud de vecinos
const NPC: usize = 2; // luma / croma
const NSZ: usize = 5;

/// Orden de barrido diagonal de un bloque `w × h` (lados de 4 a 64).
pub fn scan(w: usize, h: usize) -> &'static [(u8, u8)] {
    static S: OnceLock<Vec<Vec<(u8, u8)>>> = OnceLock::new();
    let all = S.get_or_init(|| {
        let mut out = Vec::new();
        for &h in &crate::dct::SIZES {
            for &w in &crate::dct::SIZES {
                let mut v = Vec::with_capacity(w * h);
                for d in 0..w + h - 1 {
                    for y in 0..h {
                        if d >= y && d - y < w {
                            v.push(((d - y) as u8, y as u8));
                        }
                    }
                }
                out.push(v);
            }
        }
        out
    });
    &all[size_index(h) * 5 + size_index(w)]
}

#[inline]
fn freq_class(x: usize, y: usize) -> usize {
    match x + y {
        0 => 0,
        1..=2 => 1,
        3..=5 => 2,
        6..=10 => 3,
        _ => 4,
    }
}

/// Clase de magnitud de la plantilla de vecinos de mayor frecuencia ya
/// codificados: (x+1,y), (x,y+1), (x+1,y+1), (x+2,y), (x,y+2).
#[inline(always)]
fn mag_class(levels: &[i32], w: usize, h: usize, x: usize, y: usize) -> usize {
    const LUT: [u8; 16] = [0, 1, 2, 3, 3, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5];
    let i = y * w + x;
    let c = |v: i32| v.unsigned_abs().min(3) as usize;
    let (xr, xr2, yd, yd2) = (x + 1 < w, x + 2 < w, y + 1 < h, y + 2 < h);
    let mut s = 0;
    if xr {
        s += c(levels[i + 1]);
        if yd {
            s += c(levels[i + w + 1]);
        }
    }
    if xr2 {
        s += c(levels[i + 2]);
    }
    if yd {
        s += c(levels[i + w]);
    }
    if yd2 {
        s += c(levels[i + 2 * w]);
    }
    LUT[s] as usize
}

#[derive(Clone)]
pub struct CoefCtx {
    cbf: Vec<Model>,
    eob: Vec<UintCtx>,
    sig: Vec<Model>,
    gt1: Vec<Model>,
    gt2: Vec<Model>,
    rem: Vec<UintCtx>,
    dc_sign: [Model; NPC],
}

impl Default for CoefCtx {
    fn default() -> Self {
        let nctx = NPC * NSZ * NF * NM;
        CoefCtx {
            cbf: vec![Model::new(); NPC * NSZ * 3],
            eob: vec![UintCtx::default(); NPC * NSZ],
            sig: vec![Model::new(); nctx],
            gt1: vec![Model::new(); nctx],
            gt2: vec![Model::new(); nctx],
            rem: vec![UintCtx::default(); NPC * NF],
            dc_sign: [Model::new(); NPC],
        }
    }
}

#[inline]
fn cidx(pc: usize, sz: usize, fc: usize, mc: usize) -> usize {
    ((pc * NSZ + sz) * NF + fc) * NM + mc
}

impl Visit for CoefCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.cbf.visit(f);
        self.eob.visit(f);
        self.sig.visit(f);
        self.gt1.visit(f);
        self.gt2.visit(f);
        self.rem.visit(f);
        self.dc_sign.visit(f);
    }
}

impl CoefCtx {
    /// Codifica/decodifica los niveles de un bloque N×N (orden fila).
    /// `cbf_ctx` (0..3) resume si los vecinos tenían coeficientes.
    pub fn code_block<C: Coder>(
        &mut self,
        c: &mut C,
        pc: usize,
        w: usize,
        h: usize,
        cbf_ctx: usize,
        levels: &mut [i32],
    ) -> bool {
        let n = w;
        let nn = w * h;
        let sz = size_index(w.max(h));
        let sc = scan(w, h);
        let mut eob_enc = 0;
        for (i, &(x, y)) in sc.iter().enumerate() {
            if levels[y as usize * n + x as usize] != 0 {
                eob_enc = i + 1;
            }
        }
        let nz = c.bit(&mut self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx], eob_enc > 0);
        if !nz {
            levels[..nn].fill(0);
            return false;
        }
        let eob = self.eob[pc * NSZ + sz].code(c, eob_enc.saturating_sub(1) as u32) as usize + 1;
        let enc_vals: Vec<i32> = levels[..nn].to_vec();
        levels[..nn].fill(0);
        for i in (0..eob.min(nn)).rev() {
            let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
            let pos = y * n + x;
            let v = enc_vals[pos];
            let a = v.unsigned_abs();
            let idx = cidx(pc, sz, freq_class(x, y), mag_class(levels, w, h, x, y));
            if i != eob - 1 && !c.bit(&mut self.sig[idx], a != 0) {
                continue;
            }
            let mut m = 1u32;
            if c.bit(&mut self.gt1[idx], a > 1) {
                m = 2;
                if c.bit(&mut self.gt2[idx], a > 2) {
                    m = 3 + self.rem[pc * NF + freq_class(x, y)].code(c, a.saturating_sub(3));
                }
            }
            let neg = if pos == 0 {
                c.bit(&mut self.dc_sign[pc], v < 0)
            } else {
                c.bypass(v < 0)
            };
            levels[pos] = if neg { -(m as i32) } else { m as i32 };
        }
        true
    }

    /// Coste aproximado de un nivel (magnitud) en la posición dada.
    fn level_cost(
        &self,
        pc: usize,
        sz: usize,
        x: usize,
        y: usize,
        mc: usize,
        a: u32,
        is_last: bool,
    ) -> f32 {
        let idx = cidx(pc, sz, freq_class(x, y), mc);
        let mut bits = 0.0;
        if !is_last {
            bits += self.sig[idx].cost(a != 0);
            if a == 0 {
                return bits;
            }
        }
        bits += 1.0; // signo
        bits += self.gt1[idx].cost(a > 1);
        if a > 1 {
            bits += self.gt2[idx].cost(a > 2);
            if a > 2 {
                bits += self.rem[pc * NF + freq_class(x, y)].cost(a - 3);
            }
        }
        bits
    }

    pub fn block_cost(&self, pc: usize, w: usize, h: usize, cbf_ctx: usize, levels: &[i32]) -> f32 {
        let n = w;
        let sz = size_index(w.max(h));
        let sc = scan(w, h);
        let eob = sc
            .iter()
            .rposition(|&(x, y)| levels[y as usize * n + x as usize] != 0)
            .map_or(0, |i| i + 1);
        let cbf = &self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx];
        if eob == 0 {
            return cbf.cost(false);
        }
        let mut bits = cbf.cost(true) + self.eob[pc * NSZ + sz].cost(eob as u32 - 1);
        for i in (0..eob).rev() {
            let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
            let a = levels[y * n + x].unsigned_abs();
            let mc = mag_class(levels, w, h, x, y);
            bits += self.level_cost(pc, sz, x, y, mc, a, i == eob - 1);
        }
        bits
    }

    /// Cuantización RDO. `coefs` son los coeficientes transformados, `qs` el
    /// paso de cuantización por coeficiente. Devuelve (distorsión, bits).
    pub fn quantize_rdo(
        &self,
        pc: usize,
        w: usize,
        h: usize,
        cbf_ctx: usize,
        coefs: &[f32],
        qs: &[f32],
        lambda: f32,
        levels: &mut [i32],
    ) -> (f32, f32) {
        let n = w;
        let nn = w * h;
        let sz = size_index(w.max(h));
        let sc = scan(w, h);

        let mut last = None;
        for (i, &(x, y)) in sc.iter().enumerate() {
            let p = y as usize * n + x as usize;
            let l = (coefs[p].abs() / qs[p] + 0.5) as i32;
            levels[p] = l;
            if l != 0 {
                last = Some(i);
            }
        }
        let zero_dist: f32 = coefs[..nn].iter().map(|c| c * c).sum();
        let zero_bits = self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx].cost(false);
        let Some(last) = last else {
            levels[..nn].fill(0);
            return (zero_dist, zero_bits);
        };
        for &(x, y) in &sc[last + 1..] {
            levels[y as usize * n + x as usize] = 0;
        }
        let mut seen_nz = false;
        let mut dist = 0f32;
        for i in (0..=last).rev() {
            let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
            let p = y * n + x;
            let ca = coefs[p].abs();
            let q = qs[p];
            let l0 = levels[p] as u32;
            let mc = mag_class(levels, w, h, x, y);
            let mut best_l = 0u32;
            let mut best_cost = f32::INFINITY;
            let mut best_d = 0.0;
            let lo = l0.saturating_sub(1);
            // Los mismos costes que `level_cost`, sumados en el mismo orden,
            // pero con el contexto y los modelos consultados una sola vez.
            let fc = freq_class(x, y);
            let idx = cidx(pc, sz, fc, mc);
            let (sig, gt1, gt2) = (&self.sig[idx], &self.gt1[idx], &self.gt2[idx]);
            let rem = &self.rem[pc * NF + fc];
            let lcost = |a: u32, is_last: bool| -> f32 {
                let mut bits = 0.0;
                if !is_last {
                    bits += sig.cost(a != 0);
                    if a == 0 {
                        return bits;
                    }
                }
                bits += 1.0;
                bits += gt1.cost(a > 1);
                if a > 1 {
                    bits += gt2.cost(a > 2);
                    if a > 2 {
                        bits += rem.cost(a - 3);
                    }
                }
                bits
            };
            for (k, l) in [l0, lo, 0].into_iter().enumerate() {
                // Candidatos repetidos (l0 ≤ 1): mismo coste, gana el primero.
                if (k == 1 && l == l0) || (k == 2 && (l == lo || l == l0)) {
                    continue;
                }
                let d = (ca - l as f32 * q).powi(2);
                let r = if !seen_nz {
                    if l == 0 { 0.0 } else { lcost(l, true) }
                } else {
                    lcost(l, false)
                };
                let cost = d + lambda * r;
                if cost < best_cost {
                    best_cost = cost;
                    best_l = l;
                    best_d = d;
                }
            }
            if best_l != 0 {
                seen_nz = true;
            }
            dist += best_d;
            levels[p] = if coefs[p] < 0.0 {
                -(best_l as i32)
            } else {
                best_l as i32
            };
        }
        for &(x, y) in &sc[last + 1..] {
            let p = y as usize * n + x as usize;
            dist += coefs[p] * coefs[p];
        }
        if !seen_nz {
            return (zero_dist, zero_bits);
        }
        let mut bits = self.block_cost(pc, w, h, cbf_ctx, levels);
        // Optimización del final del barrido (EOB): prueba a truncar el bloque
        // en cada uno de los últimos coeficientes no nulos.
        let nz_pos: Vec<usize> = (0..=last)
            .filter(|&i| levels[sc[i].1 as usize * n + sc[i].0 as usize] != 0)
            .collect();
        if nz_pos.len() > 1 {
            let mut trial = levels[..nn].to_vec();
            let mut d_trial = dist;
            let mut best = (dist + lambda * bits, nz_pos.len(), bits);
            for k in (1..nz_pos.len()).rev().take(4) {
                // Solo compensa recortar coeficientes ±1 del final del barrido.
                let pk = sc[nz_pos[k]].1 as usize * n + sc[nz_pos[k]].0 as usize;
                if trial[pk].abs() > 1 {
                    break;
                }
                // Quita el coeficiente no nulo k (y con él todo lo posterior).
                let i = nz_pos[k];
                let p = sc[i].1 as usize * n + sc[i].0 as usize;
                let c = coefs[p].abs();
                let l = trial[p].unsigned_abs() as f32;
                d_trial += c * c - (c - l * qs[p]).powi(2);
                trial[p] = 0;
                let b = self.block_cost(pc, w, h, cbf_ctx, &trial);
                let j = d_trial + lambda * b;
                if j < best.0 {
                    best = (j, k, b);
                }
            }
            if best.1 < nz_pos.len() {
                let mut d = dist;
                for &i in &nz_pos[best.1..] {
                    let p = sc[i].1 as usize * n + sc[i].0 as usize;
                    let c = coefs[p].abs();
                    let l = levels[p].unsigned_abs() as f32;
                    d += c * c - (c - l * qs[p]).powi(2);
                    levels[p] = 0;
                }
                dist = d;
                // Es el mismo bloque que el de la prueba ganadora.
                bits = best.2;
            }
        }
        if zero_dist + lambda * zero_bits <= dist + lambda * bits {
            levels[..nn].fill(0);
            return (zero_dist, zero_bits);
        }
        (dist, bits)
    }
}
