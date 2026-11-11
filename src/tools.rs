//! Herramientas para contenido sintético (capturas de pantalla, texto,
//! interfaces, ilustraciones planas):
//!
//! - **Paleta**: el bloque se predice con hasta 8 colores y un mapa de
//!   índices. Los índices se codifican por rango según los vecinos
//!   (izquierda, arriba, arriba-izquierda), lo que hace casi gratis el
//!   texto y los bordes nítidos.
//! - **Copia intra de bloques (IBC)**: el bloque se predice copiando una zona
//!   ya decodificada del mismo plano (vector de bloque). Las letras y los
//!   iconos repetidos cuestan muy poco.
//!
//! Ambas son modos de predicción: el residuo, si lo hay, se codifica igual
//! que en el resto de bloques.

use crate::predict::Plane;
use crate::rc::{Coder, CostCounter, Model, SintCtx, TreeCtx, UintCtx, Visit};
use std::collections::HashMap;

pub const MAX_COLORS: usize = 8;
/// Marcas en el mapa de modos para los bloques de paleta / IBC.
pub const MARK_PALETTE: u8 = 200;
pub const MARK_IBC: u8 = 201;

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Colores en orden ascendente (2..=8).
    pub colors: Vec<u8>,
    /// Índice de color de cada píxel (N×N, orden fila).
    pub idx: Vec<u8>,
}

/// Contextos de la sintaxis de paleta (por clase de plano).
#[derive(Clone)]
pub struct PalCtx {
    count: [TreeCtx; 2],
    reuse: [Model; 2],
    first: [UintCtx; 2],
    delta: [UintCtx; 2],
    /// [plano][patrón de vecinos][posición del rango]
    rank: Vec<Model>,
}

impl Default for PalCtx {
    fn default() -> Self {
        PalCtx {
            count: [TreeCtx::new(3), TreeCtx::new(3)],
            reuse: Default::default(),
            first: Default::default(),
            delta: Default::default(),
            rank: vec![Model::new(); 2 * 6 * 4],
        }
    }
}

impl Visit for PalCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.count.visit(f);
        self.reuse.visit(f);
        self.first.visit(f);
        self.delta.visit(f);
        self.rank.visit(f);
    }
}

/// Orden de candidatos para el índice de (x, y) según sus vecinos ya
/// codificados, y clase de contexto del patrón de vecinos.
fn neighbor_order(idx: &[u8], n: usize, k: usize, x: usize, y: usize) -> ([u8; MAX_COLORS], usize) {
    let l = (x > 0).then(|| idx[y * n + x - 1]);
    let a = (y > 0).then(|| idx[(y - 1) * n + x]);
    let al = (x > 0 && y > 0).then(|| idx[(y - 1) * n + x - 1]);
    let mut score = [0i32; MAX_COLORS];
    if let Some(v) = l {
        score[v as usize] += 2;
    }
    if let Some(v) = a {
        score[v as usize] += 2;
    }
    if let Some(v) = al {
        score[v as usize] += 1;
    }
    let mut order: [u8; MAX_COLORS] = [0, 1, 2, 3, 4, 5, 6, 7];
    order[..k].sort_by(|&p, &q| score[q as usize].cmp(&score[p as usize]).then(p.cmp(&q)));
    let cls = match (l, a, al) {
        (None, None, _) => 0,
        (Some(_), None, _) | (None, Some(_), _) => 1,
        (Some(l), Some(a), Some(al)) if l == a && a == al => 2,
        (Some(l), Some(a), _) if l == a => 3,
        (Some(l), Some(_), Some(al)) if l == al => 4,
        _ => 5,
    };
    (order, cls)
}

impl PalCtx {
    /// Codifica/decodifica una paleta. `last` es la última paleta del plano
    /// (estado compartido por codificador y decodificador).
    pub fn code<C: Coder>(
        &mut self,
        c: &mut C,
        pc: usize,
        w: usize,
        h: usize,
        pal: &Palette,
        last: &mut Option<Vec<u8>>,
    ) -> Palette {
        let can_reuse = last.is_some();
        let reuse = can_reuse
            && c.bit(
                &mut self.reuse[pc],
                last.as_deref() == Some(&pal.colors[..]),
            );
        let colors: Vec<u8> = if reuse {
            last.clone().unwrap()
        } else {
            let k = (self.count[pc].code(c, (pal.colors.len().max(2) - 2) as u32) as usize + 2)
                .min(MAX_COLORS);
            let mut cols = Vec::with_capacity(k);
            let mut prev = 0u32;
            for i in 0..k {
                let v_enc = pal.colors.get(i).copied().unwrap_or(0) as u32;
                let v = if i == 0 {
                    self.first[pc].code(c, v_enc)
                } else {
                    prev + 1 + self.delta[pc].code(c, v_enc.saturating_sub(prev + 1))
                };
                let v = v.min(255);
                cols.push(v as u8);
                prev = v;
            }
            cols
        };
        let k = colors.len();
        let mut idx = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let (order, cls) = neighbor_order(&idx, w, k, x, y);
                let target = pal.idx.get(y * w + x).copied().unwrap_or(0);
                let r_enc = order[..k].iter().position(|&o| o == target).unwrap_or(0);
                let mut r = 0;
                while r + 1 < k {
                    let m = &mut self.rank[(pc * 6 + cls) * 4 + r.min(3)];
                    if c.bit(m, r_enc == r) {
                        break;
                    }
                    r += 1;
                }
                idx[y * w + x] = order[r];
            }
        }
        *last = Some(colors.clone());
        Palette { colors, idx }
    }

    pub fn cost(
        &mut self,
        pc: usize,
        w: usize,
        h: usize,
        pal: &Palette,
        last: &Option<Vec<u8>>,
    ) -> f32 {
        let mut cc = CostCounter::default();
        let mut l = last.clone();
        self.code(&mut cc, pc, w, h, pal, &mut l);
        cc.bits
    }
}

pub fn predict_palette(pal: &Palette, out: &mut [i32]) {
    for (o, &i) in out.iter_mut().zip(&pal.idx) {
        *o = pal.colors[i as usize] as i32;
    }
}

/// Paletas candidatas para un bloque (vacío si el bloque no parece sintético).
pub fn palette_candidates(src: &[f32], w: usize, h: usize) -> Vec<Palette> {
    let nn = w * h;
    let mut hist = [0u32; 256];
    for &v in &src[..nn] {
        hist[v.round().clamp(0.0, 255.0) as usize] += 1;
    }
    let distinct: Vec<usize> = (0..256).filter(|&v| hist[v] > 0).collect();
    if distinct.len() < 2 || distinct.len() > 48 {
        return Vec::new();
    }
    let mut ks = vec![];
    if distinct.len() <= MAX_COLORS {
        ks.push(distinct.len());
    }
    for k in [2, 4, MAX_COLORS] {
        if k < distinct.len() && !ks.contains(&k) {
            ks.push(k);
        }
    }
    ks.into_iter()
        .map(|k| kmeans_palette(src, nn, &hist, k))
        .collect()
}

/// k-medias 1-D sobre el histograma del bloque.
fn kmeans_palette(src: &[f32], nn: usize, hist: &[u32; 256], k: usize) -> Palette {
    // Inicialización: los k valores más frecuentes.
    let mut by_freq: Vec<usize> = (0..256).filter(|&v| hist[v] > 0).collect();
    by_freq.sort_by(|&a, &b| hist[b].cmp(&hist[a]).then(a.cmp(&b)));
    let mut cent: Vec<f32> = by_freq.iter().take(k).map(|&v| v as f32).collect();
    for _ in 0..6 {
        let mut sum = vec![0f64; cent.len()];
        let mut cnt = vec![0f64; cent.len()];
        for v in 0..256 {
            if hist[v] == 0 {
                continue;
            }
            let j = nearest(&cent, v as f32);
            sum[j] += v as f64 * hist[v] as f64;
            cnt[j] += hist[v] as f64;
        }
        for j in 0..cent.len() {
            if cnt[j] > 0.0 {
                cent[j] = (sum[j] / cnt[j]) as f32;
            }
        }
    }
    let mut colors: Vec<u8> = cent
        .iter()
        .map(|c| c.round().clamp(0.0, 255.0) as u8)
        .collect();
    colors.sort_unstable();
    colors.dedup();
    if colors.len() < 2 {
        let c = colors[0];
        colors = vec![c.saturating_sub(1), c.saturating_add(1).max(1)];
        colors.dedup();
        if colors.len() < 2 {
            colors = vec![0, 255];
        }
    }
    let cf: Vec<f32> = colors.iter().map(|&c| c as f32).collect();
    let idx = src[..nn].iter().map(|&v| nearest(&cf, v) as u8).collect();
    Palette { colors, idx }
}

fn nearest(c: &[f32], v: f32) -> usize {
    let mut best = 0;
    let mut bd = f32::INFINITY;
    for (j, &cj) in c.iter().enumerate() {
        let d = (cj - v).abs();
        if d < bd {
            bd = d;
            best = j;
        }
    }
    best
}

// ------------------------------------------------------------------- IBC

/// Contextos de los vectores de bloque.
#[derive(Clone, Default)]
pub struct BvCtx {
    same: [Model; 2],
    dx: [SintCtx; 2],
    dy: [SintCtx; 2],
}

impl Visit for BvCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.same.visit(f);
        self.dx.visit(f);
        self.dy.visit(f);
    }
}

impl BvCtx {
    /// Codifica un vector como diferencia respecto al anterior del plano.
    pub fn code<C: Coder>(
        &mut self,
        c: &mut C,
        pc: usize,
        bv: (i32, i32),
        last: &mut (i32, i32),
    ) -> (i32, i32) {
        let out = if c.bit(&mut self.same[pc], bv == *last) {
            *last
        } else {
            let dx = self.dx[pc].code(c, bv.0 - last.0);
            let dy = self.dy[pc].code(c, bv.1 - last.1);
            (last.0 + dx, last.1 + dy)
        };
        *last = out;
        out
    }

    pub fn cost(&mut self, pc: usize, bv: (i32, i32), last: (i32, i32)) -> f32 {
        let mut cc = CostCounter::default();
        let mut l = last;
        self.code(&mut cc, pc, bv, &mut l);
        cc.bits
    }
}

/// ¿Es válido el vector? La zona de referencia debe estar entera dentro del
/// plano y ya decodificada.
pub fn bv_valid(p: &Plane, x0: usize, y0: usize, w: usize, h: usize, bv: (i32, i32)) -> bool {
    let rx = x0 as i64 + bv.0 as i64;
    let ry = y0 as i64 + bv.1 as i64;
    if rx < 0 || ry < 0 || rx + w as i64 > p.w as i64 || ry + h as i64 > p.h as i64 || bv == (0, 0)
    {
        return false;
    }
    let (rx, ry) = (rx as usize, ry as usize);
    let gy0 = ry / 4;
    let gy1 = (ry + h - 1) / 4;
    let gx0 = rx / 4;
    let gx1 = (rx + w - 1) / 4;
    for gy in gy0..=gy1 {
        for gx in gx0..=gx1 {
            if !p.avail[gy * p.gw + gx] {
                return false;
            }
        }
    }
    true
}

pub fn predict_ibc(
    p: &Plane,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    bv: (i32, i32),
    out: &mut [i32],
) {
    let rx = (x0 as i64 + bv.0 as i64) as usize;
    let ry = (y0 as i64 + bv.1 as i64) as usize;
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = p.data[(ry + y) * p.w + rx + x] as i32;
        }
    }
}

/// Índice hash de bloques 8×8 del plano original (solo codificador).
pub struct HashIndex {
    map: HashMap<u64, Vec<(u32, u32)>>,
}

const HB: usize = 8;

fn block_hash(src: &[f32], w: usize, x: usize, y: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for yy in 0..HB {
        for xx in 0..HB {
            let v = src[(y + yy) * w + x + xx].round() as i64 as u64;
            h ^= v;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

impl HashIndex {
    pub fn build(src: &[f32], w: usize, h: usize) -> Self {
        let mut map: HashMap<u64, Vec<(u32, u32)>> = HashMap::new();
        for y in 0..=h.saturating_sub(HB) {
            for x in 0..=w.saturating_sub(HB) {
                // Bloques planos: demasiados candidatos y poco útiles.
                let v0 = src[y * w + x];
                let mut flat = true;
                'f: for yy in 0..HB {
                    for xx in 0..HB {
                        if (src[(y + yy) * w + x + xx] - v0).abs() > 0.5 {
                            flat = false;
                            break 'f;
                        }
                    }
                }
                if flat {
                    continue;
                }
                let e = map.entry(block_hash(src, w, x, y)).or_default();
                if e.len() < 16 {
                    e.push((x as u32, y as u32));
                }
            }
        }
        HashIndex { map }
    }

    /// Vectores candidatos para el bloque en (x0, y0).
    pub fn candidates(&self, src: &[f32], w: usize, x0: usize, y0: usize) -> Vec<(i32, i32)> {
        let key = block_hash(src, w, x0, y0);
        self.map
            .get(&key)
            .map(|v| {
                v.iter()
                    .map(|&(x, y)| (x as i32 - x0 as i32, y as i32 - y0 as i32))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Detección de contenido sintético: proporción de bloques 16×16 de luma
/// con pocos valores distintos (texto, interfaces, gráficos planos).
pub fn looks_synthetic(rgb: &[u8], w: usize, h: usize) -> bool {
    let (bw, bh) = (w / 16, h / 16);
    if bw == 0 || bh == 0 {
        return false;
    }
    let mut few = 0;
    let mut total = 0;
    for by in 0..bh {
        for bx in 0..bw {
            let mut seen = [false; 256];
            let mut d = 0;
            for y in by * 16..by * 16 + 16 {
                for x in bx * 16..bx * 16 + 16 {
                    let i = (y * w + x) * 3;
                    let l =
                        ((rgb[i] as u32 * 77 + rgb[i + 1] as u32 * 150 + rgb[i + 2] as u32 * 29)
                            >> 8) as usize;
                    if !seen[l] {
                        seen[l] = true;
                        d += 1;
                    }
                }
            }
            total += 1;
            if d <= 12 {
                few += 1;
            }
        }
    }
    few * 100 >= total * 45
}
