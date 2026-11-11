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
/// Firma del archivo: "TSL" seguida del byte de versión del flujo de bits.
pub const MAGIC: &[u8; 3] = b"TSL";
/// Versión del flujo de bits. La v1 está congelada: cualquier cambio que
/// altere la decodificación de un archivo v1 debe ir en una versión nueva
/// (los vectores de `tests/vectors` lo comprueban).
pub const VERSION: u8 = 2;
/// Versión más antigua que este decodificador sabe leer.
pub const MIN_VERSION: u8 = 1;
/// Bits de `tools` definidos en la v1.
const KNOWN_TOOLS: u8 = 1 | 2 | 4 | 8;
/// Bits añadidos en la v2.
const KNOWN_TOOLS_V2: u8 = KNOWN_TOOLS | TOOL_META | TOOL_ANIM | TOOL_INTER;
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
/// v2: sección de metadatos (perfil de color ICC, EXIF) tras la cabecera.
pub const TOOL_META: u8 = 32;
/// v2: animación (lista de fotogramas en lugar de tabla de teselas).
pub const TOOL_ANIM: u8 = 64;
/// v2, en el archivo de un fotograma: predicción desde el fotograma anterior.
pub const TOOL_INTER: u8 = 128;
/// Marca en el mapa de modos de las hojas predichas desde el fotograma anterior.
const MARK_INTER: u8 = 202;
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
    /// Versión del flujo de bits a escribir (1 o 2; la 2 añade particiones rectangulares).
    pub version: u8,
    /// Hilos para el RDO dentro de cada tesela (0 = automático). El
    /// resultado es idéntico con cualquier número de hilos.
    pub threads: usize,
    /// Incluir vista previa progresiva.
    pub preview: bool,
    /// Metadatos que se guardan en el archivo (solo en la v2).
    pub meta: Metadata,
}

/// Metadatos de la imagen: se guardan tal cual, sin interpretarlos.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    /// Perfil de color ICC (por ejemplo, Display P3 en las fotos de móvil).
    pub icc: Option<Vec<u8>>,
    /// Bloque EXIF (formato TIFF, como en el segmento APP1 de JPEG sin «Exif\0\0»).
    pub exif: Option<Vec<u8>>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none()
    }

    /// Sección de metadatos: `u32` de longitud y fragmentos `etiqueta (4 B) | u32 | datos`.
    fn write(&self, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        for (tag, data) in [(b"ICCP", &self.icc), (b"EXIF", &self.exif)] {
            if let Some(d) = data {
                body.extend_from_slice(tag);
                body.extend_from_slice(&(d.len() as u32).to_le_bytes());
                body.extend_from_slice(d);
            }
        }
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
    }

    /// Lee la sección que empieza en `pos`. Devuelve los metadatos y dónde acaba.
    /// Las etiquetas desconocidas se ignoran (compatibilidad hacia delante).
    fn read(data: &[u8], pos: usize) -> Result<(Metadata, usize), String> {
        let len = data
            .get(pos..pos + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            .ok_or("archivo truncado")?;
        let end = (pos + 4)
            .checked_add(len)
            .filter(|&e| e <= data.len())
            .ok_or("archivo truncado")?;
        let mut m = Metadata::default();
        let mut p = pos + 4;
        while p < end {
            let tag = data.get(p..p + 4).ok_or("metadatos no válidos")?;
            let n = data
                .get(p + 4..p + 8)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
                .ok_or("metadatos no válidos")?;
            let q = (p + 8)
                .checked_add(n)
                .filter(|&e| e <= end)
                .ok_or("metadatos no válidos")?;
            let d = data[p + 8..q].to_vec();
            match tag {
                b"ICCP" => m.icc = Some(d),
                b"EXIF" => m.exif = Some(d),
                _ => {}
            }
            p = q;
        }
        Ok((m, end))
    }
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
            version: VERSION,
            threads: 0,
            meta: Metadata::default(),
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
    /// Versión del flujo de bits (1 o 2).
    pub version: u8,
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
        out.push(self.version);
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
        if d.len() < 4 || &d[..3] != MAGIC {
            return Err("no es un archivo Tesela".into());
        }
        match d[3] {
            MIN_VERSION..=VERSION => {}
            b'1' => {
                return Err(
                    "archivo de una versión de desarrollo anterior a la v1: vuelve a codificarlo"
                        .into(),
                );
            }
            v => {
                return Err(format!(
                    "versión {v} del formato no soportada (este decodificador lee de la v{MIN_VERSION} a la v{VERSION})"
                ));
            }
        }
        if d.len() < HEADER_LEN {
            return Err("archivo truncado".into());
        }
        let u32le = |i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap());
        let h = Header {
            version: d[3],
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
        if d[12] > 1 || d[18] > 1 || h.deblock != 0 || h.qstep == 0 || h.chroma_q == 0 {
            return Err("cabecera no válida para la v1".into());
        }
        let known = if h.version >= 2 {
            KNOWN_TOOLS_V2
        } else {
            KNOWN_TOOLS
        };
        if h.tools & !known != 0 {
            return Err(format!(
                "herramientas desconocidas (0x{:02x}): archivo de una versión posterior",
                h.tools
            ));
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

#[derive(Clone)]
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
    rect: Vec<Model>,
    rect_dir: Vec<Model>,
    /// v2, fotogramas con referencia: ¿hoja predicha del fotograma anterior? y su vector.
    inter: Vec<Model>,
    mv: BvCtx,
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
        // v2 (al final para que las tablas de la v1 sigan aplicándose igual)
        self.rect.visit(f);
        self.rect_dir.visit(f);
        self.inter.visit(f);
        self.mv.visit(f);
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
    fn trained(qstep: f32, screen: bool, version: u8) -> Self {
        let mut c = Ctx::new();
        let (s, b) = (screen as usize, quality_band(qstep));
        let t2 = crate::init_probs_v2::TABLES[s][b];
        let t = if version >= 2 && !t2.is_empty() {
            t2
        } else {
            crate::init_probs::TABLES[s][b]
        };
        if UNTRAINED.load(std::sync::atomic::Ordering::Relaxed) {
            return c;
        }
        let mut n = 0;
        c.visit(&mut |_| n += 1);
        // Una tabla más corta que el nº de modelos se aplica a los primeros
        // (los modelos añadidos en versiones posteriores van al final).
        if !t.is_empty() && t.len() <= n {
            let mut i = 0;
            c.visit(&mut |m| {
                if i < t.len() {
                    m.set_init(t[i]);
                }
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
            rect: vec![Model::new(); 2 * 4],
            rect_dir: vec![Model::new(); 2 * 4],
            inter: vec![Model::new(); 2 * 5 * 3],
            mv: BvCtx::default(),
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

/// Pesos de la matriz de cuantización por tamaño de bloque `w × h`.
struct QuantTables {
    w: [Vec<Vec<f32>>; 2],
}

impl QuantTables {
    fn new(qm_luma: f32, qm_chroma: f32) -> Self {
        let make = |s: f32| {
            let mut all = Vec::with_capacity(25);
            for &bh in &dct::SIZES {
                for &bw in &dct::SIZES {
                    let mut v = vec![0f32; bw * bh];
                    for y in 0..bh {
                        for x in 0..bw {
                            v[y * bw + x] = if bw == bh {
                                // Fórmula de la v1 (cuadrados), sin cambios.
                                let r = ((x * x + y * y) as f32).sqrt() / bw as f32;
                                1.0 + s * r * r
                            } else {
                                let (rx, ry) = (x as f32 / bw as f32, y as f32 / bh as f32);
                                1.0 + s * (rx * rx + ry * ry)
                            };
                        }
                    }
                    all.push(v);
                }
            }
            all
        };
        QuantTables {
            w: [make(qm_luma), make(qm_chroma)],
        }
    }

    fn get(&self, pc: usize, w: usize, h: usize) -> &[f32] {
        &self.w[pc][size_index(h) * 5 + size_index(w)]
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
    fn q(&self, pc: usize, x0: usize, y0: usize, bw: usize, bh: usize) -> f32 {
        let f = if pc == 1 && self.sub { 2 } else { 1 };
        let (cx0, cy0) = ((x0 * f / AQ).min(self.w - 1), (y0 * f / AQ).min(self.h - 1));
        let cx1 = ((x0 + bw) * f).div_ceil(AQ).clamp(cx0 + 1, self.w);
        let cy1 = ((y0 + bh) * f).div_ceil(AQ).clamp(cy0 + 1, self.h);
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
    /// Copia del fotograma anterior desplazada por el vector (píxeles enteros).
    Inter((i32, i32)),
}

impl Kind {
    /// Valor del mapa de modos (para el contexto de los vecinos).
    fn mark(&self) -> u8 {
        match self {
            Kind::Intra(m) => *m,
            Kind::Palette(_) => MARK_PALETTE,
            Kind::Ibc(_) => MARK_IBC,
            Kind::Inter(_) => MARK_INTER,
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
    /// Partición rectangular (v2): dos mitades, arriba/abajo si `true`
    /// (horizontal) o izquierda/derecha si `false` (vertical).
    Rect(bool, Box<[Leaf; 2]>),
}

const CHILDREN: [(usize, usize); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];

/// (x, y, ancho, alto) de las dos mitades de un nodo rectangular.
fn rect_halves(x0: usize, y0: usize, n: usize, horz: bool) -> [(usize, usize, usize, usize); 2] {
    let h = n / 2;
    if horz {
        [(x0, y0, n, h), (x0, y0 + h, n, h)]
    } else {
        [(x0, y0, h, n), (x0 + h, y0, h, n)]
    }
}

/// Sintaxis de la partición rectangular (v2), tras `split = 0` en nodos N ≥ 8.
/// Devuelve None (sin partición) o Some(horizontal).
fn code_rect<C: Coder>(
    ctx: &mut Ctx,
    c: &mut C,
    pc: usize,
    n: usize,
    rect: Option<bool>,
) -> Option<bool> {
    let i = pc * 4 + size_index(n) - 1;
    if !c.bit(&mut ctx.rect[i], rect.is_some()) {
        return None;
    }
    Some(c.bit(&mut ctx.rect_dir[i], rect == Some(true)))
}

/// Estado de las herramientas sintéticas compartido por codificador y
/// decodificador dentro de un plano (última paleta y último vector).
#[derive(Clone, Default)]
struct ScState {
    last_pal: Option<Vec<u8>>,
    last_bv: (i32, i32),
    last_mv: (i32, i32),
}

fn inter_ctx(p: &Plane, x0: usize, y0: usize) -> usize {
    let it = |v: Option<(u8, u8, bool)>| v.is_some_and(|(m, _, _)| m == MARK_INTER) as usize;
    it(p.neighbor(x0, y0, true)) + it(p.neighbor(x0, y0, false))
}

fn special_ctx(p: &Plane, x0: usize, y0: usize) -> usize {
    let sp = |v: Option<(u8, u8, bool)>| v.is_some_and(|(m, _, _)| m >= MARK_PALETTE) as usize;
    sp(p.neighbor(x0, y0, true)) + sp(p.neighbor(x0, y0, false))
}

/// Sintaxis de la cabecera de una hoja `w × h` (clase de predicción y
/// transform skip). Simétrica: el decodificador pasa una hoja ficticia.
/// Los contextos usan el lado mayor del bloque.
#[allow(clippy::too_many_arguments)]
fn code_leaf_head<C: Coder>(
    ctx: &mut Ctx,
    c: &mut C,
    p: &Plane,
    pc: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    screen: bool,
    lossless: bool,
    leaf: &Leaf,
    st: &mut ScState,
    inter: bool,
) -> (Kind, bool) {
    let n = w.max(h);
    let sz = size_index(n);
    let mut kind = None;
    if inter
        && c.bit(
            &mut ctx.inter[(pc * 5 + sz) * 3 + inter_ctx(p, x0, y0)],
            matches!(leaf.kind, Kind::Inter(_)),
        )
    {
        let mv = match leaf.kind {
            Kind::Inter(m) => m,
            _ => (0, 0),
        };
        kind = Some(Kind::Inter(ctx.mv.code(c, pc, mv, &mut st.last_mv)));
    }
    if screen && kind.is_none() {
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
                Kind::Palette(ctx.pal.code(c, pc, w, h, pal, &mut st.last_pal))
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

/// Reconstruye un bloque `bw × bh`: descuantiza, transformada inversa (salvo
/// transform skip) y suma la predicción.
#[allow(clippy::too_many_arguments)]
fn reconstruct(
    levels: &[i32],
    pred: &[i32],
    q: f32,
    wt: &[f32],
    bw: usize,
    bh: usize,
    ts: bool,
    out: &mut [u8],
) {
    let nn = bw * bh;
    let mut buf = vec![0f32; nn];
    let mut any = false;
    for i in 0..nn {
        if levels[i] != 0 {
            buf[i] = levels[i] as f32 * q * if ts { 1.0 } else { wt[i] };
            any = true;
        }
    }
    if any && !ts {
        if levels[1..nn].iter().all(|&l| l == 0) {
            // Solo DC: la inversa es constante (base DC ortonormal = 1/√(w·h)).
            let v = if bw == bh {
                buf[0] / bw as f32
            } else {
                buf[0] / (nn as f32).sqrt()
            };
            buf.fill(v);
        } else {
            dct::transform_rect(&mut buf, bw, bh, true);
        }
    }
    for i in 0..nn {
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

#[allow(clippy::too_many_arguments)]
fn predict_mode(
    p: &Plane,
    refs: &predict::Refs,
    lref: Option<&LumaRef>,
    mode: u8,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
    if mode == MODE_CCLM {
        let l = lref.expect("CCLM solo en croma");
        predict::predict_cclm(p, &l.data, l.w, l.h, x0, y0, w, h, out);
    } else {
        predict::predict(refs, mode, w, h, out);
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
    w: usize,
    h: usize,
    ip: Option<&InterPlane>,
    out: &mut [i32],
) {
    match kind {
        Kind::Inter(mv) => match ip {
            Some(ip) => ip.predict(x0, y0, w, h, *mv, out),
            None => out[..w * h].fill(128),
        },
        Kind::Intra(m) => predict_mode(p, refs, lref, *m, x0, y0, w, h, out),
        Kind::Palette(pal) => tools::predict_palette(pal, out),
        Kind::Ibc(bv) => {
            if tools::bv_valid(p, x0, y0, w, h, *bv) {
                tools::predict_ibc(p, x0, y0, w, h, *bv, out)
            } else {
                out[..w * h].fill(128)
            }
        }
    }
}

/// Plano del fotograma anterior (lienzo completo) para la predicción entre
/// fotogramas, con el origen de la tesela actual dentro de él.
#[derive(Clone, Copy)]
struct InterPlane<'a> {
    data: &'a [u8],
    w: usize,
    h: usize,
    ox: usize,
    oy: usize,
    /// Solo en el codificador: hash del fotograma anterior original.
    hash: Option<&'a tools::HashIndex>,
}

impl InterPlane<'_> {
    /// Copia desplazada; fuera del lienzo se repite el borde.
    fn predict(&self, x0: usize, y0: usize, w: usize, h: usize, mv: (i32, i32), out: &mut [i32]) {
        let bx = (self.ox + x0) as i64 + mv.0 as i64;
        let by = (self.oy + y0) as i64 + mv.1 as i64;
        let (mw, mh) = (self.w as i64 - 1, self.h as i64 - 1);
        for y in 0..h {
            let ry = (by + y as i64).clamp(0, mh) as usize;
            let row = &self.data[ry * self.w..(ry + 1) * self.w];
            for x in 0..w {
                out[y * w + x] = row[(bx + x as i64).clamp(0, mw) as usize] as i32;
            }
        }
    }
}

/// Fotograma anterior (lienzo completo) en el espacio de color del fotograma
/// actual, y dónde empieza el fotograma dentro del lienzo (coordenadas pares).
pub struct RefCanvas {
    planes: Vec<(Vec<u8>, usize, usize)>,
    hashes: Vec<Option<tools::HashIndex>>,
    x: usize,
    y: usize,
    sub: bool,
}

impl RefCanvas {
    /// `recon`: lienzo anterior decodificado (RGB). `src`: lienzo anterior
    /// original (solo el codificador, para buscar coincidencias exactas).
    fn new(recon: &[u8], src: Option<&[u8]>, w: usize, h: usize, header: &Header, x: usize, y: usize) -> Self {
        let to_u8 = |p: &[f32]| p.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect::<Vec<u8>>();
        let planes = color_planes(recon, w, h, header)
            .into_iter()
            .map(|(p, pw, ph)| (to_u8(&p), pw, ph))
            .collect();
        let hashes = match src {
            Some(s) => color_planes(s, w, h, header)
                .into_iter()
                .map(|(p, pw, ph)| Some(tools::HashIndex::build(&p, pw, ph)))
                .collect(),
            None => vec![None, None, None],
        };
        RefCanvas { planes, hashes, x, y, sub: header.subsample && header.tools & TOOL_LOSSLESS == 0 }
    }

    /// Plano `pi` para la tesela que empieza en (tx, ty) dentro del fotograma.
    fn plane(&self, pi: usize, tx: usize, ty: usize) -> InterPlane<'_> {
        let d = if pi > 0 && self.sub { 2 } else { 1 };
        let (data, w, h) = &self.planes[pi];
        InterPlane { data, w: *w, h: *h, ox: (self.x + tx) / d, oy: (self.y + ty) / d, hash: self.hashes[pi].as_ref() }
    }
}

// ------------------------------------------------------------- Codificador

struct PlaneEnc<'a> {
    src: &'a [f32],
    plane: Plane,
    pc: usize,
    /// Fotograma anterior, si este fotograma predice de él.
    inter: Option<InterPlane<'a>>,
    /// Índice hash para la búsqueda de IBC (solo con herramientas sintéticas).
    hash: Option<&'a tools::HashIndex>,
    /// Estado sintético aproximado durante el RDO.
    sc: ScState,
}

struct EncState {
    ctx: Ctx,
    qt: std::sync::Arc<QuantTables>,
    lambda_k: f32,
    topk: usize,
    screen: bool,
    lossless: bool,
    /// Flujo de bits v2 (particiones rectangulares).
    v2: bool,
    /// Esfuerzo (1..9): los niveles bajos recortan la búsqueda para ir más rápido.
    effort: u8,
    /// Fotograma con predicción desde el anterior (cada hoja lleva el bit inter).
    inter: bool,
}

/// Resultado de evaluar una hoja.
struct LeafEval {
    j: f32,
    leaf: Leaf,
    recon: Vec<u8>,
    nz: bool,
}

impl EncState {
    /// Copia de la configuración (sin contextos).
    fn shallow(&self) -> EncState {
        EncState {
            ctx: Ctx::new(),
            qt: self.qt.clone(),
            lambda_k: self.lambda_k,
            topk: self.topk,
            screen: self.screen,
            lossless: self.lossless,
            v2: self.v2,
            effort: self.effort,
            inter: self.inter,
        }
    }

    /// Coste en bits de la cabecera de una hoja con el estado actual.
    #[allow(clippy::too_many_arguments)]
    fn head_bits(
        &mut self,
        pe: &PlaneEnc,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
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
            w,
            h,
            self.screen,
            self.lossless,
            &leaf,
            &mut st,
            self.inter,
        );
        cc.bits
    }

    /// Mejor codificación del residuo de una predicción (DCT y, si procede,
    /// transform skip). Devuelve (J sin cabecera, niveles, ts).
    #[allow(clippy::too_many_arguments)]
    fn rd_residual(
        &self,
        pc: usize,
        w: usize,
        h: usize,
        cctx: usize,
        src: &[f32],
        pred: &[i32],
        q: f32,
        lambda: f32,
        try_ts: bool,
        allow_dct: bool,
    ) -> (f32, Vec<i32>, bool) {
        let nn = w * h;
        let mut res = vec![0f32; nn];
        let mut best = (f32::INFINITY, Vec::new(), false);
        let mut levels = vec![0i32; nn];
        if self.lossless {
            // Residuo exacto, sin cuantizar: solo cuenta la tasa.
            for i in 0..nn {
                levels[i] = (src[i] - pred[i] as f32).round() as i32;
            }
            let bits = self.ctx.coef_ts.block_cost(pc, w, h, cctx, &levels);
            return (lambda * bits, levels, true);
        }
        if allow_dct {
            for i in 0..nn {
                res[i] = src[i] - pred[i] as f32;
            }
            dct::transform_rect(&mut res, w, h, false);
            let qs: Vec<f32> = self.qt.get(pc, w, h).iter().map(|&wi| wi * q).collect();
            let (d, bits) =
                self.ctx
                    .coef
                    .quantize_rdo(pc, w, h, cctx, &res, &qs, lambda, &mut levels);
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
                    .quantize_rdo(pc, w, h, cctx, &res, &qs, lambda, &mut levels);
            let j = d + lambda * bits;
            if j < best.0 {
                best = (j, levels.clone(), true);
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_leaf(
        &mut self,
        pe: &mut PlaneEnc,
        lref: Option<&LumaRef>,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        q: f32,
    ) -> LeafEval {
        let lambda = self.lambda_k * q * q;
        let slam = lambda.sqrt();
        let pc = pe.pc;
        let n = w.max(h);
        let nn = w * h;
        let mut src = vec![0f32; nn];
        for y in 0..h {
            for x in 0..w {
                src[y * w + x] = pe.src[(y0 + y) * pe.plane.w + x0 + x];
            }
        }
        let refs = predict::build_refs(&pe.plane, x0, y0, w, h);
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
        let passes = if self.effort >= 3 { 2 } else { 1 };
        for pass in 0..passes {
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
                predict_mode(&pe.plane, &refs, lref, mode, x0, y0, w, h, &mut pred);
                for i in 0..nn {
                    res[i] = src[i] - pred[i] as f32;
                }
                let mut cc = CostCounter::default();
                code_mode(&mut self.ctx, &mut cc, pc, n, mpm, mode);
                let s = dct::satd_rect(&res, w, h);
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
            predict_mode(&pe.plane, &refs, lref, mode, x0, y0, w, h, &mut pred);
            let (jr, lv, ts) =
                self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true);
            let kind = Kind::Intra(mode);
            let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
            consider(&mut best, jr + lambda * hb, kind, lv, ts);
        }
        if self.screen && n <= 32 {
            // Paleta
            for pal in tools::palette_candidates(&src, w, h) {
                tools::predict_palette(&pal, &mut pred);
                let (jr, lv, ts) =
                    self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true);
                let kind = Kind::Palette(pal);
                let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                consider(&mut best, jr + lambda * hb, kind, lv, ts);
            }
            // Copia intra de bloques
            if w.min(h) >= 8 {
                let mut bvs: Vec<(i32, i32)> =
                    vec![pe.sc.last_bv, (-(w as i32), 0), (0, -(h as i32))];
                if let Some(hi) = &pe.hash {
                    bvs.extend(hi.candidates(pe.src, pe.plane.w, x0, y0));
                }
                let mut scored: Vec<(f32, (i32, i32))> = Vec::new();
                for bv in bvs {
                    if scored.iter().any(|s| s.1 == bv)
                        || !tools::bv_valid(&pe.plane, x0, y0, w, h, bv)
                    {
                        continue;
                    }
                    tools::predict_ibc(&pe.plane, x0, y0, w, h, bv, &mut pred);
                    let sad: f32 = (0..nn).map(|i| (src[i] - pred[i] as f32).abs()).sum();
                    let bits = self.ctx.bv.cost(pc, bv, pe.sc.last_bv);
                    scored.push((sad + slam * bits, bv));
                }
                scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                for &(_, bv) in scored.iter().take(2) {
                    tools::predict_ibc(&pe.plane, x0, y0, w, h, bv, &mut pred);
                    let (jr, lv, ts) =
                        self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true);
                    let kind = Kind::Ibc(bv);
                    let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                    consider(&mut best, jr + lambda * hb, kind, lv, ts);
                }
            }
        }
        if let Some(ip) = pe.inter {
            // Predicción desde el fotograma anterior: vectores candidatos (el
            // nulo, el último, coincidencias exactas del hash y una búsqueda
            // por pasos alrededor del mejor), ordenados por SAD + bits.
            let mut seen: Vec<(i32, i32)> = Vec::new();
            let mut scored: Vec<(f32, (i32, i32))> = Vec::new();
            let mut score = |mv: (i32, i32), this: &mut Self, scored: &mut Vec<(f32, (i32, i32))>, pred: &mut [i32]| {
                if seen.contains(&mv) || mv.0.abs() > 4096 || mv.1.abs() > 4096 {
                    return f32::INFINITY;
                }
                seen.push(mv);
                ip.predict(x0, y0, w, h, mv, pred);
                let sad: f32 = (0..nn).map(|i| (src[i] - pred[i] as f32).abs()).sum();
                let s = sad + slam * this.ctx.mv.cost(pc, mv, pe.sc.last_mv);
                scored.push((s, mv));
                s
            };
            let mut start = vec![(0, 0), pe.sc.last_mv];
            if let Some(hi) = ip.hash.filter(|_| w.min(h) >= 8) {
                for c in hi.candidates(pe.src, pe.plane.w, x0, y0).into_iter().take(8) {
                    start.push((c.0 - ip.ox as i32, c.1 - ip.oy as i32));
                }
            }
            let mut best_mv = (f32::INFINITY, (0, 0));
            for mv in start {
                let s = score(mv, self, &mut scored, &mut pred);
                if s < best_mv.0 {
                    best_mv = (s, mv);
                }
            }
            if self.effort >= 2 && w.min(h) >= 8 {
                for step in [8, 4, 2, 1] {
                    let mut improved = true;
                    while improved {
                        improved = false;
                        let c = best_mv.1;
                        for (dx, dy) in [(step, 0), (-step, 0), (0, step), (0, -step), (step, step), (-step, -step), (step, -step), (-step, step)] {
                            let mv = (c.0 + dx, c.1 + dy);
                            let s = score(mv, self, &mut scored, &mut pred);
                            if s < best_mv.0 {
                                best_mv = (s, mv);
                                improved = true;
                            }
                        }
                    }
                }
            }
            scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for &(_, mv) in scored.iter().take(2) {
                ip.predict(x0, y0, w, h, mv, &mut pred);
                let (jr, lv, ts) = self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true);
                let kind = Kind::Inter(mv);
                let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                consider(&mut best, jr + lambda * hb, kind, lv, ts);
            }
        }
        let (j, kind, levels, ts) = best.expect("al menos un candidato");
        predict_leaf(&pe.plane, &refs, lref, &kind, x0, y0, w, h, pe.inter.as_ref(), &mut pred);
        let mut recon = vec![0u8; nn];
        reconstruct(
            &levels,
            &pred,
            q,
            self.qt.get(pc, w, h),
            w,
            h,
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
    fn commit_leaf(pe: &mut PlaneEnc, x0: usize, y0: usize, w: usize, h: usize, ev: &LeafEval) {
        pe.plane.write_block(x0, y0, w, h, &ev.recon);
        pe.plane.set_block(x0, y0, w, h, ev.leaf.kind.mark(), ev.nz);
        match &ev.leaf.kind {
            Kind::Palette(p) => pe.sc.last_pal = Some(p.colors.clone()),
            Kind::Ibc(bv) => pe.sc.last_bv = *bv,
            Kind::Inter(mv) => pe.sc.last_mv = *mv,
            Kind::Intra(_) => {}
        }
    }

    /// Decide la partición de un nodo N×N por RDO: hoja, dos mitades
    /// (horizontal o vertical, solo v2) o cuatro hijos.
    fn rdo_node(
        &mut self,
        pe: &mut PlaneEnc,
        lref: Option<&LumaRef>,
        qm: &QMap,
        x0: usize,
        y0: usize,
        n: usize,
    ) -> (f32, Node) {
        let pc = pe.pc;
        let q = qm.q(pc, x0, y0, n, n);
        let lambda = self.lambda_k * q * q;
        let sc_before = pe.sc.clone();
        let mut ev = self.eval_leaf(pe, lref, x0, y0, n, n, q);
        // Modos rápidos: sin bloques de 4×4 (esfuerzo 1) y sin probar a
        // dividir un bloque pequeño que ya se predice bien sin residuo.
        let min_n = if self.effort <= 1 { 8 } else { 4 };
        let early = self.effort <= 2 && !ev.nz && n <= 16;
        if n <= min_n || early {
            if n > 4 {
                let sctx = split_ctx(&pe.plane, x0, y0, n, pc);
                ev.j += lambda * self.ctx.split[sctx].cost(false);
                if self.v2 {
                    ev.j += lambda * self.ctx.rect[pc * 4 + size_index(n) - 1].cost(false);
                }
            }
            Self::commit_leaf(pe, x0, y0, n, n, &ev);
            let j = ev.j;
            return (j, Node::Leaf(Box::new(ev.leaf)));
        }
        let sctx = split_ctx(&pe.plane, x0, y0, n, pc);
        let split0 = lambda * self.ctx.split[sctx].cost(false);
        let split1 = lambda * self.ctx.split[sctx].cost(true);
        let ri = pc * 4 + size_index(n) - 1;
        ev.j += split0;
        if self.v2 {
            ev.j += lambda * self.ctx.rect[ri].cost(false);
        }
        // 0 = hoja, 1 = mitades horizontales, 2 = mitades verticales.
        let mut best_j = ev.j;
        let mut best = 0;
        let mut rect_best: Option<[LeafEval; 2]> = None;
        // Las mitades de 64×64 casi nunca ganan y son las más caras de evaluar.
        if self.v2 && n < 64 && self.effort >= 4 {
            for (opt, horz) in [(1, true), (2, false)] {
                pe.plane.clear_avail(x0, y0, n, n);
                pe.sc = sc_before.clone();
                let mut j = split0
                    + lambda * (self.ctx.rect[ri].cost(true) + self.ctx.rect_dir[ri].cost(horz));
                let mut evs: Vec<LeafEval> = Vec::with_capacity(2);
                for (hx, hy, hw, hh) in rect_halves(x0, y0, n, horz) {
                    if j >= best_j {
                        break; // poda exacta
                    }
                    let e = self.eval_leaf(pe, lref, hx, hy, hw, hh, qm.q(pc, hx, hy, hw, hh));
                    j += e.j;
                    Self::commit_leaf(pe, hx, hy, hw, hh, &e);
                    evs.push(e);
                }
                if evs.len() == 2 && j < best_j {
                    best_j = j;
                    best = opt;
                    let b = evs.pop().unwrap();
                    let a = evs.pop().unwrap();
                    rect_best = Some([a, b]);
                }
            }
            pe.plane.clear_avail(x0, y0, n, n);
            pe.sc = sc_before.clone();
        }
        let h = n / 2;
        let mut jb = split1;
        let mut kids: Vec<Node> = Vec::with_capacity(4);
        for (dx, dy) in CHILDREN {
            let (j, node) = self.rdo_node(pe, lref, qm, x0 + dx * h, y0 + dy * h, h);
            jb += j;
            kids.push(node);
            // Poda exacta: los costes son no negativos, así que si la suma
            // parcial ya supera a la mejor alternativa, dividir no puede ganar.
            if jb >= best_j {
                break;
            }
        }
        if kids.len() == 4 && jb < best_j {
            let arr: [Node; 4] = kids.try_into().ok().unwrap();
            return (jb, Node::Split(Box::new(arr)));
        }
        pe.sc = sc_before;
        if best == 0 {
            Self::commit_leaf(pe, x0, y0, n, n, &ev);
            let j = ev.j;
            return (j, Node::Leaf(Box::new(ev.leaf)));
        }
        let horz = best == 1;
        let [a, b] = rect_best.expect("mitades evaluadas");
        let halves = rect_halves(x0, y0, n, horz);
        Self::commit_leaf(pe, halves[0].0, halves[0].1, halves[0].2, halves[0].3, &a);
        Self::commit_leaf(pe, halves[1].0, halves[1].1, halves[1].2, halves[1].3, &b);
        (best_j, Node::Rect(horz, Box::new([a.leaf, b.leaf])))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_leaf(
        &mut self,
        enc: &mut Encoder,
        p: &Plane,
        pc: usize,
        st: &mut ScState,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        leaf: &Leaf,
    ) {
        code_leaf_head(
            &mut self.ctx,
            enc,
            p,
            pc,
            x0,
            y0,
            w,
            h,
            self.screen,
            self.lossless,
            leaf,
            st,
            self.inter,
        );
        let mut l = leaf.levels.clone();
        let coef = if leaf.ts {
            &mut self.ctx.coef_ts
        } else {
            &mut self.ctx.coef
        };
        coef.code_block(enc, pc, w, h, cbf_ctx(p, x0, y0), &mut l);
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
            let split = matches!(node, Node::Split(_));
            enc.bit(&mut self.ctx.split[sctx], split);
            if !split && self.v2 {
                let r = match node {
                    Node::Rect(horz, _) => Some(*horz),
                    _ => None,
                };
                code_rect(&mut self.ctx, enc, pc, n, r);
            }
        }
        match node {
            Node::Split(kids) => {
                let h = n / 2;
                for (k, (dx, dy)) in CHILDREN.iter().enumerate() {
                    self.write_node(enc, p, pc, st, x0 + dx * h, y0 + dy * h, h, &kids[k]);
                }
            }
            Node::Leaf(leaf) => self.write_leaf(enc, p, pc, st, x0, y0, n, n, leaf),
            Node::Rect(horz, leaves) => {
                for (k, (hx, hy, hw, hh)) in rect_halves(x0, y0, n, *horz).into_iter().enumerate() {
                    self.write_leaf(enc, p, pc, st, hx, hy, hw, hh, &leaves[k]);
                }
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

static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Fija los hilos que usa el codificador cuando `Params::threads` es 0
/// (0 = los que diga el sistema). Sirve en WebAssembly, donde el sistema no
/// lo sabe y lo indica la página (`navigator.hardwareConcurrency`).
pub fn set_default_threads(n: usize) {
    THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
}

fn available_threads() -> usize {
    let n = THREADS.load(std::sync::atomic::Ordering::Relaxed);
    if n > 0 {
        n
    } else if cfg!(feature = "parallel") {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        1
    }
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
            version: p.version,
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
        version: p.version,
        width: width as u32,
        height: height as u32,
        subsample: p.subsample.unwrap_or(!screen && base_q > 1.04),
        qstep: (base_q * 64.0).round().clamp(1.0, 65535.0) as u16,
        chroma_q: (p.chroma_q * 32.0).round().clamp(1.0, 255.0) as u8,
        qm_luma: (p.qm_luma * 16.0).round().clamp(0.0, 255.0) as u8,
        qm_chroma: (p.qm_chroma * 16.0).round().clamp(0.0, 255.0) as u8,
        aq: p.aq_strength != 0.0,
        // El desbloqueo no forma parte de la v1 (campo reservado, siempre 0).
        deblock: 0,
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
    assemble_full(header, &Metadata::default(), preview, payloads)
}

/// Archivo completo: cabecera, metadatos (v2), vista previa y teselas.
pub fn assemble_full(
    header: &Header,
    meta: &Metadata,
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
    let with_meta = h.version >= 2 && !meta.is_empty();
    if with_meta {
        h.tools |= TOOL_META;
    } else {
        h.tools &= !TOOL_META;
    }
    h.write(&mut bytes);
    if with_meta {
        meta.write(&mut bytes);
    }
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
    encode_with_alpha_ref(rgb, alpha, width, height, p, None)
}

/// Fotograma anterior para predecir de él: lienzo reconstruido y original
/// (RGB), su tamaño y el origen (par) del fotograma dentro del lienzo.
struct RefSource<'a> {
    recon: &'a [u8],
    src: &'a [u8],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
}

fn encode_with_alpha_ref(
    rgb: &[u8],
    alpha: Option<&[u8]>,
    width: usize,
    height: usize,
    p: &Params,
    rsrc: Option<RefSource>,
) -> Encoded {
    let mut header = make_header(rgb, width, height, p);
    if alpha.is_some() {
        header.tools |= TOOL_ALPHA;
    }
    if rsrc.is_some() {
        header.tools |= TOOL_INTER;
    }
    let rc = rsrc.map(|r| RefCanvas::new(r.recon, Some(r.src), r.w, r.h, &header, r.x, r.y));
    let tiles = tile_grid(width, height, header.tile_size());
    // Hilos por tesela: todos los disponibles aunque las teselas también vayan
    // en paralelo, porque suelen ser de tamaños distintos (las del borde son
    // más pequeñas) y el frente de onda de cada una tiene esperas.
    let mut p2 = p.clone();
    if p2.threads == 0 {
        p2.threads = available_threads();
    }
    let p = &p2;
    let res = par_map(&tiles, |t| {
        let a = alpha.map(|a| crop(a, 1, width, t));
        let r = encode_tile_ref(
            &crop(rgb, 3, width, t),
            a.as_deref(),
            t.w,
            t.h,
            &header,
            p,
            rc.as_ref().map(|rc| (rc, t.x, t.y)),
        );
        (r.0, r.1, r.2)
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
        bytes: assemble_full(&header, &p.meta, preview.as_deref(), &payloads),
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
/// reconstrucción y la fuente rellenada.
///
/// La decisión (RDO) de cada superbloque se hace en frente de onda: (x, y)
/// solo ve los píxeles ya decididos de la fila y−1 hasta x+1 y de la fila y
/// hasta x−1. Las probabilidades con que estima el coste son las de su propia
/// fila (actualizadas simulando la escritura de cada superbloque decidido),
/// y cada fila arranca con las de la anterior tras su segundo superbloque,
/// como el WPP de HEVC. Así nada depende del número de hilos y el resultado es
/// idéntico con 1 o con N. La escritura real es secuencial, con los contextos
/// de verdad, así que el decodificador no cambia.
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
    threads: usize,
    ip: Option<InterPlane<'_>>,
) -> (Plane, Vec<f32>) {
    use std::sync::{Condvar, Mutex};
    let (src, pw, ph) = pad_plane(src, w, h);
    let hash = st.screen.then(|| tools::HashIndex::build(&src, pw, ph));
    let (cols, rows) = (pw / SB, ph / SB);
    let total = cols * rows;
    let qm_ro: &QMap = qm;
    let kq = SB / AQ;
    let code_dqs = |ctx: &mut Ctx, enc: &mut Encoder, sx: usize, sy: usize| {
        if code_dq {
            for cy in sy * kq..(sy * kq + kq).min(qm_ro.h) {
                for cx in sx * kq..(sx * kq + kq).min(qm_ro.w) {
                    ctx.dq.code(enc, qm_ro.dq[cy * qm_ro.w + cx]);
                }
            }
        }
    };

    struct Sync {
        plane: Plane,
        done: Vec<bool>,
        nodes: Vec<Option<Node>>,
        /// Probabilidades de la fila tras simular el superbloque k.
        after: Vec<Option<Ctx>>,
    }
    let shared = Mutex::new(Sync {
        plane: Plane::new(pw, ph),
        done: vec![false; total],
        nodes: (0..total).map(|_| None).collect(),
        after: (0..total).map(|_| None).collect(),
    });
    let cv = Condvar::new();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let ctx0 = st.ctx.clone();
    let base = st.shallow();
    // Superbloque del que la fila hereda las probabilidades al empezar.
    let seed_col = 1.min(cols - 1);

    // Zona del plano que el RDO de (sx, sy) puede leer: en fotos, los
    // superbloques vecinos (referencias intra y contextos); con contenido
    // sintético, todo lo anterior (la copia intra-imagen busca en cualquier sitio).
    type Rect = (usize, usize, usize, usize);
    let window = |sx: usize, sy: usize| -> Rect {
        if hash.is_some() {
            (0, 0, pw, (sy + 1) * SB)
        } else {
            let (x0, y0) = (sx.saturating_sub(1) * SB, sy.saturating_sub(1) * SB);
            (x0, y0, ((sx + 2) * SB).min(pw) - x0, (sy + 1) * SB - y0)
        }
    };

    // Cada hilo tiene su propio plano; fuera de la ventana todo está como no
    // disponible, así que lo que quede de otros superbloques no se lee.
    let rdo = |k: usize, mine: &mut Option<(Plane, Rect)>| {
        let (sx, sy) = (k % cols, k / cols);
        let (wx, wy, ww, wh) = window(sx, sy);
        let mut plane = match mine.take() {
            Some((mut p, (ox, oy, ow, oh))) => {
                p.clear_avail(ox, oy, ow, oh);
                p
            }
            None => Plane::new(pw, ph),
        };
        let ctx = {
            let mut g = shared.lock().unwrap();
            loop {
                let left = sx == 0 || g.done[k - 1];
                let up = sy == 0 || g.done[(sy - 1) * cols + (sx + 1).min(cols - 1)];
                if left && up {
                    break;
                }
                g = cv.wait(g).unwrap();
            }
            // La columna semilla tiene dos consumidores (el siguiente de su
            // fila y el inicio de la fila de abajo), en cualquier orden: se clona.
            let ctx = if sx > 0 {
                if sx - 1 == seed_col {
                    g.after[k - 1].clone()
                } else {
                    g.after[k - 1].take()
                }
            } else if sy > 0 {
                g.after[(sy - 1) * cols + seed_col].clone()
            } else {
                Some(ctx0.clone())
            };
            plane.copy_region(&g.plane, wx, wy, ww, wh);
            ctx.unwrap()
        };
        // Oculta lo que no es determinista: fila y−1 más allá de x+1, fila y desde x y filas posteriores.
        for gy in wy / 4..(wy + wh) / 4 {
            let row = gy * 4 / SB;
            for gx in wx / 4..(wx + ww) / 4 {
                let col = gx * 4 / SB;
                let visible =
                    row + 1 < sy || (row + 1 == sy && col <= sx + 1) || (row == sy && col < sx);
                if !visible {
                    plane.avail[gy * plane.gw + gx] = false;
                }
            }
        }
        let mut es = EncState {
            ctx,
            ..base.shallow()
        };
        let mut pe = PlaneEnc {
            src: &src,
            plane,
            pc,
            inter: ip,
            hash: hash.as_ref(),
            sc: ScState::default(),
        };
        let (_, node) = es.rdo_node(&mut pe, lref, qm_ro, sx * SB, sy * SB, SB);
        // Simula la escritura para que las probabilidades de la fila avancen.
        let mut sink = Encoder::new();
        code_dqs(&mut es.ctx, &mut sink, sx, sy);
        es.write_node(
            &mut sink,
            &pe.plane,
            pc,
            &mut ScState::default(),
            sx * SB,
            sy * SB,
            SB,
            &node,
        );
        let mut g = shared.lock().unwrap();
        g.plane.copy_region(&pe.plane, sx * SB, sy * SB, SB, SB);
        g.nodes[k] = Some(node);
        if sx + 1 < cols || (sx == seed_col && sy + 1 < rows) {
            g.after[k] = Some(es.ctx);
        }
        g.done[k] = true;
        cv.notify_all();
        drop(g);
        *mine = Some((pe.plane, (wx, wy, ww, wh)));
    };

    // Escritura secuencial en orden de barrido, con su propia copia del plano
    // para no retener el cerrojo mientras codifica.
    let mut wst = ScState::default();
    let mut wplane = Plane::new(pw, ph);
    let mut write = |k: usize, st: &mut EncState, enc: &mut Encoder| {
        let (sx, sy) = (k % cols, k / cols);
        let node = {
            let mut g = shared.lock().unwrap();
            while !g.done[k] {
                g = cv.wait(g).unwrap();
            }
            wplane.copy_region(&g.plane, sx * SB, sy * SB, SB, SB);
            g.nodes[k].take().unwrap()
        };
        code_dqs(&mut st.ctx, enc, sx, sy);
        st.write_node(enc, &wplane, pc, &mut wst, sx * SB, sy * SB, SB, &node);
    };

    if threads <= 1 {
        let mut mine = None;
        for k in 0..total {
            rdo(k, &mut mine);
            write(k, st, enc);
        }
    } else {
        // Reparte por diagonales del frente (x + 2y): en orden de barrido,
        // todos los hilos acabarían en la misma fila esperándose entre sí.
        let mut order: Vec<usize> = (0..total).collect();
        order.sort_by_key(|&k| (k % cols + 2 * (k / cols), k / cols));
        let order = &order;
        let task = || {
            let mut mine = None;
            loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= total {
                    break;
                }
                rdo(order[i], &mut mine);
            }
            drop(mine);
            #[cfg(target_os = "wasi")]
            crate::talloc::flush();
        };
        let n = threads.min(total);
        #[cfg(target_os = "wasi")]
        crate::talloc::reserve(n, pw * ph, total);
        std::thread::scope(|scope| {
            // Se esperan con join para que ninguno siga vivo al salir.
            let handles: Vec<_> = (0..n)
                .map(|_| {
                    // Pila explícita: en WebAssembly la de los hilos es pequeña
                    // por defecto y no hay páginas de guarda.
                    std::thread::Builder::new()
                        .stack_size(4 << 20)
                        .spawn_scoped(scope, task)
                        .expect("no se pudo crear un hilo del codificador")
                })
                .collect();
            for k in 0..total {
                write(k, st, enc);
            }
            for h in handles {
                h.join().expect("un hilo del codificador falló");
            }
        });
        // El hilo principal ha liberado lo que crearon los otros (árboles,
        // probabilidades): que vuelva a malloc para el plano siguiente.
        #[cfg(target_os = "wasi")]
        crate::talloc::flush();
    }
    let plane = shared.into_inner().unwrap().plane;
    (plane, src)
}

/// `encode_plane` para el alfa (sin predicción entre fotogramas).
#[allow(clippy::too_many_arguments)]
fn encode_plane_alpha(
    st: &mut EncState,
    enc: &mut Encoder,
    src: &[f32],
    w: usize,
    h: usize,
    pc: usize,
    lref: Option<&LumaRef>,
    qm: &mut QMap,
    code_dq: bool,
    threads: usize,
) -> (Plane, Vec<f32>) {
    encode_plane(st, enc, src, w, h, pc, lref, qm, code_dq, threads, None)
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
    encode_tile_ref(rgb, alpha, width, height, header, p, None)
}

/// Como `encode_tile_stats`, con el fotograma anterior como referencia
/// (y el origen de la tesela dentro del fotograma).
#[allow(clippy::type_complexity)]
fn encode_tile_ref(
    rgb: &[u8],
    alpha: Option<&[u8]>,
    width: usize,
    height: usize,
    header: &Header,
    p: &Params,
    iref: Option<(&RefCanvas, usize, usize)>,
) -> (Vec<u8>, Vec<u8>, Option<Vec<u8>>, Vec<(u32, u32)>) {
    let lossless = header.tools & TOOL_LOSSLESS != 0;
    let sub = header.subsample && !lossless;
    let base_q = header.qstep();
    let cq = header.chroma_q as f32 / 32.0;
    let mut st = EncState {
        ctx: if lossless {
            Ctx::new()
        } else {
            Ctx::trained(base_q, header.tools & TOOL_SCREEN != 0, header.version)
        },
        qt: std::sync::Arc::new(QuantTables::new(
            header.qm_luma as f32 / 16.0,
            header.qm_chroma as f32 / 16.0,
        )),
        lambda_k: p.lambda_k,
        topk: (p.effort.clamp(1, 9) as usize) + 1,
        screen: header.tools & TOOL_SCREEN != 0,
        lossless,
        v2: header.version >= 2,
        effort: p.effort.clamp(1, 9),
        inter: iref.is_some(),
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
            p.threads.max(1),
            iref.map(|(rc, tx, ty)| rc.plane(pi, tx, ty)),
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
            qt: std::sync::Arc::new(QuantTables::new(0.0, 0.0)),
            lambda_k: p.lambda_k,
            topk: st.topk,
            screen: true,
            lossless: true,
            v2: header.version >= 2,
            effort: st.effort,
            inter: false,
        };
        let mut aq = unit_qmap(width, height);
        let (plane, _) = encode_plane_alpha(
            &mut ast,
            &mut enc,
            &a,
            width,
            height,
            0,
            None,
            &mut aq,
            false,
            p.threads.max(1),
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
    /// Flujo de bits v2 (particiones rectangulares).
    v2: bool,
    /// Fotograma con predicción desde el anterior.
    inter: bool,
    /// Fotograma anterior y (plano, origen de la tesela) que se decodifica ahora.
    rc: Option<std::sync::Arc<RefCanvas>>,
    cur: (usize, usize, usize),
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
            if self.v2
                && let Some(horz) = code_rect(&mut self.ctx, dec, pc, n, None)
            {
                for (hx, hy, hw, hh) in rect_halves(x0, y0, n, horz) {
                    self.read_leaf(dec, p, lref, qm, pc, st, hx, hy, hw, hh);
                }
                return;
            }
        }
        self.read_leaf(dec, p, lref, qm, pc, st, x0, y0, n, n);
    }

    #[allow(clippy::too_many_arguments)]
    fn read_leaf(
        &mut self,
        dec: &mut Decoder,
        p: &mut Plane,
        lref: Option<&LumaRef>,
        qm: &QMap,
        pc: usize,
        st: &mut ScState,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
    ) {
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
            w,
            h,
            self.screen,
            self.lossless,
            &dummy,
            st,
            self.inter,
        );
        let kind = match kind {
            Kind::Intra(m) if m == MODE_CCLM && lref.is_none() => Kind::Intra(MODE_DC),
            Kind::Intra(m) if m > MODE_CCLM => Kind::Intra(MODE_DC),
            k => k,
        };
        let mut levels = vec![0i32; w * h];
        let coef = if ts {
            &mut self.ctx.coef_ts
        } else {
            &mut self.ctx.coef
        };
        let nz = coef.code_block(dec, pc, w, h, cbf_ctx(p, x0, y0), &mut levels);
        let refs = predict::build_refs(p, x0, y0, w, h);
        let mut pred = vec![0i32; w * h];
        let (pi, tx, ty) = self.cur;
        let ip = self.rc.as_ref().map(|rc| rc.plane(pi, tx, ty));
        predict_leaf(p, &refs, lref, &kind, x0, y0, w, h, ip.as_ref(), &mut pred);
        let mut recon = vec![0u8; w * h];
        let q = qm.q(pc, x0, y0, w, h);
        reconstruct(
            &levels,
            &pred,
            q,
            self.qt.get(pc, w, h),
            w,
            h,
            ts,
            &mut recon,
        );
        p.write_block(x0, y0, w, h, &recon);
        p.set_block(x0, y0, w, h, kind.mark(), nz);
    }
}

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    /// Canal alfa, si la imagen lo tiene.
    pub alpha: Option<Vec<u8>>,
    /// Metadatos del archivo (perfil ICC, EXIF).
    pub meta: Metadata,
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
    /// Metadatos (perfil ICC, EXIF).
    pub meta: Metadata,
}

/// Máximo de píxeles que acepta el decodificador. El formato admite hasta
/// 65 536 × 65 536, pero un archivo de pocos bytes podría declarar esas
/// dimensiones y obligar a reservar decenas de GB: por encima de este límite
/// (268 MP, p. ej. 16 384 × 16 384) se rechaza antes de reservar nada.
pub const MAX_DECODE_PIXELS: u64 = 1 << 28;

pub fn info(data: &[u8]) -> Result<Info, String> {
    let header = Header::read(data)?;
    if header.width as u64 * header.height as u64 > MAX_DECODE_PIXELS {
        return Err(format!(
            "imagen demasiado grande para decodificar ({}×{}; máximo {} píxeles)",
            header.width, header.height, MAX_DECODE_PIXELS
        ));
    }
    let tiles = tile_grid(
        header.width as usize,
        header.height as usize,
        header.tile_size(),
    );
    if header.tools & TOOL_ANIM != 0 {
        return Err("es una animación: usa anim_info / decode_animation".into());
    }
    let mut base = HEADER_LEN;
    let mut meta = Metadata::default();
    if header.tools & TOOL_META != 0 {
        (meta, base) = Metadata::read(data, base)?;
    }
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
        meta,
    })
}

/// Decodifica solo la vista previa (si el archivo la tiene).
pub fn decode_preview(data: &[u8]) -> Result<Option<Decoded>, String> {
    let header = Header::read(data)?;
    if header.tools & TOOL_PREVIEW == 0 {
        return Ok(None);
    }
    let base = if header.tools & TOOL_META != 0 {
        Metadata::read(data, HEADER_LEN)?.1
    } else {
        HEADER_LEN
    };
    let len = data
        .get(base..base + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or("archivo truncado")?;
    let pv = data
        .get(base + 4..base + 4 + len)
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
    decode_tile_ref(payload, w, ht, h, None, 0, 0)
}

/// Como `decode_tile`, con el fotograma anterior y el origen de la tesela.
fn decode_tile_ref(
    payload: &[u8],
    w: usize,
    ht: usize,
    h: &Header,
    rc: Option<std::sync::Arc<RefCanvas>>,
    tx: usize,
    ty: usize,
) -> (Vec<u8>, Option<Vec<u8>>) {
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
            Ctx::trained(base_q, h.tools & TOOL_SCREEN != 0, h.version)
        },
        qt: QuantTables::new(h.qm_luma as f32 / 16.0, h.qm_chroma as f32 / 16.0),
        screen: h.tools & TOOL_SCREEN != 0,
        lossless,
        v2: h.version >= 2,
        inter: h.tools & TOOL_INTER != 0,
        rc,
        cur: (0, tx, ty),
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
        st.cur.0 = pi;
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
            v2: h.version >= 2,
            inter: false,
            rc: None,
            cur: (0, 0, 0),
        };
        let mut aq = unit_qmap(w, ht);
        let plane = decode_plane(&mut ast, &mut dec, w, ht, 0, None, &mut aq, false);
        crop_plane(&plane, w, ht)
    });
    (finish_planes(h, &planes, w, ht), alpha)
}

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    // Una animación se decodifica como su primer fotograma (que cubre todo el lienzo).
    if let Some(a) = anim_info(data)? {
        let d = decode_frame(data, &a.frames[0], None)?;
        return Ok(Decoded { meta: a.meta, ..d });
    }
    decode_with_ref(data, None)
}

/// Decodifica un archivo fijo; si es un fotograma con predicción entre
/// fotogramas, necesita el fotograma anterior (`rc`).
fn decode_with_ref(data: &[u8], rc: Option<std::sync::Arc<RefCanvas>>) -> Result<Decoded, String> {
    let inf = info(data)?;
    if inf.header.tools & TOOL_INTER != 0 && rc.is_none() {
        return Err("fotograma que depende del anterior: decodifica la animación completa".into());
    }
    let (w, ht) = (inf.header.width as usize, inf.header.height as usize);
    let idx: Vec<usize> = (0..inf.tiles.len()).collect();
    let parts = par_map(&idx, |&i| {
        let t = inf.tiles[i];
        decode_tile_ref(&data[inf.ranges[i].clone()], t.w, t.h, &inf.header, rc.clone(), t.x, t.y)
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
        meta: inf.meta.clone(),
    })
}

// ------------------------------------------------------------- Animación

/// Un fotograma de una animación: cuánto dura y qué rectángulo del lienzo
/// sustituye. Los datos son un archivo `.tsl` completo de ese rectángulo.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameInfo {
    pub duration_ms: u32,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub range: std::ops::Range<usize>,
}

/// Estructura de un archivo animado.
#[derive(Clone, Debug)]
pub struct AnimInfo {
    pub width: usize,
    pub height: usize,
    /// Repeticiones (0 = sin fin).
    pub loops: u16,
    pub frames: Vec<FrameInfo>,
    pub meta: Metadata,
}

/// Lee la estructura de una animación. `Ok(None)` si el archivo no es animado.
///
/// Sección (v2, bit ANIM), tras la cabecera y los metadatos:
/// `u16 repeticiones | u16 reservado (0) | u32 n` y, por fotograma,
/// `u32 duración (ms) | u32 x | u32 y | u32 ancho | u32 alto | u32 longitud | datos`.
/// El primer fotograma cubre todo el lienzo; los demás, un rectángulo dentro de él.
pub fn anim_info(data: &[u8]) -> Result<Option<AnimInfo>, String> {
    let header = Header::read(data)?;
    if header.tools & TOOL_ANIM == 0 {
        return Ok(None);
    }
    let (w, h) = (header.width as usize, header.height as usize);
    if w as u64 * h as u64 > MAX_DECODE_PIXELS {
        return Err("lienzo demasiado grande".into());
    }
    if header.tools & TOOL_PREVIEW != 0 {
        return Err("una animación no lleva vista previa".into());
    }
    let mut pos = HEADER_LEN;
    let mut meta = Metadata::default();
    if header.tools & TOOL_META != 0 {
        (meta, pos) = Metadata::read(data, pos)?;
    }
    let u16_at = |p: usize| data.get(p..p + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |p: usize| {
        data.get(p..p + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let loops = u16_at(pos).ok_or("archivo truncado")?;
    if u16_at(pos + 2).ok_or("archivo truncado")? != 0 {
        return Err("animación no válida".into());
    }
    let n = u32_at(pos + 4).ok_or("archivo truncado")? as usize;
    pos += 8;
    // Cada fotograma ocupa al menos 24 + 23 bytes: n no puede ser absurdo.
    if n == 0 || n > (data.len() - pos.min(data.len())) / 47 {
        return Err("animación no válida".into());
    }
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        let f: Vec<usize> = (0..6)
            .map(|k| u32_at(pos + 4 * k).map(|v| v as usize))
            .collect::<Option<_>>()
            .ok_or("archivo truncado")?;
        let (dur, fx, fy, fw, fh, len) = (f[0], f[1], f[2], f[3], f[4], f[5]);
        let start = pos + 24;
        let end = start
            .checked_add(len)
            .filter(|&e| e <= data.len())
            .ok_or("archivo truncado")?;
        let full = fx == 0 && fy == 0 && fw == w && fh == h;
        if fw == 0 || fh == 0 || fx + fw > w || fy + fh > h || (i == 0 && !full) {
            return Err("fotograma fuera del lienzo".into());
        }
        frames.push(FrameInfo {
            duration_ms: dur as u32,
            x: fx,
            y: fy,
            w: fw,
            h: fh,
            range: start..end,
        });
        pos = end;
    }
    Ok(Some(AnimInfo {
        width: w,
        height: h,
        loops,
        frames,
        meta,
    }))
}

/// Decodifica el rectángulo de un fotograma (sin componer).
pub fn decode_frame(
    data: &[u8],
    f: &FrameInfo,
    prev: Option<(&[u8], usize, usize)>,
) -> Result<Decoded, String> {
    let inner = &data[f.range.clone()];
    let h = Header::read(inner)?;
    if h.tools & TOOL_ANIM != 0 || h.width as usize != f.w || h.height as usize != f.h {
        return Err("fotograma no válido".into());
    }
    if h.tools & TOOL_INTER == 0 {
        return decode(inner);
    }
    // Predice del lienzo anterior (RGBA de cw × ch).
    let (canvas, cw, ch) = prev.ok_or("fotograma que depende del anterior")?;
    if h.subsample && (f.x | f.y) & 1 != 0 {
        return Err("fotograma con predicción en coordenadas impares".into());
    }
    let rgb: Vec<u8> = canvas.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let rc = std::sync::Arc::new(RefCanvas::new(&rgb, None, cw, ch, &h, f.x, f.y));
    decode_with_ref(inner, Some(rc))
}

/// Pega un fotograma decodificado en el lienzo RGBA.
pub fn compose(canvas: &mut [u8], width: usize, frame: &Decoded, f: &FrameInfo) {
    let rgba = frame.rgba();
    for y in 0..f.h {
        let dst = ((f.y + y) * width + f.x) * 4;
        canvas[dst..dst + f.w * 4].copy_from_slice(&rgba[y * f.w * 4..(y + 1) * f.w * 4]);
    }
}

/// Decodifica todos los fotogramas ya compuestos (lienzos RGBA completos),
/// con su duración. `max_pixels` limita la memoria: fotogramas × píxeles del lienzo.
#[allow(clippy::type_complexity)]
pub fn decode_animation(
    data: &[u8],
    max_pixels: u64,
) -> Result<(AnimInfo, Vec<(Vec<u8>, u32)>), String> {
    let a = anim_info(data)?.ok_or("no es una animación")?;
    if a.frames.len() as u64 * (a.width * a.height) as u64 > max_pixels {
        return Err("animación demasiado grande para decodificarla entera".into());
    }
    let mut canvas = vec![0u8; a.width * a.height * 4];
    let mut out = Vec::with_capacity(a.frames.len());
    for f in &a.frames {
        let d = decode_frame(data, f, Some((&canvas, a.width, a.height)))?;
        compose(&mut canvas, a.width, &d, f);
        out.push((canvas.clone(), f.duration_ms));
    }
    Ok((a, out))
}

/// Rectángulo mínimo que contiene los píxeles distintos entre dos fotogramas RGBA.
fn changed_rect(a: &[u8], b: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        let ra = &a[y * w * 4..(y + 1) * w * 4];
        let rb = &b[y * w * 4..(y + 1) * w * 4];
        if ra == rb {
            continue;
        }
        let first = (0..w)
            .find(|&x| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4])
            .unwrap();
        let last = (0..w)
            .rev()
            .find(|&x| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4])
            .unwrap();
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x1 > x0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

/// Codifica una animación. `frames` son lienzos RGBA completos de `width ×
/// height`. Cada fotograma solo guarda el rectángulo que cambia respecto al
/// anterior; los que no cambian alargan la duración del anterior.
pub fn encode_animation(
    frames: &[Vec<u8>],
    durations_ms: &[u32],
    width: usize,
    height: usize,
    loops: u16,
    p: &Params,
) -> Result<Vec<u8>, String> {
    if frames.is_empty() || frames.len() != durations_ms.len() {
        return Err("hace falta al menos un fotograma, cada uno con su duración".into());
    }
    if p.version < 2 {
        return Err("la animación necesita la versión 2 del formato".into());
    }
    let sub = Params {
        meta: Metadata::default(),
        preview: false,
        ..p.clone()
    };
    // Predicción entre fotogramas (TESELA_NOINTER=1 la desactiva, para comparar).
    let use_inter = std::env::var_os("TESELA_NOINTER").is_none();
    let rgb_of = |f: &[u8]| -> Vec<u8> { f.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect() };
    // Lienzo tal como lo verá el decodificador (RGBA).
    let mut recon = vec![0u8; width * height * 4];
    // (duración, x, y, w, h, datos)
    let mut list: Vec<(u32, usize, usize, usize, usize, Vec<u8>)> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let rect = if i == 0 {
            Some((0, 0, width, height))
        } else {
            changed_rect(&frames[i - 1], f, width, height)
        };
        match rect {
            None => list.last_mut().unwrap().0 += durations_ms[i],
            Some((x, y, w, h)) => {
                // Origen par: la croma 4:2:0 del fotograma y la de la referencia coinciden.
                let (x, y, w, h) = (x & !1, y & !1, w + (x & 1), h + (y & 1));
                let mut crop = Vec::with_capacity(w * h * 4);
                for yy in y..y + h {
                    crop.extend_from_slice(&f[(yy * width + x) * 4..(yy * width + x + w) * 4]);
                }
                let crgb = rgb_of(&crop);
                let alpha: Vec<u8> = crop.chunks_exact(4).map(|px| px[3]).collect();
                let has_alpha = alpha.iter().any(|&a| a != 255);
                let (prev_recon, prev_src) = (rgb_of(&recon), rgb_of(&frames[i.max(1) - 1]));
                let rsrc = (i > 0 && use_inter).then(|| RefSource {
                    recon: &prev_recon,
                    src: &prev_src,
                    w: width,
                    h: height,
                    x,
                    y,
                });
                let e = encode_with_alpha_ref(&crgb, has_alpha.then_some(&alpha[..]), w, h, &sub, rsrc);
                let d = Decoded {
                    width: w,
                    height: h,
                    rgb: e.recon_rgb.clone(),
                    alpha: e.recon_alpha.clone(),
                    meta: Metadata::default(),
                };
                let fi = FrameInfo { duration_ms: 0, x, y, w, h, range: 0..0 };
                compose(&mut recon, width, &d, &fi);
                list.push((durations_ms[i], x, y, w, h, e.bytes));
            }
        }
    }
    let rgb: Vec<u8> = frames[0]
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    let mut header = make_header(&rgb, width, height, &sub);
    header.tools = (header.tools & !(TOOL_PREVIEW | TOOL_META)) | TOOL_ANIM;
    if !p.meta.is_empty() {
        header.tools |= TOOL_META;
    }
    let mut out = Vec::new();
    header.write(&mut out);
    if !p.meta.is_empty() {
        p.meta.write(&mut out);
    }
    out.extend_from_slice(&loops.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for (d, x, y, w, h, bytes) in &list {
        for v in [
            *d,
            *x as u32,
            *y as u32,
            *w as u32,
            *h as u32,
            bytes.len() as u32,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(bytes);
    }
    Ok(out)
}
