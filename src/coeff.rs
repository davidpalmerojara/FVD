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
//!
//! En la v2, la última posición se codifica como (x, y), el resto tiene
//! contexto de la suma de la plantilla y, en los bloques DCT, los signos van
//! al final del bloque con ocultación de un signo por grupo de 16 posiciones
//! (sign data hiding, como en HEVC).

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
    /// v2, solo DCT: ocultación de signos (sign data hiding) por grupos de
    /// `SDH_GROUP` posiciones del barrido.
    sdh: bool,
    /// v2: última posición como (x, y) y resto con contexto de los vecinos.
    v2: bool,
    /// v2: prefijos de la última posición, por plano, lado y bin.
    last: Vec<Model>,
    /// v2: resto por plano, clase de frecuencia y clase de la suma de vecinos.
    rem_n: Vec<UintCtx>,
    /// Búfer de trabajo (no es estado del flujo): no nulos del bloque.
    nzl: Vec<(u16, u16)>,
}

/// Bins de prefijo de una coordenada de la última posición (lado 64).
const NLB: usize = 11;
/// Clases de la suma de magnitudes de la plantilla para el resto.
const NRC: usize = 7;

/// Grupo de una coordenada de la última posición: 0, 1, 2, 3, 4–5, 6–7,
/// 8–11, 12–15, 16–23, 24–31, 32–47, 48–63.
#[inline]
fn last_group(v: u32) -> u32 {
    if v < 4 {
        v
    } else {
        let k = 31 - v.leading_zeros();
        2 * k + ((v >> (k - 1)) & 1)
    }
}

/// Primer valor del grupo `g`.
#[inline]
fn group_min(g: u32) -> u32 {
    if g < 4 {
        g
    } else {
        let k = g / 2;
        (1 << k) | ((g & 1) << (k - 1))
    }
}

/// Índice en el barrido de cada posición (orden fila) de un bloque `w × h`.
fn scan_index(w: usize, h: usize) -> &'static [u16] {
    static S: OnceLock<Vec<Vec<u16>>> = OnceLock::new();
    let all = S.get_or_init(|| {
        let mut out = Vec::new();
        for &h in &crate::dct::SIZES {
            for &w in &crate::dct::SIZES {
                let mut v = vec![0u16; w * h];
                for (i, &(x, y)) in scan(w, h).iter().enumerate() {
                    v[y as usize * w + x as usize] = i as u16;
                }
                out.push(v);
            }
        }
        out
    });
    &all[size_index(h) * 5 + size_index(w)]
}

/// Suma sin tope de las magnitudes de la plantilla de vecinos.
#[inline(always)]
fn tmpl_sum(levels: &[i32], w: usize, h: usize, x: usize, y: usize) -> u32 {
    let i = y * w + x;
    let a = |v: i32| v.unsigned_abs();
    let (xr, xr2, yd, yd2) = (x + 1 < w, x + 2 < w, y + 1 < h, y + 2 < h);
    let mut s = 0;
    if xr {
        s += a(levels[i + 1]);
        if yd {
            s += a(levels[i + w + 1]);
        }
    }
    if xr2 {
        s += a(levels[i + 2]);
    }
    if yd {
        s += a(levels[i + w]);
    }
    if yd2 {
        s += a(levels[i + 2 * w]);
    }
    s
}

/// Clase del resto según la suma sin tope de la plantilla.
#[inline]
fn rem_class(s: u32) -> usize {
    match s {
        0..=2 => 0,
        3..=5 => 1,
        6..=9 => 2,
        10..=15 => 3,
        16..=25 => 4,
        26..=40 => 5,
        _ => 6,
    }
}

/// Posiciones del barrido por grupo de ocultación de signo.
const SDH_GROUP: usize = 16;
/// Distancia mínima en el barrido entre el primer y el último no nulo de un
/// grupo para que se oculte el signo del primero.
const SDH_SPAN: usize = 4;

/// Primer y último índice no nulo del grupo `lo..hi` y suma de magnitudes.
#[inline]
fn sdh_group(
    levels: &[i32],
    sc: &[(u8, u8)],
    n: usize,
    lo: usize,
    hi: usize,
) -> (usize, usize, u32) {
    let (mut first, mut last, mut sum) = (usize::MAX, 0, 0u32);
    for (i, &(x, y)) in sc[lo..hi].iter().enumerate() {
        let a = levels[y as usize * n + x as usize].unsigned_abs();
        if a != 0 {
            if first == usize::MAX {
                first = lo + i;
            }
            last = lo + i;
            sum += a;
        }
    }
    (first, last, sum)
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
            sdh: false,
            v2: false,
            last: vec![Model::new(); NPC * NSZ * NLB],
            rem_n: vec![UintCtx::default(); NPC * NF * NRC],
            nzl: Vec::new(),
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
    /// Modelos de la v2 (van al final del orden de visita de `Ctx`).
    pub fn visit_v2(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.last.visit(f);
        self.rem_n.visit(f);
    }

    /// Activa la sintaxis de la v2 (`sdh`: con ocultación de signos). Con
    /// `inherit` (la tabla entrenada no cubre los modelos nuevos), cada resto
    /// nuevo empieza con el estado del resto de la v1 de su plano y clase de
    /// frecuencia.
    pub fn init_v2(&mut self, sdh: bool, inherit: bool) {
        self.v2 = true;
        self.sdh = sdh;
        if inherit {
            for (k, r) in self.rem_n.iter_mut().enumerate() {
                *r = self.rem[k / NRC].clone();
            }
        }
    }

    #[inline]
    fn rem_idx(
        pc: usize,
        fc: usize,
        levels: &[i32],
        w: usize,
        h: usize,
        x: usize,
        y: usize,
    ) -> usize {
        (pc * NF + fc) * NRC + rem_class(tmpl_sum(levels, w, h, x, y))
    }

    /// Última posición (v2): x y luego y, cada una con el grupo en unario
    /// truncado (con contexto) y la posición dentro del grupo en bypass.
    fn code_last<C: Coder>(
        &mut self,
        c: &mut C,
        pc: usize,
        w: usize,
        h: usize,
        eob: usize,
    ) -> usize {
        let (ex, ey) = scan(w, h)[eob.max(1) - 1];
        let mut xy = [0u32; 2];
        for (d, (v, len)) in [(ex as u32, w), (ey as u32, h)].into_iter().enumerate() {
            let g_enc = last_group(v);
            let ng = last_group(len as u32 - 1);
            let base = (pc * NSZ + size_index(len)) * NLB;
            let mut g = 0;
            while g < ng && c.bit(&mut self.last[base + g as usize], g_enc > g) {
                g += 1;
            }
            xy[d] = group_min(g);
            if g > 3 {
                xy[d] += c.bypass_bits(v.wrapping_sub(group_min(g)), g / 2 - 1);
            }
        }
        scan_index(w, h)[xy[1] as usize * w + xy[0] as usize] as usize + 1
    }

    fn last_cost(&self, pc: usize, w: usize, h: usize, eob: usize) -> f32 {
        let (ex, ey) = scan(w, h)[eob - 1];
        let mut bits = 0.0;
        for (v, len) in [(ex as u32, w), (ey as u32, h)] {
            let g_enc = last_group(v);
            let ng = last_group(len as u32 - 1);
            let base = (pc * NSZ + size_index(len)) * NLB;
            for g in 0..ng {
                bits += self.last[base + g as usize].cost(g_enc > g);
                if g_enc <= g {
                    break;
                }
            }
            if g_enc > 3 {
                bits += (g_enc / 2 - 1) as f32;
            }
        }
        bits
    }

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
        let eob = if self.v2 {
            self.code_last(c, pc, w, h, eob_enc)
        } else {
            self.eob[pc * NSZ + sz].code(c, eob_enc.saturating_sub(1) as u32) as usize + 1
        };
        let enc_vals: Vec<i32> = levels[..nn].to_vec();
        levels[..nn].fill(0);
        // v2: posiciones no nulas en orden de decodificación (para los signos).
        let mut nzl = std::mem::take(&mut self.nzl);
        nzl.clear();
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
                    let fc = freq_class(x, y);
                    let r = if self.v2 {
                        &mut self.rem_n[Self::rem_idx(pc, fc, levels, w, h, x, y)]
                    } else {
                        &mut self.rem[pc * NF + fc]
                    };
                    m = 3 + r.code(c, a.saturating_sub(3));
                }
            }
            if self.sdh {
                // Los signos van después de todos los niveles.
                levels[pos] = m as i32;
                nzl.push((i as u16, pos as u16));
                continue;
            }
            let neg = if pos == 0 {
                c.bit(&mut self.dc_sign[pc], v < 0)
            } else {
                c.bypass(v < 0)
            };
            levels[pos] = if neg { -(m as i32) } else { m as i32 };
        }
        // Signos por grupos de SDH_GROUP posiciones del barrido, del último al
        // primero; `nzl` ya está en ese orden (índices decrecientes).
        let mut k = 0;
        while k < nzl.len() {
            let g = nzl[k].0 as usize / SDH_GROUP;
            let mut e = k;
            let mut sum = 0u32;
            while e < nzl.len() && nzl[e].0 as usize / SDH_GROUP == g {
                sum += levels[nzl[e].1 as usize] as u32;
                e += 1;
            }
            let hide = (nzl[k].0 - nzl[e - 1].0) as usize >= SDH_SPAN;
            for (j, &(_, pos)) in nzl[k..e].iter().enumerate() {
                let pos = pos as usize;
                let v = enc_vals[pos];
                let neg = if hide && k + j == e - 1 {
                    sum & 1 == 1
                } else if pos == 0 {
                    c.bit(&mut self.dc_sign[pc], v < 0)
                } else {
                    c.bypass(v < 0)
                };
                if neg {
                    levels[pos] = -levels[pos];
                }
            }
            k = e;
        }
        self.nzl = nzl;
        true
    }

    /// Signos ocultos de un bloque (bits que no se escriben).
    fn hidden_signs(&self, w: usize, h: usize, levels: &[i32], eob: usize) -> f32 {
        if !self.sdh {
            return 0.0;
        }
        let sc = scan(w, h);
        let mut k = 0;
        for g in 0..eob.div_ceil(SDH_GROUP) {
            let (lo, hi) = (g * SDH_GROUP, ((g + 1) * SDH_GROUP).min(eob));
            let (first, last, _) = sdh_group(levels, sc, w, lo, hi);
            k += (first != usize::MAX && last - first >= SDH_SPAN) as u32;
        }
        k as f32
    }

    /// Coste aproximado de un nivel (magnitud) en la posición dada.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn level_cost(
        &self,
        pc: usize,
        sz: usize,
        levels: &[i32],
        w: usize,
        h: usize,
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
                bits += self
                    .rem_model(pc, freq_class(x, y), levels, w, h, x, y)
                    .cost(a - 3);
            }
        }
        bits
    }

    /// Modelo del resto de la posición (x, y).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn rem_model(
        &self,
        pc: usize,
        fc: usize,
        levels: &[i32],
        w: usize,
        h: usize,
        x: usize,
        y: usize,
    ) -> &UintCtx {
        if self.v2 {
            &self.rem_n[Self::rem_idx(pc, fc, levels, w, h, x, y)]
        } else {
            &self.rem[pc * NF + fc]
        }
    }

    /// Coste de la cabecera de un bloque no vacío: cbf y última posición.
    #[inline]
    fn block_head(&self, pc: usize, w: usize, h: usize, cbf_ctx: usize, eob: usize) -> f32 {
        let sz = size_index(w.max(h));
        let eob_bits = if self.v2 {
            self.last_cost(pc, w, h, eob)
        } else {
            self.eob[pc * NSZ + sz].cost(eob as u32 - 1)
        };
        self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx].cost(true) + eob_bits
    }

    /// Coste de la posición `i` del barrido (el de `block_cost` para ese
    /// término), con los niveles actuales.
    #[inline(always)]
    fn term(&self, pc: usize, w: usize, h: usize, levels: &[i32], i: usize, eob: usize) -> f32 {
        let sz = size_index(w.max(h));
        let (x, y) = scan(w, h)[i];
        let (x, y) = (x as usize, y as usize);
        let a = levels[y * w + x].unsigned_abs();
        let mc = mag_class(levels, w, h, x, y);
        self.level_cost(pc, sz, levels, w, h, x, y, mc, a, i == eob - 1)
    }

    /// Recalcula los términos en `terms` afectados por un cambio del nivel
    /// de la posición `i` del barrido: ella misma y los vecinos de la
    /// plantilla que la tienen en su contexto (todos con índice menor).
    fn refresh_terms(
        &self,
        pc: usize,
        w: usize,
        h: usize,
        levels: &[i32],
        terms: &mut [f32],
        i: usize,
        eob: usize,
    ) {
        let (x, y) = scan(w, h)[i];
        let (x, y) = (x as usize, y as usize);
        let si = scan_index(w, h);
        terms[i] = self.term(pc, w, h, levels, i, eob);
        let nb = [
            (x >= 1, x.wrapping_sub(1), y),
            (x >= 2, x.wrapping_sub(2), y),
            (y >= 1, x, y.wrapping_sub(1)),
            (y >= 2, x, y.wrapping_sub(2)),
            (x >= 1 && y >= 1, x.wrapping_sub(1), y.wrapping_sub(1)),
        ];
        for (ok, nx, ny) in nb {
            if ok {
                let j = si[ny * w + nx] as usize;
                if j < eob {
                    terms[j] = self.term(pc, w, h, levels, j, eob);
                }
            }
        }
    }

    /// Suma de `base` y los términos de `eob - 1` a 0 menos los signos
    /// ocultos: el mismo resultado, bit a bit, que `block_cost`.
    fn sum_terms(&self, base: f32, w: usize, h: usize, levels: &[i32], terms: &[f32], eob: usize) -> f32 {
        let sc = scan(w, h);
        let mut bits = base;
        let (mut hidden, mut g_cur, mut g_hi, mut g_lo) = (0u32, usize::MAX, 0usize, 0usize);
        for i in (0..eob).rev() {
            if self.sdh {
                let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
                if levels[y * w + x] != 0 {
                    let g = i / SDH_GROUP;
                    if g != g_cur {
                        if g_cur != usize::MAX && g_hi - g_lo >= SDH_SPAN {
                            hidden += 1;
                        }
                        (g_cur, g_hi) = (g, i);
                    }
                    g_lo = i;
                }
            }
            bits += terms[i];
        }
        if g_cur != usize::MAX && g_hi - g_lo >= SDH_SPAN {
            hidden += 1;
        }
        bits - hidden as f32
    }

    pub fn block_cost(&self, pc: usize, w: usize, h: usize, cbf_ctx: usize, levels: &[i32]) -> f32 {
        let n = w;
        let sz = size_index(w.max(h));
        let sc = scan(w, h);
        let eob = sc
            .iter()
            .rposition(|&(x, y)| levels[y as usize * n + x as usize] != 0)
            .map_or(0, |i| i + 1);
        if eob == 0 {
            return self.cbf[(pc * NSZ + sz) * 3 + cbf_ctx].cost(false);
        }
        let mut bits = self.block_head(pc, w, h, cbf_ctx, eob);
        // Signos ocultos contados en la misma pasada (igual que
        // `hidden_signs`): por grupo, distancia entre el primer y el último no nulo.
        let (mut hidden, mut g_cur, mut g_hi, mut g_lo) = (0u32, usize::MAX, 0usize, 0usize);
        for i in (0..eob).rev() {
            let (x, y) = (sc[i].0 as usize, sc[i].1 as usize);
            let a = levels[y * n + x].unsigned_abs();
            if self.sdh && a != 0 {
                let g = i / SDH_GROUP;
                if g != g_cur {
                    if g_cur != usize::MAX && g_hi - g_lo >= SDH_SPAN {
                        hidden += 1;
                    }
                    (g_cur, g_hi) = (g, i);
                }
                g_lo = i;
            }
            let mc = mag_class(levels, w, h, x, y);
            bits += self.level_cost(pc, sz, levels, w, h, x, y, mc, a, i == eob - 1);
        }
        if g_cur != usize::MAX && g_hi - g_lo >= SDH_SPAN {
            hidden += 1;
        }
        debug_assert_eq!(hidden as f32, self.hidden_signs(w, h, levels, eob));
        bits - hidden as f32
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
        let mut eob = 0;
        let mut dist = 0f32;
        // Término de coste de cada posición (el de `block_cost`), para no
        // recalcular el bloque entero al probar finales y ocultar signos.
        let mut terms = vec![0f32; last + 1];
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
            let mut best_r = 0.0;
            let lo = l0.saturating_sub(1);
            // Los mismos costes que `level_cost`, sumados en el mismo orden,
            // pero con el contexto y los modelos consultados una sola vez.
            let fc = freq_class(x, y);
            let idx = cidx(pc, sz, fc, mc);
            let (sig, gt1, gt2) = (&self.sig[idx], &self.gt1[idx], &self.gt2[idx]);
            // El modelo del resto (plantilla de vecinos) solo hace falta si algún
            // candidato pasa de 2.
            let rem = (l0 > 2).then(|| self.rem_model(pc, fc, levels, w, h, x, y));
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
                        bits += rem.unwrap().cost(a - 3);
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
                    best_r = r;
                }
            }
            if best_l != 0 && !seen_nz {
                seen_nz = true;
                eob = i + 1;
            }
            terms[i] = best_r;
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
        let head = self.block_head(pc, w, h, cbf_ctx, eob);
        let mut bits = self.sum_terms(head, w, h, levels, &terms, eob);
        debug_assert_eq!(bits, self.block_cost(pc, w, h, cbf_ctx, levels));
        // Optimización del final del barrido (EOB): prueba a truncar el bloque
        // en cada uno de los últimos coeficientes no nulos.
        let mut tail = [0usize; 5];
        let mut nt = 0;
        for i in (0..eob).rev() {
            if levels[sc[i].1 as usize * n + sc[i].0 as usize] != 0 {
                tail[nt] = i;
                nt += 1;
                if nt == tail.len() {
                    break;
                }
            }
        }
        if nt > 1 {
            let mut d_trial = dist;
            // (J, coeficientes quitados, bits)
            let mut best = (dist + lambda * bits, 0usize, bits);
            let mut saved = [(0usize, 0i32); 4];
            let mut nrem = 0;
            for j in 0..(nt - 1).min(4) {
                let i = tail[j];
                let p = sc[i].1 as usize * n + sc[i].0 as usize;
                // Solo compensa recortar coeficientes ±1 del final del barrido.
                if levels[p].abs() > 1 {
                    break;
                }
                // Quita el coeficiente no nulo (y con él todo lo posterior).
                let c = coefs[p].abs();
                let l = levels[p].unsigned_abs() as f32;
                d_trial += c * c - (c - l * qs[p]).powi(2);
                saved[nrem] = (i, levels[p]);
                nrem += 1;
                levels[p] = 0;
                let ne = tail[j + 1] + 1;
                self.refresh_terms(pc, w, h, levels, &mut terms, i, ne);
                // El nuevo último coeficiente pierde el coste de significancia.
                terms[ne - 1] = self.term(pc, w, h, levels, ne - 1, ne);
                let head = self.block_head(pc, w, h, cbf_ctx, ne);
                let b = self.sum_terms(head, w, h, levels, &terms, ne);
                let jv = d_trial + lambda * b;
                if jv < best.0 {
                    best = (jv, nrem, b);
                }
            }
            // Devuelve los coeficientes quitados de más y deja `terms` al día.
            let keep = best.1;
            if nrem > keep {
                for &(i, v) in &saved[keep..nrem] {
                    levels[sc[i].1 as usize * n + sc[i].0 as usize] = v;
                }
                let ne = if keep == 0 { eob } else { tail[keep] + 1 };
                for &(i, _) in &saved[keep..nrem] {
                    self.refresh_terms(pc, w, h, levels, &mut terms, i, ne);
                }
                // El último de la prueba más profunda deja de serlo.
                terms[tail[nrem]] = self.term(pc, w, h, levels, tail[nrem], ne);
            }
            if keep > 0 {
                let mut d = dist;
                // En orden creciente del barrido (como siempre).
                for &(i, _) in saved[..keep].iter().rev() {
                    let p = sc[i].1 as usize * n + sc[i].0 as usize;
                    let c = coefs[p].abs();
                    d += c * c - (c - qs[p]).powi(2);
                }
                dist = d;
                eob = tail[keep] + 1;
                // Es el mismo bloque que el de la prueba ganadora.
                bits = best.2;
            }
        }
        if self.sdh {
            let (d, b) = self.sdh_fix(pc, w, h, cbf_ctx, coefs, qs, lambda, levels, &mut terms, eob);
            dist += d;
            if let Some(b) = b {
                bits = b;
            }
        }
        if zero_dist + lambda * zero_bits <= dist + lambda * bits {
            levels[..nn].fill(0);
            return (zero_dist, zero_bits);
        }
        debug_assert!(self.parity_ok(w, h, levels), "paridad de un signo oculto");
        (dist, bits)
    }

    /// ¿Cumplen la paridad todos los signos ocultos del bloque? (comprobación
    /// en compilaciones de depuración)
    fn parity_ok(&self, w: usize, h: usize, levels: &[i32]) -> bool {
        let sc = scan(w, h);
        let eob = sc
            .iter()
            .rposition(|&(x, y)| levels[y as usize * w + x as usize] != 0)
            .map_or(0, |i| i + 1);
        !self.sdh
            || (0..eob.div_ceil(SDH_GROUP)).all(|g| {
                let (lo, hi) = (g * SDH_GROUP, ((g + 1) * SDH_GROUP).min(eob));
                let (first, last, sum) = sdh_group(levels, sc, w, lo, hi);
                first == usize::MAX || last - first < SDH_SPAN || {
                    let (x, y) = sc[first];
                    (levels[y as usize * w + x as usize] < 0) == (sum & 1 == 1)
                }
            })
    }

    /// Ocultación de signos en el codificador: en cada grupo con el signo del
    /// primer no nulo oculto, si la paridad de la suma de magnitudes no lo
    /// da, cambia en ±1 el nivel que menos encarece J = D + λ·R. Devuelve el
    /// aumento de distorsión y, si cambió algo, los bits del bloque.
    #[allow(clippy::too_many_arguments)]
    fn sdh_fix(
        &self,
        pc: usize,
        w: usize,
        h: usize,
        cbf_ctx: usize,
        coefs: &[f32],
        qs: &[f32],
        lambda: f32,
        levels: &mut [i32],
        terms: &mut [f32],
        eob: usize,
    ) -> (f32, Option<f32>) {
        let n = w;
        let sz = size_index(w.max(h));
        let sc = scan(w, h);
        let mut dd = 0f32;
        let mut changed = false;
        for g in 0..eob.div_ceil(SDH_GROUP) {
            let (lo, hi) = (g * SDH_GROUP, ((g + 1) * SDH_GROUP).min(eob));
            let (first, last, sum) = sdh_group(levels, sc, n, lo, hi);
            if first == usize::MAX || last - first < SDH_SPAN {
                continue;
            }
            let pf = sc[first].1 as usize * n + sc[first].0 as usize;
            if (levels[pf] < 0) == (sum & 1 == 1) {
                continue;
            }
            // (coste, posición, nuevo nivel con signo, aumento de distorsión)
            let mut best = (f32::INFINITY, 0usize, 0i32, 0f32, 0usize);
            for (i, &(x, y)) in sc.iter().enumerate().take(hi).skip(first) {
                let (x, y) = (x as usize, y as usize);
                let p = y * n + x;
                let l = levels[p].unsigned_abs();
                let ca = coefs[p].abs();
                let q = qs[p];
                let d0 = (ca - l as f32 * q).powi(2);
                let mc = mag_class(levels, w, h, x, y);
                let is_last = i == eob - 1;
                let r0 = terms[i];
                let mut cands = [u32::MAX; 2];
                cands[0] = l + 1;
                // Bajar a 0 cambiaría el primero, el último o el final del bloque.
                if l >= 2 || (l == 1 && i != first && i != last && !is_last) {
                    cands[1] = l - 1;
                }
                for nl in cands {
                    if nl == u32::MAX {
                        continue;
                    }
                    let d1 = (ca - nl as f32 * q).powi(2);
                    let r1 = self.level_cost(pc, sz, levels, w, h, x, y, mc, nl, is_last);
                    let j = d1 - d0 + lambda * (r1 - r0);
                    if j < best.0 {
                        let s = if l != 0 {
                            levels[p].signum()
                        } else if coefs[p] < 0.0 {
                            -1
                        } else {
                            1
                        };
                        best = (j, p, s * nl as i32, d1 - d0, i);
                    }
                }
            }
            levels[best.1] = best.2;
            self.refresh_terms(pc, w, h, levels, terms, best.4, eob);
            dd += best.3;
            changed = true;
        }
        if !changed {
            return (0.0, None);
        }
        let head = self.block_head(pc, w, h, cbf_ctx, eob);
        let b = self.sum_terms(head, w, h, levels, terms, eob);
        debug_assert_eq!(b, self.block_cost(pc, w, h, cbf_ctx, levels));
        (dd, Some(b))
    }
}
