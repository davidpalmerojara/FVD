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
use crate::rc::{Coder, Model, UintCtx};
use std::sync::OnceLock;

const NF: usize = 5; // clases de frecuencia
const NM: usize = 6; // clases de magnitud de vecinos
const NPC: usize = 2; // luma / croma
const NSZ: usize = 5;

pub fn scan(n: usize) -> &'static [(u8, u8)] {
    static S: OnceLock<[Vec<(u8, u8)>; 5]> = OnceLock::new();
    let all = S.get_or_init(|| {
        crate::dct::SIZES.map(|n| {
            let mut v = Vec::with_capacity(n * n);
            for d in 0..2 * n - 1 {
                for y in 0..n {
                    if d >= y && d - y < n {
                        v.push(((d - y) as u8, y as u8));
                    }
                }
            }
            v
        })
    });
    &all[size_index(n)]
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

#[inline]
fn mag_class(levels: &[i32], n: usize, x: usize, y: usize) -> usize {
    let mut s = 0;
    let get = |xx: usize, yy: usize| -> i32 {
        if xx < n && yy < n {
            levels[yy * n + xx].abs().min(3)
        } else {
            0
        }
    };
    s += get(x + 1, y);
    s += get(x, y + 1);
    s += get(x + 1, y + 1);
    s += get(x + 2, y);
    s += get(x, y + 2);
    match s {
        0 => 0,
        1 => 1,
        2 => 2,
        3 | 4 => 3,
        5 | 6 => 4,
        _ => 5,
    }
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

impl CoefCtx {
    /// Codifica/decodifica los niveles de un bloque N×N (orden fila).
    /// `cbf_ctx` (0..3) resume si los vecinos tenían coeficientes.
    pub fn code_block<C: Coder>(
        &mut self,
        c: &mut C,
        pc: usize,
        n: usize,
        cbf_ctx: usize,
        levels: &mut [i32],
    ) -> bool {
        let sz = size_index(n);
        let sc = scan(n);
        let mut eob_enc = 0;
        for (i, &(x, y)) in sc.iter().enumerate() {
            if levels[y as usize * n + x as usize] != 0 {
                eob_enc = i + 1;
            }
        }
        let nz = c.bit(&mut self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx], eob_enc > 0);
        if !nz {
            levels[..n * n].fill(0);
            return false;
        }
        let eob = self.eob[pc * NSZ + sz].code(c, eob_enc.saturating_sub(1) as u32) as usize + 1;
        let enc_vals: Vec<i32> = levels[..n * n].to_vec();
        levels[..n * n].fill(0);
        for i in (0..eob.min(n * n)).rev() {
            let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
            let pos = y * n + x;
            let v = enc_vals[pos];
            let a = v.unsigned_abs();
            let idx = cidx(pc, sz, freq_class(x, y), mag_class(levels, n, x, y));
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

    pub fn block_cost(&self, pc: usize, n: usize, cbf_ctx: usize, levels: &[i32]) -> f32 {
        let sz = size_index(n);
        let sc = scan(n);
        let mut eob = 0;
        for (i, &(x, y)) in sc.iter().enumerate() {
            if levels[y as usize * n + x as usize] != 0 {
                eob = i + 1;
            }
        }
        let cbf = &self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx];
        if eob == 0 {
            return cbf.cost(false);
        }
        let mut bits = cbf.cost(true) + self.eob[pc * NSZ + sz].cost(eob as u32 - 1);
        for i in (0..eob).rev() {
            let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
            let a = levels[y * n + x].unsigned_abs();
            let mc = mag_class(levels, n, x, y);
            bits += self.level_cost(pc, sz, x, y, mc, a, i == eob - 1);
        }
        bits
    }

    /// Cuantización RDO. `coefs` son los coeficientes transformados, `qs` el
    /// paso de cuantización por coeficiente. Devuelve (distorsión, bits).
    pub fn quantize_rdo(
        &self,
        pc: usize,
        n: usize,
        cbf_ctx: usize,
        coefs: &[f32],
        qs: &[f32],
        lambda: f32,
        levels: &mut [i32],
    ) -> (f32, f32) {
        let sz = size_index(n);
        let sc = scan(n);
        let nn = n * n;
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
            let mc = mag_class(levels, n, x, y);
            let mut best_l = 0u32;
            let mut best_cost = f32::INFINITY;
            let mut best_d = 0.0;
            let lo = l0.saturating_sub(1);
            for l in [l0, lo, 0] {
                if l > l0 {
                    continue;
                }
                let d = (ca - l as f32 * q).powi(2);
                let r = if !seen_nz {
                    if l == 0 {
                        0.0
                    } else {
                        self.level_cost(pc, sz, x, y, mc, l, true)
                    }
                } else {
                    self.level_cost(pc, sz, x, y, mc, l, false)
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
        let bits = self.block_cost(pc, n, cbf_ctx, levels);
        if zero_dist + lambda * zero_bits <= dist + lambda * bits {
            levels[..nn].fill(0);
            return (zero_dist, zero_bits);
        }
        (dist, bits)
    }
}
