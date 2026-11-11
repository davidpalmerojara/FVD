//! Codificador y decodificador Tesela.
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
use crate::rc::{Coder, CostCounter, Decoder, Encoder, Model, SintCtx, TreeCtx, Visit};
use crate::restore;
use crate::tools::{self, BvCtx, MARK_IBC, MARK_PALETTE, PalCtx, Palette};

pub const HEADER_SIZE: usize = HEADER_LEN;
pub const MAGIC: &[u8; 4] = b"TSL1";
const SB: usize = 64;
/// Granularidad de la cuantización adaptativa (AQ).
const AQ: usize = 32;
const HEADER_LEN: usize = 4 + 4 + 4 + 1 + 2 + 1 + 1 + 1 + 1 + 1 + 2 + 1;
/// Bit de `Header::tools`: herramientas de contenido sintético (paleta, IBC, transform skip).
pub const TOOL_SCREEN: u8 = 1;
/// Modo sin pérdida (planos G, B, R; residuo exacto).
pub const TOOL_LOSSLESS: u8 = 2;
/// Canal alfa (plano adicional sin pérdida).
pub const TOOL_ALPHA: u8 = 4;
/// Vista previa diminuta al principio del archivo (para mostrar algo mientras llega el resto).
pub const TOOL_PREVIEW: u8 = 8;
/// Lado mayor de la vista previa, en píxeles.
const PREVIEW_SIDE: usize = 48;

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
    /// Tamaño de las teselas independientes (múltiplo de 64).
    pub tile: usize,
    /// Filtro de restauración Wiener.
    pub restore: bool,
    /// Herramientas de contenido sintético (None = detección automática).
    pub screen: Option<bool>,
    /// Compresión sin pérdida.
    pub lossless: bool,
    /// Incluir vista previa progresiva.
    pub preview: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            quality: 75.0,
            subsample: None,
            effort: 4,
            aq_strength: 1.0,
            chroma_q: 0.8,
            qm_luma: 1.5,
            qm_chroma: 3.0,
            lambda_k: 0.11,
            deblock: Some(0),
            tile: 1024,
            restore: true,
            screen: None,
            lossless: false,
            preview: false,
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
        self.lambda_k = 0.12;
        self
    }

    /// Ajuste perceptual fuerte: maximiza SSIMULACRA2 a costa de PSNR.
    pub fn tune_perceptual(mut self) -> Self {
        self.aq_strength = 1.0;
        self.chroma_q = 0.6;
        self.qm_luma = 3.0;
        self.qm_chroma = 3.0;
        self.lambda_k = 0.12;
        self
    }
}

/// Calidad 0..100 → paso de cuantización. Usa la misma curva que el factor
/// de escala de JPEG (5000/q por debajo de 50, 200−2q por encima), calibrada
/// para que una calidad dada se vea parecida a un JPEG de la misma calidad.
pub fn quality_to_qstep(q: f32) -> f32 {
    let q = q.clamp(1.0, 100.0);
    let scale = if q < 50.0 {
        5000.0 / q
    } else {
        200.0 - 2.0 * q
    };
    (0.257 * scale).max(0.5)
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
    /// Tamaño de tesela en unidades de 64 píxeles.
    pub tile: u16,
    /// Herramientas activadas (bits `TOOL_*`).
    pub tools: u8,
}

impl Header {
    pub fn tile_size(&self) -> usize {
        self.tile as usize * 64
    }

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
        out.extend_from_slice(&self.tile.to_le_bytes());
        out.push(self.tools);
    }

    pub fn read(d: &[u8]) -> Result<Header, String> {
        if d.len() < HEADER_LEN || &d[..4] != MAGIC {
            return Err("no es un archivo Tesela".into());
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
            tile: u16::from_le_bytes([d[20], d[21]]),
            tools: d[22],
        };
        if h.tile == 0 {
            return Err("tamaño de tesela no válido".into());
        }
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
    rest: restore::RestCtx,
    coef_ts: CoefCtx,
    ts: Vec<Model>,
    special: Vec<Model>,
    pal_or_ibc: [Model; 2],
    pal: PalCtx,
    bv: BvCtx,
}

impl Visit for Ctx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.coef.visit(f);
        self.split.visit(f);
        self.mpm_flag.visit(f);
        self.mpm_idx.visit(f);
        self.rem_mode.visit(f);
        self.cclm.visit(f);
        self.dq.visit(f);
        self.rest.visit(f);
        self.coef_ts.visit(f);
        self.ts.visit(f);
        self.special.visit(f);
        self.pal_or_ibc.visit(f);
        self.pal.visit(f);
        self.bv.visit(f);
    }
}

/// Banda de calidad (selecciona la tabla de probabilidades iniciales).
pub fn quality_band(qstep: f32) -> usize {
    if qstep < 2.5 {
        0
    } else if qstep < 6.0 {
        1
    } else if qstep < 16.0 {
        2
    } else {
        3
    }
}

/// Desactiva las tablas entrenadas (solo para entrenar).
pub static UNTRAINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Ctx {
    fn trained(qstep: f32, screen: bool) -> Self {
        let mut c = Ctx::new();
        let t = crate::init_probs::TABLES[screen as usize][quality_band(qstep)];
        if UNTRAINED.load(std::sync::atomic::Ordering::Relaxed) {
            return c;
        }
        let mut n = 0;
        c.visit(&mut |_| n += 1);
        if t.len() == n {
            let mut i = 0;
            c.visit(&mut |m| {
                m.set_init(t[i]);
                i += 1;
            });
        }
        c
    }

    /// Estado final de todos los modelos: (probabilidad, observaciones).
    fn stats(&mut self) -> Vec<(u32, u32)> {
        let mut v = Vec::new();
        self.visit(&mut |m| v.push(m.stats()));
        v
    }

    fn new() -> Self {
        Ctx {
            coef: CoefCtx::default(),
            split: vec![Model::new(); 2 * 4 * 4],
            mpm_flag: vec![Model::new(); 2 * 5],
            mpm_idx: [[Model::new(); 2]; 2],
            rem_mode: [TreeCtx::new(5), TreeCtx::new(5)],
            cclm: [Model::new(); 5],
            dq: SintCtx::default(),
            rest: restore::RestCtx::default(),
            coef_ts: CoefCtx::default(),
            ts: vec![Model::new(); 2 * 3],
            special: vec![Model::new(); 2 * 5 * 3],
            pal_or_ibc: Default::default(),
            pal: PalCtx::default(),
            bv: BvCtx::default(),
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

/// Clase de predicción de una hoja.
#[derive(Clone, Debug)]
enum Kind {
    Intra(u8),
    Palette(Palette),
    Ibc((i32, i32)),
}

impl Kind {
    /// Valor del mapa de modos (para el contexto de los vecinos).
    fn mark(&self) -> u8 {
        match self {
            Kind::Intra(m) => *m,
            Kind::Palette(_) => MARK_PALETTE,
            Kind::Ibc(_) => MARK_IBC,
        }
    }
}

#[derive(Clone, Debug)]
struct Leaf {
    kind: Kind,
    /// Residuo sin transformar (transform skip).
    ts: bool,
    levels: Vec<i32>,
}

enum Node {
    Split(Box<[Node; 4]>),
    Leaf(Box<Leaf>),
}

const CHILDREN: [(usize, usize); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];

/// Estado de las herramientas sintéticas compartido por codificador y
/// decodificador dentro de un plano (última paleta y último vector).
#[derive(Clone, Default)]
struct ScState {
    last_pal: Option<Vec<u8>>,
    last_bv: (i32, i32),
}

fn special_ctx(p: &Plane, x0: usize, y0: usize) -> usize {
    let sp = |v: Option<(u8, u8, bool)>| v.is_some_and(|(m, _, _)| m >= MARK_PALETTE) as usize;
    sp(p.neighbor(x0, y0, true)) + sp(p.neighbor(x0, y0, false))
}

/// Sintaxis de la cabecera de una hoja (clase de predicción y transform
/// skip). Simétrica: el decodificador pasa una hoja ficticia.
#[allow(clippy::too_many_arguments)]
fn code_leaf_head<C: Coder>(
    ctx: &mut Ctx,
    c: &mut C,
    p: &Plane,
    pc: usize,
    x0: usize,
    y0: usize,
    n: usize,
    screen: bool,
    lossless: bool,
    leaf: &Leaf,
    st: &mut ScState,
) -> (Kind, bool) {
    let sz = size_index(n);
    let mut kind = None;
    if screen {
        let special = !matches!(leaf.kind, Kind::Intra(_));
        if c.bit(
            &mut ctx.special[(pc * 5 + sz) * 3 + special_ctx(p, x0, y0)],
            special,
        ) {
            let is_pal = c.bit(
                &mut ctx.pal_or_ibc[pc],
                matches!(leaf.kind, Kind::Palette(_)),
            );
            kind = Some(if is_pal {
                let dummy = Palette {
                    colors: vec![0, 1],
                    idx: Vec::new(),
                };
                let pal = match &leaf.kind {
                    Kind::Palette(pl) => pl,
                    _ => &dummy,
                };
                Kind::Palette(ctx.pal.code(c, pc, n, n, pal, &mut st.last_pal))
            } else {
                let bv = match leaf.kind {
                    Kind::Ibc(b) => b,
                    _ => (0, 0),
                };
                Kind::Ibc(ctx.bv.code(c, pc, bv, &mut st.last_bv))
            });
        }
    }
    let kind = kind.unwrap_or_else(|| {
        let m = match leaf.kind {
            Kind::Intra(m) => m,
            _ => MODE_DC,
        };
        Kind::Intra(code_mode(ctx, c, pc, n, mpm_list(p, x0, y0), m))
    });
    let ts = lossless || (screen && n <= 16 && c.bit(&mut ctx.ts[pc * 3 + sz.min(2)], leaf.ts));
    (kind, ts)
}

/// Reconstruye un bloque: descuantiza, transformada inversa (salvo
/// transform skip) y suma la predicción.
fn reconstruct(
    levels: &[i32],
    pred: &[i32],
    q: f32,
    w: &[f32],
    n: usize,
    ts: bool,
    out: &mut [u8],
) {
    let mut buf = vec![0f32; n * n];
    let mut any = false;
    for i in 0..n * n {
        if levels[i] != 0 {
            buf[i] = levels[i] as f32 * q * if ts { 1.0 } else { w[i] };
            any = true;
        }
    }
    if any && !ts {
        if levels[1..n * n].iter().all(|&l| l == 0) {
            // Solo DC: la inversa es constante (base DC ortonormal = 1/n).
            let v = buf[0] / n as f32;
            buf.fill(v);
        } else {
            dct::inverse(&mut buf[..n * n], n);
        }
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
        predict::predict_cclm(p, &l.data, l.w, l.h, x0, y0, n, n, out);
    } else {
        predict::predict(refs, mode, n, n, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_leaf(
    p: &Plane,
    refs: &predict::Refs,
    lref: Option<&LumaRef>,
    kind: &Kind,
    x0: usize,
    y0: usize,
    n: usize,
    out: &mut [i32],
) {
    match kind {
        Kind::Intra(m) => predict_mode(p, refs, lref, *m, x0, y0, n, out),
        Kind::Palette(pal) => tools::predict_palette(pal, out),
        Kind::Ibc(bv) => {
            if tools::bv_valid(p, x0, y0, n, n, *bv) {
                tools::predict_ibc(p, x0, y0, n, n, *bv, out)
            } else {
                out[..n * n].fill(128)
            }
        }
    }
}

// ------------------------------------------------------------- Codificador

struct PlaneEnc<'a> {
    src: &'a [f32],
    plane: Plane,
    pc: usize,
    /// Índice hash para la búsqueda de IBC (solo con herramientas sintéticas).
    hash: Option<tools::HashIndex>,
    /// Estado sintético aproximado durante el RDO.
    sc: ScState,
}

struct EncState {
    ctx: Ctx,
    qt: QuantTables,
    lambda_k: f32,
    topk: usize,
    screen: bool,
    lossless: bool,
}

/// Resultado de evaluar una hoja.
struct LeafEval {
    j: f32,
    leaf: Leaf,
    recon: Vec<u8>,
    nz: bool,
}

impl EncState {
    /// Coste en bits de la cabecera de una hoja con el estado actual.
    #[allow(clippy::too_many_arguments)]
    fn head_bits(
        &mut self,
        pe: &PlaneEnc,
        x0: usize,
        y0: usize,
        n: usize,
        kind: &Kind,
        ts: bool,
    ) -> f32 {
        let mut cc = CostCounter::default();
        let mut st = pe.sc.clone();
        let leaf = Leaf {
            kind: kind.clone(),
            ts,
            levels: Vec::new(),
        };
        code_leaf_head(
            &mut self.ctx,
            &mut cc,
            &pe.plane,
            pe.pc,
            x0,
            y0,
            n,
            self.screen,
            self.lossless,
            &leaf,
            &mut st,
        );
        cc.bits
    }

    /// Mejor codificación del residuo de una predicción (DCT y, si procede,
    /// transform skip). Devuelve (J sin cabecera, niveles, ts).
    #[allow(clippy::too_many_arguments)]
    fn rd_residual(
        &self,
        pc: usize,
        n: usize,
        cctx: usize,
        src: &[f32],
        pred: &[i32],
        q: f32,
        lambda: f32,
        try_ts: bool,
        allow_dct: bool,
    ) -> (f32, Vec<i32>, bool) {
        let nn = n * n;
        let mut res = vec![0f32; nn];
        let mut best = (f32::INFINITY, Vec::new(), false);
        let mut levels = vec![0i32; nn];
        if self.lossless {
            // Residuo exacto, sin cuantizar: solo cuenta la tasa.
            for i in 0..nn {
                levels[i] = (src[i] - pred[i] as f32).round() as i32;
            }
            let bits = self.ctx.coef_ts.block_cost(pc, n, n, cctx, &levels);
            return (lambda * bits, levels, true);
        }
        if allow_dct {
            for i in 0..nn {
                res[i] = src[i] - pred[i] as f32;
            }
            dct::forward(&mut res[..nn], n);
            let w = &self.qt.w[pc][size_index(n)];
            let qs: Vec<f32> = w.iter().map(|&wi| wi * q).collect();
            let (d, bits) =
                self.ctx
                    .coef
                    .quantize_rdo(pc, n, n, cctx, &res[..nn], &qs, lambda, &mut levels);
            best = (d + lambda * bits, levels.clone(), false);
        }
        if try_ts {
            for i in 0..nn {
                res[i] = src[i] - pred[i] as f32;
            }
            let qs = vec![q; nn];
            let (d, bits) =
                self.ctx
                    .coef_ts
                    .quantize_rdo(pc, n, n, cctx, &res[..nn], &qs, lambda, &mut levels);
            let j = d + lambda * bits;
            if j < best.0 {
                best = (j, levels.clone(), true);
            }
        }
        best
    }

    fn eval_leaf(
        &mut self,
        pe: &mut PlaneEnc,
        lref: Option<&LumaRef>,
        x0: usize,
        y0: usize,
        n: usize,
        q: f32,
    ) -> LeafEval {
        let lambda = self.lambda_k * q * q;
        let slam = lambda.sqrt();
        let pc = pe.pc;
        let nn = n * n;
        let mut src = vec![0f32; nn];
        for y in 0..n {
            for x in 0..n {
                src[y * n + x] = pe.src[(y0 + y) * pe.plane.w + x0 + x];
            }
        }
        let refs = predict::build_refs(&pe.plane, x0, y0, n, n);
        let mpm = mpm_list(&pe.plane, x0, y0);
        let cctx = cbf_ctx(&pe.plane, x0, y0);
        let nmodes = if pc == 1 && lref.is_some() {
            MODE_CCLM + 1
        } else {
            predict::NUM_ANGULAR_MODES
        };
        let try_ts = self.screen && n <= 16;
        let mut pred = vec![0i32; nn];
        let mut res = vec![0f32; nn];
        let mut cands: Vec<(f32, u8, f32)> = Vec::with_capacity(nmodes as usize);
        // Búsqueda intra en dos fases: barrido grueso y refinado angular.
        let mut done = [false; 64];
        let mut todo: Vec<u8> = vec![MODE_PLANAR, MODE_DC];
        todo.extend((2..predict::NUM_ANGULAR_MODES).step_by(4));
        todo.extend_from_slice(&mpm);
        if nmodes > predict::NUM_ANGULAR_MODES {
            todo.push(MODE_CCLM);
        }
        for pass in 0..2 {
            if pass == 1 {
                let mut ang: Vec<(f32, u8)> = cands
                    .iter()
                    .filter(|c| c.1 >= 2 && c.1 < MODE_CCLM)
                    .map(|c| (c.0, c.1))
                    .collect();
                ang.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                todo.clear();
                let width = if self.topk > 4 { 3 } else { 2 };
                for &(_, m) in ang.iter().take(2) {
                    for d in -width..=width {
                        let mm = m as i32 + d;
                        if (2..predict::NUM_ANGULAR_MODES as i32).contains(&mm) {
                            todo.push(mm as u8);
                        }
                    }
                }
            }
            for &mode in &todo {
                if done[mode as usize] {
                    continue;
                }
                done[mode as usize] = true;
                predict_mode(&pe.plane, &refs, lref, mode, x0, y0, n, &mut pred);
                for i in 0..nn {
                    res[i] = src[i] - pred[i] as f32;
                }
                let mut cc = CostCounter::default();
                code_mode(&mut self.ctx, &mut cc, pc, n, mpm, mode);
                let s = dct::satd(&res[..nn], n);
                cands.push((s + slam * cc.bits, mode, cc.bits));
            }
        }
        cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut best: Option<(f32, Kind, Vec<i32>, bool)> = None;
        let consider = |best: &mut Option<(f32, Kind, Vec<i32>, bool)>,
                        j: f32,
                        kind: Kind,
                        lv: Vec<i32>,
                        ts: bool| {
            if best.as_ref().is_none_or(|b| j < b.0) {
                *best = Some((j, kind, lv, ts));
            }
        };
        // Solo pasan a RDO completo los modos cuyo coste SATD está cerca del mejor.
        let satd_limit = cands[0].0 * 1.3 + slam * 4.0;
        for &(sc, mode, _) in cands.iter().take(self.topk) {
            if sc > satd_limit {
                break;
            }
            predict_mode(&pe.plane, &refs, lref, mode, x0, y0, n, &mut pred);
            let (jr, lv, ts) = self.rd_residual(pc, n, cctx, &src, &pred, q, lambda, try_ts, true);
            let kind = Kind::Intra(mode);
            let hb = self.head_bits(pe, x0, y0, n, &kind, ts);
            consider(&mut best, jr + lambda * hb, kind, lv, ts);
        }
        if self.screen && n <= 32 {
            // Paleta
            for pal in tools::palette_candidates(&src, n, n) {
                tools::predict_palette(&pal, &mut pred);
                let (jr, lv, ts) =
                    self.rd_residual(pc, n, cctx, &src, &pred, q, lambda, try_ts, true);
                let kind = Kind::Palette(pal);
                let hb = self.head_bits(pe, x0, y0, n, &kind, ts);
                consider(&mut best, jr + lambda * hb, kind, lv, ts);
            }
            // Copia intra de bloques
            if n >= 8 {
                let mut bvs: Vec<(i32, i32)> =
                    vec![pe.sc.last_bv, (-(n as i32), 0), (0, -(n as i32))];
                if let Some(h) = &pe.hash {
                    bvs.extend(h.candidates(pe.src, pe.plane.w, x0, y0));
                }
                let mut scored: Vec<(f32, (i32, i32))> = Vec::new();
                for bv in bvs {
                    if scored.iter().any(|s| s.1 == bv)
                        || !tools::bv_valid(&pe.plane, x0, y0, n, n, bv)
                    {
                        continue;
                    }
                    tools::predict_ibc(&pe.plane, x0, y0, n, n, bv, &mut pred);
                    let sad: f32 = (0..nn).map(|i| (src[i] - pred[i] as f32).abs()).sum();
                    let bits = self.ctx.bv.cost(pc, bv, pe.sc.last_bv);
                    scored.push((sad + slam * bits, bv));
                }
                scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                for &(_, bv) in scored.iter().take(2) {
                    tools::predict_ibc(&pe.plane, x0, y0, n, n, bv, &mut pred);
                    let (jr, lv, ts) =
                        self.rd_residual(pc, n, cctx, &src, &pred, q, lambda, try_ts, true);
                    let kind = Kind::Ibc(bv);
                    let hb = self.head_bits(pe, x0, y0, n, &kind, ts);
                    consider(&mut best, jr + lambda * hb, kind, lv, ts);
                }
            }
        }
        let (j, kind, levels, ts) = best.expect("al menos un candidato");
        predict_leaf(&pe.plane, &refs, lref, &kind, x0, y0, n, &mut pred);
        let mut recon = vec![0u8; nn];
        reconstruct(
            &levels,
            &pred,
            q,
            &self.qt.w[pc][size_index(n)],
            n,
            ts,
            &mut recon,
        );
        let nz = levels.iter().any(|&l| l != 0);
        LeafEval {
            j,
            leaf: Leaf { kind, ts, levels },
            recon,
            nz,
        }
    }

    /// Aplica una hoja elegida al plano y al estado sintético aproximado.
    fn commit_leaf(pe: &mut PlaneEnc, x0: usize, y0: usize, n: usize, ev: &LeafEval) {
        pe.plane.write_block(x0, y0, n, n, &ev.recon);
        pe.plane.set_block(x0, y0, n, n, ev.leaf.kind.mark(), ev.nz);
        match &ev.leaf.kind {
            Kind::Palette(p) => pe.sc.last_pal = Some(p.colors.clone()),
            Kind::Ibc(bv) => pe.sc.last_bv = *bv,
            Kind::Intra(_) => {}
        }
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
        let sc_before = pe.sc.clone();
        let mut ev = self.eval_leaf(pe, lref, x0, y0, n, q);
        if n == 4 {
            Self::commit_leaf(pe, x0, y0, n, &ev);
            let j = ev.j;
            return (j, Node::Leaf(Box::new(ev.leaf)));
        }
        let sctx = split_ctx(&pe.plane, x0, y0, n, pe.pc);
        ev.j += lambda * self.ctx.split[sctx].cost(false);
        let h = n / 2;
        let mut jb = lambda * self.ctx.split[sctx].cost(true);
        let mut kids: Vec<Node> = Vec::with_capacity(4);
        for (dx, dy) in CHILDREN {
            let (j, node) = self.rdo_node(pe, lref, qm, x0 + dx * h, y0 + dy * h, h);
            jb += j;
            kids.push(node);
            // Poda exacta: los costes son no negativos, así que si la suma
            // parcial ya supera al bloque sin dividir, dividir no puede ganar.
            if jb >= ev.j {
                break;
            }
        }
        if ev.j <= jb {
            pe.sc = sc_before;
            Self::commit_leaf(pe, x0, y0, n, &ev);
            let j = ev.j;
            (j, Node::Leaf(Box::new(ev.leaf)))
        } else {
            let arr: [Node; 4] = kids.try_into().ok().unwrap();
            (jb, Node::Split(Box::new(arr)))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_node(
        &mut self,
        enc: &mut Encoder,
        p: &Plane,
        pc: usize,
        st: &mut ScState,
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
                    self.write_node(enc, p, pc, st, x0 + dx * h, y0 + dy * h, h, &kids[k]);
                }
            }
            Node::Leaf(leaf) => {
                code_leaf_head(
                    &mut self.ctx,
                    enc,
                    p,
                    pc,
                    x0,
                    y0,
                    n,
                    self.screen,
                    self.lossless,
                    leaf,
                    st,
                );
                let mut l = leaf.levels.clone();
                let coef = if leaf.ts {
                    &mut self.ctx.coef_ts
                } else {
                    &mut self.ctx.coef
                };
                coef.code_block(enc, pc, n, n, cbf_ctx(p, x0, y0), &mut l);
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
    /// Alfa reconstruido (si la imagen tiene transparencia).
    pub recon_alpha: Option<Vec<u8>>,
}

/// Rectángulo de una tesela de la imagen (en píxeles).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

pub fn tile_grid(width: usize, height: usize, tile: usize) -> Vec<TileRect> {
    let mut v = Vec::new();
    for y in (0..height).step_by(tile) {
        for x in (0..width).step_by(tile) {
            v.push(TileRect {
                x,
                y,
                w: tile.min(width - x),
                h: tile.min(height - y),
            });
        }
    }
    v
}

#[cfg(feature = "parallel")]
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    use rayon::prelude::*;
    items.par_iter().map(f).collect()
}

#[cfg(not(feature = "parallel"))]
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    items.iter().map(f).collect()
}

fn crop(px: &[u8], ch: usize, width: usize, t: &TileRect) -> Vec<u8> {
    let mut out = Vec::with_capacity(t.w * t.h * ch);
    for y in t.y..t.y + t.h {
        out.extend_from_slice(&px[(y * width + t.x) * ch..(y * width + t.x + t.w) * ch]);
    }
    out
}

fn paste(dst: &mut [u8], ch: usize, width: usize, t: &TileRect, src: &[u8]) {
    for y in 0..t.h {
        dst[((t.y + y) * width + t.x) * ch..((t.y + y) * width + t.x + t.w) * ch]
            .copy_from_slice(&src[y * t.w * ch..(y + 1) * t.w * ch]);
    }
}

/// Cabecera para una imagen. Analiza el contenido para decidir si activar
/// las herramientas de contenido sintético (salvo que `p.screen` lo fije).
pub fn make_header(rgb: &[u8], width: usize, height: usize, p: &Params) -> Header {
    let screen = p
        .screen
        .unwrap_or_else(|| tools::looks_synthetic(rgb, width, height));
    let base_q = quality_to_qstep(p.quality);
    let tile = (p.tile.clamp(64, 65535 * 64) / 64) as u16;
    if p.lossless {
        return Header {
            width: width as u32,
            height: height as u32,
            subsample: false,
            qstep: 64,
            chroma_q: 32,
            qm_luma: 0,
            qm_chroma: 0,
            aq: false,
            deblock: 0,
            tile,
            tools: TOOL_SCREEN | TOOL_LOSSLESS,
        };
    }
    Header {
        width: width as u32,
        height: height as u32,
        subsample: p.subsample.unwrap_or(!screen && base_q > 1.04),
        qstep: (base_q * 64.0).round().clamp(1.0, 65535.0) as u16,
        chroma_q: (p.chroma_q * 32.0).round().clamp(1.0, 255.0) as u8,
        qm_luma: (p.qm_luma * 16.0).round().clamp(0.0, 255.0) as u8,
        qm_chroma: (p.qm_chroma * 16.0).round().clamp(0.0, 255.0) as u8,
        aq: p.aq_strength != 0.0,
        deblock: p.deblock.unwrap_or(0),
        tile,
        tools: if screen { TOOL_SCREEN } else { 0 },
    }
}

/// Une cabecera y cargas de las teselas en un archivo completo.
pub fn assemble(header: &Header, payloads: &[Vec<u8>]) -> Vec<u8> {
    assemble_with_preview(header, None, payloads)
}

/// Como `assemble`, con una vista previa opcional (un archivo .tsl completo).
pub fn assemble_with_preview(
    header: &Header,
    preview: Option<&[u8]>,
    payloads: &[Vec<u8>],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut h = header.clone();
    if preview.is_some() {
        h.tools |= TOOL_PREVIEW;
    } else {
        h.tools &= !TOOL_PREVIEW;
    }
    h.write(&mut bytes);
    if let Some(pv) = preview {
        bytes.extend_from_slice(&(pv.len() as u32).to_le_bytes());
        bytes.extend_from_slice(pv);
    }
    for p in payloads {
        bytes.extend_from_slice(&(p.len() as u32).to_le_bytes());
    }
    for p in payloads {
        bytes.extend_from_slice(p);
    }
    bytes
}

/// Codifica una imagen RGB (8 bits por canal, orden fila).
pub fn encode(rgb: &[u8], width: usize, height: usize, p: &Params) -> Encoded {
    encode_with_alpha(rgb, None, width, height, p)
}

/// Codifica una imagen RGBA. Si todos los píxeles son opacos no se guarda alfa.
pub fn encode_rgba(rgba: &[u8], width: usize, height: usize, p: &Params) -> Encoded {
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    let alpha: Vec<u8> = rgba.chunks_exact(4).map(|px| px[3]).collect();
    let has_alpha = alpha.iter().any(|&a| a != 255);
    encode_with_alpha(&rgb, has_alpha.then_some(&alpha[..]), width, height, p)
}

fn encode_with_alpha(
    rgb: &[u8],
    alpha: Option<&[u8]>,
    width: usize,
    height: usize,
    p: &Params,
) -> Encoded {
    let mut header = make_header(rgb, width, height, p);
    if alpha.is_some() {
        header.tools |= TOOL_ALPHA;
    }
    let tiles = tile_grid(width, height, header.tile_size());
    let res = par_map(&tiles, |t| {
        let a = alpha.map(|a| crop(a, 1, width, t));
        encode_tile(&crop(rgb, 3, width, t), a.as_deref(), t.w, t.h, &header, p)
    });
    let mut recon_rgb = vec![0u8; width * height * 3];
    let mut recon_alpha = alpha.map(|_| vec![0u8; width * height]);
    for (t, r) in tiles.iter().zip(&res) {
        paste(&mut recon_rgb, 3, width, t, &r.1);
        if let (Some(dst), Some(src)) = (recon_alpha.as_mut(), r.2.as_ref()) {
            paste(dst, 1, width, t, src);
        }
    }
    let payloads: Vec<Vec<u8>> = res.into_iter().map(|r| r.0).collect();
    let preview = (p.preview && width.max(height) > PREVIEW_SIDE)
        .then(|| make_preview(rgb, alpha, width, height));
    Encoded {
        bytes: assemble_with_preview(&header, preview.as_deref(), &payloads),
        recon_rgb,
        recon_alpha,
    }
}

/// Vista previa: la imagen reducida (promedio por áreas) a `PREVIEW_SIDE`
/// píxeles de lado mayor, codificada como un .tsl independiente.
fn make_preview(rgb: &[u8], alpha: Option<&[u8]>, w: usize, h: usize) -> Vec<u8> {
    let s = PREVIEW_SIDE as f64 / w.max(h) as f64;
    let (pw, ph) = (
        ((w as f64 * s).round() as usize).max(1),
        ((h as f64 * s).round() as usize).max(1),
    );
    let mut out = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        for x in 0..pw {
            let (x0, x1) = (x * w / pw, ((x + 1) * w / pw).max(x * w / pw + 1));
            let (y0, y1) = (y * h / ph, ((y + 1) * h / ph).max(y * h / ph + 1));
            let mut acc = [0u32; 4];
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = yy * w + xx;
                    for c in 0..3 {
                        acc[c] += rgb[i * 3 + c] as u32;
                    }
                    acc[3] += alpha.map_or(255, |a| a[i]) as u32;
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            for c in 0..4 {
                out[(y * pw + x) * 4 + c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
    let p = Params {
        quality: 45.0,
        preview: false,
        screen: Some(false),
        ..Params::default()
    };
    encode_rgba(&out, pw, ph, &p).bytes
}

/// Codifica una tesela independiente.
/// Devuelve (carga útil, reconstrucción RGB, reconstrucción alfa).
pub fn encode_tile(
    rgb: &[u8],
    alpha: Option<&[u8]>,
    width: usize,
    height: usize,
    header: &Header,
    p: &Params,
) -> (Vec<u8>, Vec<u8>, Option<Vec<u8>>) {
    let r = encode_tile_stats(rgb, alpha, width, height, header, p);
    (r.0, r.1, r.2)
}

/// Planos de color a codificar: YCbCr (con pérdida) o G, B, R (sin pérdida).
fn color_planes(rgb: &[u8], w: usize, h: usize, header: &Header) -> Vec<(Vec<f32>, usize, usize)> {
    if header.tools & TOOL_LOSSLESS != 0 {
        let ch = |c: usize| {
            rgb.chunks_exact(3)
                .map(|px| px[c] as f32)
                .collect::<Vec<f32>>()
        };
        vec![(ch(1), w, h), (ch(2), w, h), (ch(0), w, h)]
    } else {
        color::rgb_to_ycbcr(rgb, w, h, header.subsample).planes
    }
}

/// Codifica un plano completo (quadtree por superbloques). Devuelve la
/// reconstrucción y el original con relleno.
#[allow(clippy::too_many_arguments)]
fn encode_plane(
    st: &mut EncState,
    enc: &mut Encoder,
    src: &[f32],
    w: usize,
    h: usize,
    pc: usize,
    lref: Option<&LumaRef>,
    qm: &mut QMap,
    code_dq: bool,
) -> (Plane, Vec<f32>) {
    let (src, pw, ph) = pad_plane(src, w, h);
    let mut pe = PlaneEnc {
        src: &src,
        plane: Plane::new(pw, ph),
        pc,
        hash: st.screen.then(|| tools::HashIndex::build(&src, pw, ph)),
        sc: ScState::default(),
    };
    let mut wst = ScState::default();
    for sy in 0..ph / SB {
        for sx in 0..pw / SB {
            if code_dq {
                qm.code_sb(&mut st.ctx, enc, sx, sy);
            }
            let (_, node) = st.rdo_node(&mut pe, lref, qm, sx * SB, sy * SB, SB);
            st.write_node(enc, &pe.plane, pc, &mut wst, sx * SB, sy * SB, SB, &node);
        }
    }
    let plane = pe.plane;
    (plane, src)
}

/// Mapa de cuantización plano con paso 1 (alfa y modo sin pérdida).
fn unit_qmap(w: usize, h: usize) -> QMap {
    let (qw, qh) = (w.div_ceil(SB) * SB / AQ, h.div_ceil(SB) * SB / AQ);
    QMap {
        dq: vec![0; qw * qh],
        w: qw,
        h: qh,
        base: 1.0,
        cq: 1.0,
        sub: false,
    }
}

/// Como `encode_tile`, pero devuelve también el estado final de los modelos.
pub fn encode_tile_stats(
    rgb: &[u8],
    alpha: Option<&[u8]>,
    width: usize,
    height: usize,
    header: &Header,
    p: &Params,
) -> (Vec<u8>, Vec<u8>, Option<Vec<u8>>, Vec<(u32, u32)>) {
    let lossless = header.tools & TOOL_LOSSLESS != 0;
    let sub = header.subsample && !lossless;
    let base_q = header.qstep();
    let cq = header.chroma_q as f32 / 32.0;
    let mut st = EncState {
        ctx: if lossless {
            Ctx::new()
        } else {
            Ctx::trained(base_q, header.tools & TOOL_SCREEN != 0)
        },
        qt: QuantTables::new(header.qm_luma as f32 / 16.0, header.qm_chroma as f32 / 16.0),
        lambda_k: p.lambda_k,
        topk: (p.effort.clamp(1, 9) as usize) + 1,
        screen: header.tools & TOOL_SCREEN != 0,
        lossless,
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
    let mut srcs = Vec::new();
    for (pi, (src, w, h)) in color_planes(rgb, width, height, header).iter().enumerate() {
        let pc = (pi > 0) as usize;
        if pi == 0 {
            let (padded, pw, ph) = pad_plane(src, *w, *h);
            qm.dq = compute_aq(&padded, pw, ph, if header.aq { p.aq_strength } else { 0.0 });
            qm.w = pw / AQ;
            qm.h = ph / AQ;
        }
        let (plane, src) = encode_plane(
            &mut st,
            &mut enc,
            src,
            *w,
            *h,
            pc,
            lref.as_ref(),
            &mut qm,
            pi == 0 && header.aq,
        );
        if pi == 0 {
            lref = Some(LumaRef::new(&plane, sub));
        }
        planes.push(plane);
        srcs.push(src);
    }
    for (pi, plane) in planes.iter_mut().enumerate() {
        let pc = (pi > 0) as usize;
        let q = base_q * if pc == 1 { cq } else { 1.0 };
        let d = if p.restore && !lossless {
            restore::decide(plane, &srcs[pi], p.lambda_k * q * q)
        } else {
            restore::Decision::default()
        };
        let d = st
            .ctx
            .rest
            .code(&mut enc, pc, plane.w / SB, plane.h / SB, &d);
        restore::apply(plane, &d);
    }
    let stats = st.ctx.stats();
    // Canal alfa: plano adicional sin pérdida, con contextos propios.
    let mut recon_alpha = None;
    if header.tools & TOOL_ALPHA != 0 {
        let a: Vec<f32> = match alpha {
            Some(a) => a.iter().map(|&v| v as f32).collect(),
            None => vec![255.0; width * height],
        };
        let mut ast = EncState {
            ctx: Ctx::new(),
            qt: QuantTables::new(0.0, 0.0),
            lambda_k: p.lambda_k,
            topk: st.topk,
            screen: true,
            lossless: true,
        };
        let mut aq = unit_qmap(width, height);
        let (plane, _) = encode_plane(
            &mut ast, &mut enc, &a, width, height, 0, None, &mut aq, false,
        );
        recon_alpha = Some(crop_plane(&plane, width, height));
    }
    let recon = finish_planes(header, &planes, width, height);
    (enc.finish(), recon, recon_alpha, stats)
}

fn crop_plane(p: &Plane, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        out.extend_from_slice(&p.data[y * p.w..y * p.w + w]);
    }
    out
}

fn finish_planes(h: &Header, planes: &[Plane], w: usize, ht: usize) -> Vec<u8> {
    if h.tools & TOOL_LOSSLESS != 0 {
        let (g, b, r) = (&planes[0], &planes[1], &planes[2]);
        let mut out = Vec::with_capacity(w * ht * 3);
        for y in 0..ht {
            for x in 0..w {
                out.extend_from_slice(&[r.px(x, y), g.px(x, y), b.px(x, y)]);
            }
        }
        return out;
    }
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
    screen: bool,
    lossless: bool,
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
        st: &mut ScState,
        x0: usize,
        y0: usize,
        n: usize,
    ) {
        if n > 4 {
            let sctx = split_ctx(p, x0, y0, n, pc);
            if dec.bit(&mut self.ctx.split[sctx], false) {
                let h = n / 2;
                for (dx, dy) in CHILDREN {
                    self.read_node(dec, p, lref, qm, pc, st, x0 + dx * h, y0 + dy * h, h);
                }
                return;
            }
        }
        let dummy = Leaf {
            kind: Kind::Intra(MODE_DC),
            ts: false,
            levels: Vec::new(),
        };
        let (kind, ts) = code_leaf_head(
            &mut self.ctx,
            dec,
            p,
            pc,
            x0,
            y0,
            n,
            self.screen,
            self.lossless,
            &dummy,
            st,
        );
        let kind = match kind {
            Kind::Intra(m) if m == MODE_CCLM && lref.is_none() => Kind::Intra(MODE_DC),
            Kind::Intra(m) if m > MODE_CCLM => Kind::Intra(MODE_DC),
            k => k,
        };
        let mut levels = vec![0i32; n * n];
        let coef = if ts {
            &mut self.ctx.coef_ts
        } else {
            &mut self.ctx.coef
        };
        let nz = coef.code_block(dec, pc, n, n, cbf_ctx(p, x0, y0), &mut levels);
        let refs = predict::build_refs(p, x0, y0, n, n);
        let mut pred = vec![0i32; n * n];
        predict_leaf(p, &refs, lref, &kind, x0, y0, n, &mut pred);
        let mut recon = vec![0u8; n * n];
        let q = qm.q(pc, x0, y0, n);
        reconstruct(
            &levels,
            &pred,
            q,
            &self.qt.w[pc][size_index(n)],
            n,
            ts,
            &mut recon,
        );
        p.write_block(x0, y0, n, n, &recon);
        p.set_block(x0, y0, n, n, kind.mark(), nz);
    }
}

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    /// Canal alfa, si la imagen lo tiene.
    pub alpha: Option<Vec<u8>>,
}

impl Decoded {
    /// Píxeles RGBA (alfa 255 si la imagen no tiene transparencia).
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for (i, px) in self.rgb.chunks_exact(3).enumerate() {
            out.extend_from_slice(px);
            out.push(self.alpha.as_ref().map_or(255, |a| a[i]));
        }
        out
    }
}

/// Cabecera y tabla de teselas de un archivo.
pub struct Info {
    pub header: Header,
    pub tiles: Vec<TileRect>,
    /// Rango de bytes de la carga útil de cada tesela.
    pub ranges: Vec<std::ops::Range<usize>>,
    /// Rango de la vista previa (un .tsl completo), si la hay.
    pub preview: Option<std::ops::Range<usize>>,
}

pub fn info(data: &[u8]) -> Result<Info, String> {
    let header = Header::read(data)?;
    let tiles = tile_grid(
        header.width as usize,
        header.height as usize,
        header.tile_size(),
    );
    let mut base = HEADER_LEN;
    let mut preview = None;
    if header.tools & TOOL_PREVIEW != 0 {
        let len = data
            .get(base..base + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            .ok_or("archivo truncado")?;
        let end = (base + 4)
            .checked_add(len)
            .filter(|&e| e <= data.len())
            .ok_or("archivo truncado")?;
        preview = Some(base + 4..end);
        base = end;
    }
    let mut pos = base + 4 * tiles.len();
    if data.len() < pos {
        return Err("archivo truncado".into());
    }
    let mut ranges = Vec::with_capacity(tiles.len());
    for i in 0..tiles.len() {
        let o = base + 4 * i;
        let len = u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
        let end = pos
            .checked_add(len)
            .filter(|&e| e <= data.len())
            .ok_or("archivo truncado")?;
        ranges.push(pos..end);
        pos = end;
    }
    Ok(Info {
        header,
        tiles,
        ranges,
        preview,
    })
}

/// Decodifica solo la vista previa (si el archivo la tiene).
pub fn decode_preview(data: &[u8]) -> Result<Option<Decoded>, String> {
    let header = Header::read(data)?;
    if header.tools & TOOL_PREVIEW == 0 {
        return Ok(None);
    }
    let len = data
        .get(HEADER_LEN..HEADER_LEN + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or("archivo truncado")?;
    let pv = data
        .get(HEADER_LEN + 4..HEADER_LEN + 4 + len)
        .ok_or("archivo truncado")?;
    let inner = Header::read(pv)?;
    if inner.tools & TOOL_PREVIEW != 0 {
        return Err("vista previa no válida".into());
    }
    decode(pv).map(Some)
}

/// Decodifica un plano completo.
#[allow(clippy::too_many_arguments)]
fn decode_plane(
    st: &mut DecState,
    dec: &mut Decoder,
    w: usize,
    h: usize,
    pc: usize,
    lref: Option<&LumaRef>,
    qm: &mut QMap,
    code_dq: bool,
) -> Plane {
    let pw = w.div_ceil(SB) * SB;
    let ph = h.div_ceil(SB) * SB;
    let mut plane = Plane::new(pw, ph);
    let mut sst = ScState::default();
    for sy in 0..ph / SB {
        for sx in 0..pw / SB {
            if code_dq {
                qm.code_sb(&mut st.ctx, dec, sx, sy);
            }
            st.read_node(
                dec,
                &mut plane,
                lref,
                qm,
                pc,
                &mut sst,
                sx * SB,
                sy * SB,
                SB,
            );
        }
    }
    plane
}

/// Decodifica una tesela de `w × ht`. Devuelve (RGB, alfa).
pub fn decode_tile(payload: &[u8], w: usize, ht: usize, h: &Header) -> (Vec<u8>, Option<Vec<u8>>) {
    let lossless = h.tools & TOOL_LOSSLESS != 0;
    let sub = h.subsample && !lossless;
    let base_q = h.qstep();
    let cq = h.chroma_q as f32 / 32.0;
    let (cw, ch) = if sub {
        (w.div_ceil(2), ht.div_ceil(2))
    } else {
        (w, ht)
    };
    let dims = [(w, ht), (cw, ch), (cw, ch)];
    let mut st = DecState {
        ctx: if lossless {
            Ctx::new()
        } else {
            Ctx::trained(base_q, h.tools & TOOL_SCREEN != 0)
        },
        qt: QuantTables::new(h.qm_luma as f32 / 16.0, h.qm_chroma as f32 / 16.0),
        screen: h.tools & TOOL_SCREEN != 0,
        lossless,
    };
    let mut dec = Decoder::new(payload);
    let mut planes = Vec::new();
    let mut qm = QMap {
        dq: Vec::new(),
        w: 0,
        h: 0,
        base: base_q,
        cq,
        sub,
    };
    let mut lref: Option<LumaRef> = None;
    for (pi, &(pw0, ph0)) in dims.iter().enumerate() {
        if pi == 0 {
            qm.w = pw0.div_ceil(SB) * SB / AQ;
            qm.h = ph0.div_ceil(SB) * SB / AQ;
            qm.dq = vec![0; qm.w * qm.h];
        }
        let plane = decode_plane(
            &mut st,
            &mut dec,
            pw0,
            ph0,
            (pi > 0) as usize,
            lref.as_ref(),
            &mut qm,
            pi == 0 && h.aq,
        );
        if pi == 0 {
            lref = Some(LumaRef::new(&plane, sub));
        }
        planes.push(plane);
    }
    for (pi, plane) in planes.iter_mut().enumerate() {
        let d = st.ctx.rest.code(
            &mut dec,
            (pi > 0) as usize,
            plane.w / SB,
            plane.h / SB,
            &restore::Decision::default(),
        );
        restore::apply(plane, &d);
    }
    let alpha = (h.tools & TOOL_ALPHA != 0).then(|| {
        let mut ast = DecState {
            ctx: Ctx::new(),
            qt: QuantTables::new(0.0, 0.0),
            screen: true,
            lossless: true,
        };
        let mut aq = unit_qmap(w, ht);
        let plane = decode_plane(&mut ast, &mut dec, w, ht, 0, None, &mut aq, false);
        crop_plane(&plane, w, ht)
    });
    (finish_planes(h, &planes, w, ht), alpha)
}

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let inf = info(data)?;
    let (w, ht) = (inf.header.width as usize, inf.header.height as usize);
    let idx: Vec<usize> = (0..inf.tiles.len()).collect();
    let parts = par_map(&idx, |&i| {
        let t = inf.tiles[i];
        decode_tile(&data[inf.ranges[i].clone()], t.w, t.h, &inf.header)
    });
    let mut rgb = vec![0u8; w * ht * 3];
    let mut alpha = (inf.header.tools & TOOL_ALPHA != 0).then(|| vec![255u8; w * ht]);
    for (t, (prgb, palpha)) in inf.tiles.iter().zip(&parts) {
        paste(&mut rgb, 3, w, t, prgb);
        if let (Some(dst), Some(src)) = (alpha.as_mut(), palpha.as_ref()) {
            paste(dst, 1, w, t, src);
        }
    }
    Ok(Decoded {
        width: w,
        height: ht,
        rgb,
        alpha,
    })
}
