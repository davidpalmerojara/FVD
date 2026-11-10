//! Codificador y decodificador FVD.
//!
//! Estructura del bitstream: cabecera fija + carga útil con codificación
//! aritmética. Se codifican los planos Y, Cb y Cr en ese orden, cada uno en
//! superbloques de 64×64 recorridos en orden raster. Cada superbloque es un
//! quadtree (64 → 32 → 16 → 8 → 4) cuyas hojas llevan un modo de predicción intra
//! y los coeficientes cuantizados del residuo.

use crate::coeff::CoefCtx;
use crate::color;
use crate::dct::{self, size_index};
use crate::predict::{self, MODE_CCLM, MODE_DC, MODE_PLANAR, MODE_VER, Plane};
use crate::rc::{Coder, CostCounter, Decoder, Encoder, Model, SintCtx, TreeCtx};

pub const MAGIC: &[u8; 4] = b"FVD1";
const SB: usize = 64;
/// Granularidad de la cuantización adaptativa (AQ).
const AQ: usize = 32;
const HEADER_LEN: usize = 4 + 4 + 4 + 1 + 2 + 1 + 1 + 1 + 1 + 1;

/// Parámetros del codificador.
#[derive(Clone, Debug)]
pub struct Params {
    /// Calidad 0..100 (100 = casi sin pérdida).
    pub quality: f32,
    /// Submuestreo de croma 4:2:0 (None = automático según calidad).
    pub subsample: Option<bool>,
    /// Esfuerzo 1..9: nº de modos que pasan a RDO completo.
    pub effort: u8,
    /// Intensidad de la cuantización adaptativa por superbloque.
    pub aq_strength: f32,
    /// Factor del paso de croma respecto a luma.
    pub chroma_q: f32,
    /// Pendiente de la matriz de cuantización (luma / croma).
    pub qm_luma: f32,
    pub qm_chroma: f32,
    /// λ = lambda_k · q².
    pub lambda_k: f32,
    /// Intensidad del filtro de desbloqueo (None = automático).
    pub deblock: Option<u8>,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            quality: 75.0,
            subsample: None,
            effort: 2,
            aq_strength: 1.0,
            chroma_q: 0.6,
            qm_luma: 3.0,
            qm_chroma: 3.0,
            lambda_k: 0.12,
            deblock: Some(0),
        }
    }
}

impl Params {
    /// Ajuste para maximizar PSNR (sin ponderación perceptual).
    pub fn tune_psnr(mut self) -> Self {
        self.aq_strength = 0.0;
        self.chroma_q = 1.0;
        self.qm_luma = 0.0;
        self.qm_chroma = 0.0;
        self
    }
}

pub fn quality_to_qstep(q: f32) -> f32 {
    0.6 * 2f32.powf((100.0 - q.clamp(0.0, 100.0)) / 12.5)
}

/// Cabecera: todo lo que el decodificador necesita para reconstruir.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub subsample: bool,
    pub qstep: u16,   // q · 64
    pub chroma_q: u8, // factor · 32
    pub qm_luma: u8,  // · 16
    pub qm_chroma: u8,
    pub aq: bool,
    pub deblock: u8,
}

impl Header {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(self.subsample as u8);
        out.extend_from_slice(&self.qstep.to_le_bytes());
        out.push(self.chroma_q);
        out.push(self.qm_luma);
        out.push(self.qm_chroma);
        out.push(self.aq as u8);
        out.push(self.deblock);
    }

    fn read(d: &[u8]) -> Result<Header, String> {
        if d.len() < HEADER_LEN || &d[..4] != MAGIC {
            return Err("no es un archivo FVD".into());
        }
        let u32le = |i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap());
        let h = Header {
            width: u32le(4),
            height: u32le(8),
            subsample: d[12] != 0,
            qstep: u16::from_le_bytes([d[13], d[14]]),
            chroma_q: d[15],
            qm_luma: d[16],
            qm_chroma: d[17],
            aq: d[18] != 0,
            deblock: d[19],
        };
        if h.width == 0 || h.height == 0 || h.width > 1 << 16 || h.height > 1 << 16 {
            return Err("dimensiones no válidas".into());
        }
        Ok(h)
    }

    fn qstep(&self) -> f32 {
        self.qstep as f32 / 64.0
    }
}

// ------------------------------------------------------------------ Contextos

struct Ctx {
    coef: CoefCtx,
    split: Vec<Model>,
    mpm_flag: Vec<Model>,
    mpm_idx: [[Model; 2]; 2],
    rem_mode: [TreeCtx; 2],
    cclm: [Model; 5],
    dq: SintCtx,
}

impl Ctx {
    fn new() -> Self {
        Ctx {
            coef: CoefCtx::default(),
            split: vec![Model::new(); 2 * 4 * 4],
            mpm_flag: vec![Model::new(); 2 * 5],
            mpm_idx: [[Model::new(); 2]; 2],
            rem_mode: [TreeCtx::new(5), TreeCtx::new(5)],
            cclm: [Model::new(); 5],
            dq: SintCtx::default(),
        }
    }
}

fn mpm_list(p: &Plane, x0: usize, y0: usize) -> [u8; 3] {
    let norm = |m: Option<(u8, u8, bool)>| match m {
        Some((m, _, _)) if m < MODE_CCLM => m,
        _ => MODE_DC,
    };
    let l = norm(p.neighbor(x0, y0, true));
    let a = norm(p.neighbor(x0, y0, false));
    if l == a {
        if l < 2 {
            [MODE_PLANAR, MODE_DC, MODE_VER]
        } else {
            [l, 2 + ((l + 29) % 32), 2 + ((l - 2 + 1) % 32)]
        }
    } else {
        let third = if l != MODE_PLANAR && a != MODE_PLANAR {
            MODE_PLANAR
        } else if l != MODE_DC && a != MODE_DC {
            MODE_DC
        } else {
            MODE_VER
        };
        [l, a, third]
    }
}

fn code_mode<C: Coder>(
    ctx: &mut Ctx,
    c: &mut C,
    pc: usize,
    n: usize,
    mpm: [u8; 3],
    mode: u8,
) -> u8 {
    let sz = size_index(n);
    if pc == 1 && c.bit(&mut ctx.cclm[sz], mode == MODE_CCLM) {
        return MODE_CCLM;
    }
    let mi = mpm.iter().position(|&m| m == mode).unwrap_or(0);
    let is_mpm = mpm.contains(&mode);
    if c.bit(&mut ctx.mpm_flag[pc * 5 + sz], is_mpm) {
        if !c.bit(&mut ctx.mpm_idx[pc][0], mi > 0) {
            return mpm[0];
        }
        let b = c.bit(&mut ctx.mpm_idx[pc][1], mi > 1);
        return mpm[if b { 2 } else { 1 }];
    }
    let mut sorted = mpm;
    sorted.sort_unstable();
    let r_enc = mode.saturating_sub(sorted.iter().filter(|&&m| m < mode).count() as u8);
    let mut m = ctx.rem_mode[pc].code(c, r_enc as u32) as u8;
    for &s in &sorted {
        if m >= s {
            m += 1;
        }
    }
    m
}

fn split_ctx(p: &Plane, x0: usize, y0: usize, n: usize, pc: usize) -> usize {
    let lvl = size_index(n) - 1;
    let l = p
        .neighbor(x0, y0, true)
        .map_or(0, |(_, s, _)| ((s as usize) < n) as usize);
    let a = p
        .neighbor(x0, y0, false)
        .map_or(0, |(_, s, _)| ((s as usize) < n) as usize);
    (pc * 4 + lvl) * 4 + l * 2 + a
}

fn cbf_ctx(p: &Plane, x0: usize, y0: usize) -> usize {
    p.neighbor(x0, y0, true).map_or(0, |v| v.2 as usize)
        + p.neighbor(x0, y0, false).map_or(0, |v| v.2 as usize)
}

// ------------------------------------------------------------- Cuantización

/// Pesos de la matriz de cuantización por tamaño de bloque.
struct QuantTables {
    w: [[Vec<f32>; 5]; 2],
}

impl QuantTables {
    fn new(qm_luma: f32, qm_chroma: f32) -> Self {
        let make = |s: f32| {
            dct::SIZES.map(|n| {
                let mut v = vec![0f32; n * n];
                for y in 0..n {
                    for x in 0..n {
                        let r = ((x * x + y * y) as f32).sqrt() / n as f32;
                        v[y * n + x] = 1.0 + s * r * r;
                    }
                }
                v
            })
        };
        QuantTables {
            w: [make(qm_luma), make(qm_chroma)],
        }
    }
}

fn sb_qstep(base: f32, dq: i32) -> f32 {
    base * 2f32.powf(dq as f32 / 6.0)
}

/// Mapa de dq (cuantización adaptativa) a granularidad AQ×AQ de luma.
struct QMap {
    dq: Vec<i32>,
    w: usize,
    h: usize,
    base: f32,
    cq: f32,
    sub: bool,
}

impl QMap {
    /// Paso de cuantización de un bloque: media redondeada del dq de las
    /// celdas de luma que cubre.
    fn q(&self, pc: usize, x0: usize, y0: usize, n: usize) -> f32 {
        let f = if pc == 1 && self.sub { 2 } else { 1 };
        let (cx0, cy0) = ((x0 * f / AQ).min(self.w - 1), (y0 * f / AQ).min(self.h - 1));
        let cx1 = ((x0 + n) * f).div_ceil(AQ).clamp(cx0 + 1, self.w);
        let cy1 = ((y0 + n) * f).div_ceil(AQ).clamp(cy0 + 1, self.h);
        let mut sum = 0;
        let mut cnt = 0;
        for cy in cy0..cy1 {
            for cx in cx0..cx1 {
                sum += self.dq[cy * self.w + cx];
                cnt += 1;
            }
        }
        let dq = (2 * sum + cnt).div_euclid(2 * cnt);
        sb_qstep(self.base, dq) * if pc == 1 { self.cq } else { 1.0 }
    }

    /// Codifica/decodifica los dq de las celdas AQ de un superbloque.
    fn code_sb<C: Coder>(&mut self, ctx: &mut Ctx, c: &mut C, sx: usize, sy: usize) {
        let k = SB / AQ;
        for cy in sy * k..(sy * k + k).min(self.h) {
            for cx in sx * k..(sx * k + k).min(self.w) {
                let i = cy * self.w + cx;
                self.dq[i] = ctx.dq.code(c, self.dq[i]).clamp(-64, 64);
            }
        }
    }
}

// ----------------------------------------------------------------- Árbol

enum Node {
    Split(Box<[Node; 4]>),
    Leaf { mode: u8, levels: Vec<i32> },
}

const CHILDREN: [(usize, usize); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];

/// Reconstruye un bloque: descuantiza, IDCT y suma la predicción.
fn reconstruct(levels: &[i32], pred: &[i32], q: f32, w: &[f32], n: usize, out: &mut [u8]) {
    let mut buf = [0f32; 64 * 64];
    let mut any = false;
    for i in 0..n * n {
        if levels[i] != 0 {
            buf[i] = levels[i] as f32 * q * w[i];
            any = true;
        }
    }
    if any {
        dct::inverse(&mut buf[..n * n], n);
    }
    for i in 0..n * n {
        out[i] = (pred[i] as f32 + buf[i]).round().clamp(0.0, 255.0) as u8;
    }
}

struct LumaRef {
    data: Vec<u8>,
    w: usize,
    h: usize,
}

impl LumaRef {
    fn new(luma: &Plane, sub: bool) -> Self {
        if !sub {
            return LumaRef {
                data: luma.data.clone(),
                w: luma.w,
                h: luma.h,
            };
        }
        let (w, h) = (luma.w / 2, luma.h / 2);
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let s = luma.px(2 * x, 2 * y) as u32
                    + luma.px(2 * x + 1, 2 * y) as u32
                    + luma.px(2 * x, 2 * y + 1) as u32
                    + luma.px(2 * x + 1, 2 * y + 1) as u32;
                data[y * w + x] = ((s + 2) / 4) as u8;
            }
        }
        LumaRef { data, w, h }
    }
}

fn predict_mode(
    p: &Plane,
    refs: &predict::Refs,
    lref: Option<&LumaRef>,
    mode: u8,
    x0: usize,
    y0: usize,
    n: usize,
    out: &mut [i32],
) {
    if mode == MODE_CCLM {
        let l = lref.expect("CCLM solo en croma");
        predict::predict_cclm(p, &l.data, l.w, l.h, x0, y0, n, out);
    } else {
        predict::predict(refs, mode, n, out);
    }
}

// ------------------------------------------------------------- Codificador

struct PlaneEnc<'a> {
    src: &'a [f32],
    plane: Plane,
    pc: usize,
}

struct EncState {
    ctx: Ctx,
    qt: QuantTables,
    lambda_k: f32,
    topk: usize,
}

impl EncState {
    fn eval_leaf(
        &mut self,
        pe: &mut PlaneEnc,
        lref: Option<&LumaRef>,
        x0: usize,
        y0: usize,
        n: usize,
        q: f32,
    ) -> (f32, u8, Vec<i32>, Vec<u8>, bool) {
        let lambda = self.lambda_k * q * q;
        let slam = lambda.sqrt();
        let pc = pe.pc;
        let nn = n * n;
        let mut src = [0f32; 64 * 64];
        for y in 0..n {
            for x in 0..n {
                src[y * n + x] = pe.src[(y0 + y) * pe.plane.w + x0 + x];
            }
        }
        let refs = predict::build_refs(&pe.plane, x0, y0, n);
        let mpm = mpm_list(&pe.plane, x0, y0);
        let cctx = cbf_ctx(&pe.plane, x0, y0);
        let nmodes = if pc == 1 && lref.is_some() {
            MODE_CCLM + 1
        } else {
            predict::NUM_ANGULAR_MODES
        };
        let mut pred = vec![0i32; nn];
        let mut res = [0f32; 64 * 64];
        let mut cands: Vec<(f32, u8, f32)> = Vec::with_capacity(nmodes as usize);
        for mode in 0..nmodes {
            predict_mode(&pe.plane, &refs, lref, mode, x0, y0, n, &mut pred);
            for i in 0..nn {
                res[i] = src[i] - pred[i] as f32;
            }
            let mut cc = CostCounter::default();
            code_mode(&mut self.ctx, &mut cc, pc, n, mpm, mode);
            let s = dct::satd(&res[..nn], n);
            cands.push((s + slam * cc.bits, mode, cc.bits));
        }
        cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let w = &self.qt.w[pc][size_index(n)];
        let qs: Vec<f32> = w.iter().map(|&wi| wi * q).collect();
        let mut best = (f32::INFINITY, 0u8, vec![0i32; nn]);
        let mut levels = vec![0i32; nn];
        for &(_, mode, mbits) in cands.iter().take(self.topk) {
            predict_mode(&pe.plane, &refs, lref, mode, x0, y0, n, &mut pred);
            for i in 0..nn {
                res[i] = src[i] - pred[i] as f32;
            }
            dct::forward(&mut res[..nn], n);
            let (d, bits) =
                self.ctx
                    .coef
                    .quantize_rdo(pc, n, cctx, &res[..nn], &qs, lambda, &mut levels);
            let j = d + lambda * (bits + mbits);
            if j < best.0 {
                best = (j, mode, levels.clone());
            }
        }
        let (j, mode, levels) = best;
        predict_mode(&pe.plane, &refs, lref, mode, x0, y0, n, &mut pred);
        let mut recon = vec![0u8; nn];
        reconstruct(&levels, &pred, q, w, n, &mut recon);
        let nz = levels.iter().any(|&l| l != 0);
        (j, mode, levels, recon, nz)
    }

    fn rdo_node(
        &mut self,
        pe: &mut PlaneEnc,
        lref: Option<&LumaRef>,
        qm: &QMap,
        x0: usize,
        y0: usize,
        n: usize,
    ) -> (f32, Node) {
        let q = qm.q(pe.pc, x0, y0, n);
        let lambda = self.lambda_k * q * q;
        let (mut ja, mode, levels, recon, nz) = self.eval_leaf(pe, lref, x0, y0, n, q);
        if n == 4 {
            pe.plane.write_block(x0, y0, n, &recon);
            pe.plane.set_block(x0, y0, n, mode, nz);
            return (ja, Node::Leaf { mode, levels });
        }
        let sctx = split_ctx(&pe.plane, x0, y0, n, pe.pc);
        ja += lambda * self.ctx.split[sctx].cost(false);
        let h = n / 2;
        let mut jb = lambda * self.ctx.split[sctx].cost(true);
        let mut kids: Vec<Node> = Vec::with_capacity(4);
        for (dx, dy) in CHILDREN {
            let (j, node) = self.rdo_node(pe, lref, qm, x0 + dx * h, y0 + dy * h, h);
            jb += j;
            kids.push(node);
        }
        if ja <= jb {
            pe.plane.write_block(x0, y0, n, &recon);
            pe.plane.set_block(x0, y0, n, mode, nz);
            (ja, Node::Leaf { mode, levels })
        } else {
            let arr: [Node; 4] = kids.try_into().ok().unwrap();
            (jb, Node::Split(Box::new(arr)))
        }
    }

    fn write_node(
        &mut self,
        enc: &mut Encoder,
        p: &Plane,
        pc: usize,
        x0: usize,
        y0: usize,
        n: usize,
        node: &Node,
    ) {
        if n > 4 {
            let sctx = split_ctx(p, x0, y0, n, pc);
            enc.bit(&mut self.ctx.split[sctx], matches!(node, Node::Split(_)));
        }
        match node {
            Node::Split(kids) => {
                let h = n / 2;
                for (k, (dx, dy)) in CHILDREN.iter().enumerate() {
                    self.write_node(enc, p, pc, x0 + dx * h, y0 + dy * h, h, &kids[k]);
                }
            }
            Node::Leaf { mode, levels } => {
                let mpm = mpm_list(p, x0, y0);
                code_mode(&mut self.ctx, enc, pc, n, mpm, *mode);
                let mut l = levels.clone();
                self.ctx
                    .coef
                    .code_block(enc, pc, n, cbf_ctx(p, x0, y0), &mut l);
            }
        }
    }
}

/// Cálculo heurístico del dq por superbloque (enmascaramiento por actividad).
fn compute_aq(src: &[f32], w: usize, h: usize, strength: f32) -> Vec<i32> {
    let (sw, sh) = (w / AQ, h / AQ);
    if strength == 0.0 {
        return vec![0; sw * sh];
    }
    let mut act = vec![0f32; sw * sh];
    for sy in 0..sh {
        for sx in 0..sw {
            let mut tot = 0f32;
            let k = AQ / 8;
            for by in 0..k {
                for bx in 0..k {
                    let (x0, y0) = (sx * AQ + bx * 8, sy * AQ + by * 8);
                    let mut s = 0f32;
                    let mut s2 = 0f32;
                    for y in 0..8 {
                        for x in 0..8 {
                            let v = src[(y0 + y) * w + x0 + x];
                            s += v;
                            s2 += v * v;
                        }
                    }
                    let var = (s2 - s * s / 64.0) / 64.0;
                    tot += (1.0 + var).log2();
                }
            }
            act[sy * sw + sx] = tot / (k * k) as f32;
        }
    }
    let mean = act.iter().sum::<f32>() / act.len() as f32;
    act.iter()
        .map(|&a| (strength * (a - mean)).round().clamp(-8.0, 8.0) as i32)
        .collect()
}

fn pad_plane(src: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let pw = w.div_ceil(SB) * SB;
    let ph = h.div_ceil(SB) * SB;
    let mut out = vec![0f32; pw * ph];
    for y in 0..ph {
        let sy = y.min(h - 1);
        for x in 0..pw {
            out[y * pw + x] = src[sy * w + x.min(w - 1)];
        }
    }
    (out, pw, ph)
}

/// Resultado de la codificación: bytes y la reconstrucción que verá el decodificador.
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub recon_rgb: Vec<u8>,
}

pub fn encode(rgb: &[u8], width: usize, height: usize, p: &Params) -> Encoded {
    let base_q = quality_to_qstep(p.quality);
    let sub = p.subsample.unwrap_or(p.quality < 90.0);
    let header = Header {
        width: width as u32,
        height: height as u32,
        subsample: sub,
        qstep: (base_q * 64.0).round().clamp(1.0, 65535.0) as u16,
        chroma_q: (p.chroma_q * 32.0).round().clamp(1.0, 255.0) as u8,
        qm_luma: (p.qm_luma * 16.0).round().clamp(0.0, 255.0) as u8,
        qm_chroma: (p.qm_chroma * 16.0).round().clamp(0.0, 255.0) as u8,
        aq: p.aq_strength != 0.0,
        deblock: p.deblock.unwrap_or(0),
    };
    let base_q = header.qstep();
    let cq = header.chroma_q as f32 / 32.0;
    let ycc = color::rgb_to_ycbcr(rgb, width, height, sub);

    let mut st = EncState {
        ctx: Ctx::new(),
        qt: QuantTables::new(header.qm_luma as f32 / 16.0, header.qm_chroma as f32 / 16.0),
        lambda_k: p.lambda_k,
        topk: (p.effort.clamp(1, 9) as usize) + 1,
    };
    let mut enc = Encoder::new();
    let mut planes: Vec<Plane> = Vec::new();
    let mut qm = QMap {
        dq: Vec::new(),
        w: 0,
        h: 0,
        base: base_q,
        cq,
        sub,
    };
    let mut lref: Option<LumaRef> = None;

    for (pi, (src, w, h)) in ycc.planes.iter().enumerate() {
        let (src, pw, ph) = pad_plane(src, *w, *h);
        let pc = (pi > 0) as usize;
        if pi == 0 {
            qm.dq = compute_aq(&src, pw, ph, p.aq_strength);
            qm.w = pw / AQ;
            qm.h = ph / AQ;
        }
        let mut pe = PlaneEnc {
            src: &src,
            plane: Plane::new(pw, ph),
            pc,
        };
        for sy in 0..ph / SB {
            for sx in 0..pw / SB {
                if pi == 0 && header.aq {
                    qm.code_sb(&mut st.ctx, &mut enc, sx, sy);
                }
                let (_, node) = st.rdo_node(&mut pe, lref.as_ref(), &qm, sx * SB, sy * SB, SB);
                st.write_node(&mut enc, &pe.plane, pc, sx * SB, sy * SB, SB, &node);
            }
        }
        if pi == 0 {
            lref = Some(LumaRef::new(&pe.plane, sub));
        }
        planes.push(pe.plane);
    }
    let mut bytes = Vec::new();
    header.write(&mut bytes);
    bytes.extend_from_slice(&enc.finish());
    let recon_rgb = finish_planes(&header, &planes);
    Encoded { bytes, recon_rgb }
}

fn finish_planes(h: &Header, planes: &[Plane]) -> Vec<u8> {
    let (w, ht) = (h.width as usize, h.height as usize);
    let mut ps: Vec<Plane> = planes.to_vec();
    if h.deblock > 0 {
        for (i, p) in ps.iter_mut().enumerate() {
            crate::deblock::filter(
                p,
                h.qstep() * if i > 0 { h.chroma_q as f32 / 32.0 } else { 1.0 },
                h.deblock,
            );
        }
    }
    color::ycbcr_to_rgb(
        &ps[0].data,
        ps[0].w,
        &ps[1].data,
        &ps[2].data,
        ps[1].w,
        w,
        ht,
        h.subsample,
    )
}

// ----------------------------------------------------------- Decodificador

struct DecState {
    ctx: Ctx,
    qt: QuantTables,
}

impl DecState {
    #[allow(clippy::too_many_arguments)]
    fn read_node(
        &mut self,
        dec: &mut Decoder,
        p: &mut Plane,
        lref: Option<&LumaRef>,
        qm: &QMap,
        pc: usize,
        x0: usize,
        y0: usize,
        n: usize,
    ) {
        if n > 4 {
            let sctx = split_ctx(p, x0, y0, n, pc);
            if dec.bit(&mut self.ctx.split[sctx], false) {
                let h = n / 2;
                for (dx, dy) in CHILDREN {
                    self.read_node(dec, p, lref, qm, pc, x0 + dx * h, y0 + dy * h, h);
                }
                return;
            }
        }
        let mpm = mpm_list(p, x0, y0);
        let mode = code_mode(&mut self.ctx, dec, pc, n, mpm, 0);
        let mode = if mode == MODE_CCLM && lref.is_none() {
            MODE_DC
        } else {
            mode
        };
        let mut levels = vec![0i32; n * n];
        let nz = self
            .ctx
            .coef
            .code_block(dec, pc, n, cbf_ctx(p, x0, y0), &mut levels);
        let refs = predict::build_refs(p, x0, y0, n);
        let mut pred = vec![0i32; n * n];
        predict_mode(p, &refs, lref, mode.min(MODE_CCLM), x0, y0, n, &mut pred);
        let mut recon = vec![0u8; n * n];
        let q = qm.q(pc, x0, y0, n);
        reconstruct(
            &levels,
            &pred,
            q,
            &self.qt.w[pc][size_index(n)],
            n,
            &mut recon,
        );
        p.write_block(x0, y0, n, &recon);
        p.set_block(x0, y0, n, mode, nz);
    }
}

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let h = Header::read(data)?;
    let (w, ht) = (h.width as usize, h.height as usize);
    let base_q = h.qstep();
    let cq = h.chroma_q as f32 / 32.0;
    let (cw, ch) = if h.subsample {
        (w.div_ceil(2), ht.div_ceil(2))
    } else {
        (w, ht)
    };
    let dims = [(w, ht), (cw, ch), (cw, ch)];
    let mut st = DecState {
        ctx: Ctx::new(),
        qt: QuantTables::new(h.qm_luma as f32 / 16.0, h.qm_chroma as f32 / 16.0),
    };
    let mut dec = Decoder::new(&data[HEADER_LEN..]);
    let mut planes = Vec::new();
    let mut qm = QMap {
        dq: Vec::new(),
        w: 0,
        h: 0,
        base: base_q,
        cq,
        sub: h.subsample,
    };
    let mut lref: Option<LumaRef> = None;
    for (pi, &(pw0, ph0)) in dims.iter().enumerate() {
        let pw = pw0.div_ceil(SB) * SB;
        let ph = ph0.div_ceil(SB) * SB;
        let pc = (pi > 0) as usize;
        let mut plane = Plane::new(pw, ph);
        if pi == 0 {
            qm.w = pw / AQ;
            qm.h = ph / AQ;
            qm.dq = vec![0; qm.w * qm.h];
        }
        for sy in 0..ph / SB {
            for sx in 0..pw / SB {
                if pi == 0 && h.aq {
                    qm.code_sb(&mut st.ctx, &mut dec, sx, sy);
                }
                st.read_node(
                    &mut dec,
                    &mut plane,
                    lref.as_ref(),
                    &qm,
                    pc,
                    sx * SB,
                    sy * SB,
                    SB,
                );
            }
        }
        if pi == 0 {
            lref = Some(LumaRef::new(&plane, h.subsample));
        }
        planes.push(plane);
    }
    let rgb = finish_planes(&h, &planes);
    Ok(Decoded {
        width: w,
        height: ht,
        rgb,
    })
}
