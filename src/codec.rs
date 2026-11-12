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
/// Hoja con el movimiento global de la tesela.
const MARK_WARP: u8 = 203;
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

/// ¿Es este perfil ICC equivalente a sRGB? Perfil de matriz (sin tablas
/// A2B0/B2A0) con los primarios de sRGB adaptados a D50 y curvas de tono como
/// la de sRGB. Ante la duda, no: entonces el perfil se guarda tal cual.
pub fn icc_is_srgb(p: &[u8]) -> bool {
    let be32 = |o: usize| -> Option<u32> {
        p.get(o..o + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
    };
    let be16 = |o: usize| -> Option<u32> {
        p.get(o..o + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as u32)
    };
    let s15 = |o: usize| be32(o).map(|v| v as i32 as f64 / 65536.0);
    if p.len() < 132 || &p[16..20] != b"RGB " || &p[20..24] != b"XYZ " {
        return false;
    }
    let n = match be32(128) {
        Some(n) if n <= 100 => n as usize,
        _ => return false,
    };
    let tag = |sig: &[u8; 4]| -> Option<(usize, usize)> {
        (0..n).find_map(|i| {
            let o = 132 + 12 * i;
            (p.get(o..o + 4)? == sig)
                .then(|| Some((be32(o + 4)? as usize, be32(o + 8)? as usize)))?
        })
    };
    if tag(b"A2B0").is_some() || tag(b"B2A0").is_some() {
        return false;
    }
    let xyz = |sig: &[u8; 4]| -> Option<[f64; 3]> {
        let (o, _) = tag(sig)?;
        (p.get(o..o + 4)? == b"XYZ ").then_some(())?;
        Some([s15(o + 8)?, s15(o + 12)?, s15(o + 16)?])
    };
    let want = [
        (b"rXYZ", [0.4361, 0.2225, 0.0139]),
        (b"gXYZ", [0.3851, 0.7169, 0.0971]),
        (b"bXYZ", [0.1431, 0.0606, 0.7141]),
    ];
    for (sig, w) in want {
        match xyz(sig) {
            Some(v) if v.iter().zip(w).all(|(a, b)| (a - b).abs() < 0.003) => {}
            _ => return false,
        }
    }
    // Curva de tono evaluada en x ∈ [0, 1].
    let trc = |sig: &[u8; 4], x: f64| -> Option<f64> {
        let (o, _) = tag(sig)?;
        match p.get(o..o + 4)? {
            b"curv" => {
                let cnt = be32(o + 8)? as usize;
                match cnt {
                    0 => Some(x),
                    1 => Some(x.powf(be16(o + 12)? as f64 / 256.0)),
                    _ => {
                        let f = x * (cnt - 1) as f64;
                        let i = (f as usize).min(cnt - 2);
                        let a = be16(o + 12 + 2 * i)? as f64;
                        let b = be16(o + 12 + 2 * (i + 1))? as f64;
                        Some((a + (b - a) * (f - i as f64)) / 65535.0)
                    }
                }
            }
            b"para" => {
                let ty = be16(o + 8)?;
                let g = s15(o + 12)?;
                let prm = |k: usize| s15(o + 12 + 4 * k);
                match ty {
                    0 => Some(x.powf(g)),
                    3 => {
                        let (a, b, c, d) = (prm(1)?, prm(2)?, prm(3)?, prm(4)?);
                        Some(if x >= d {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            c * x
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    };
    let srgb = |x: f64| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    for sig in [b"rTRC", b"gTRC", b"bTRC"] {
        for x in [0.02, 0.1, 0.25, 0.5, 0.75, 0.95] {
            match trc(sig, x) {
                Some(v) if (v - srgb(x)).abs() < 0.006 => {}
                _ => return false,
            }
        }
    }
    true
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

    /// Los mismos metadatos sin el perfil ICC si es sRGB: sin perfil, los
    /// valores ya se interpretan como sRGB, así que solo ocupa (~3 KB).
    pub fn without_srgb(&self) -> Metadata {
        Metadata {
            icc: self.icc.clone().filter(|p| !icc_is_srgb(p)),
            exif: self.exif.clone(),
        }
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
    /// ¿Hoja con el movimiento global? (solo si la tesela lo lleva)
    warp: Vec<Model>,
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
        self.warp.visit(f);
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
            warp: vec![Model::new(); 2 * 5 * 3],
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
    /// Fotogramas predichos: dq deducido de la referencia; se codifica la
    /// diferencia con él (vacío = 0).
    pred: Vec<i32>,
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
                let p = self.pred.get(i).copied().unwrap_or(0);
                self.dq[i] = (p + ctx.dq.code(c, self.dq[i] - p)).clamp(-64, 64);
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
    /// Copia del fotograma anterior desplazada por el vector (cuartos de píxel).
    Inter((i32, i32)),
    /// Copia del fotograma anterior con el movimiento global de la tesela
    /// (un vector por bloque de 4 × 4 deducido del modelo afín).
    Warp,
}

impl Kind {
    /// Valor del mapa de modos (para el contexto de los vecinos).
    fn mark(&self) -> u8 {
        match self {
            Kind::Intra(m) => *m,
            Kind::Palette(_) => MARK_PALETTE,
            Kind::Ibc(_) => MARK_IBC,
            Kind::Inter(_) => MARK_INTER,
            Kind::Warp => MARK_WARP,
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

/// Predictor del vector de movimiento: mediana de los vectores de los vecinos
/// izquierdo, superior y superior izquierdo que sean hojas predichas del
/// fotograma anterior; los que falten valen el último vector del plano. Sin
/// ningún vecino así, el último vector. (El superior derecho no se usa: en el
/// orden de codificación a veces aún no está decodificado.)
fn mv_pred(p: &Plane, x0: usize, y0: usize, w: usize, last: (i32, i32)) -> (i32, i32) {
    let (x, y) = (x0 as isize, y0 as isize);
    let a = inter_mv_at(p, x - 1, y);
    let b = inter_mv_at(p, x, y - 1);
    let _ = w;
    let c = inter_mv_at(p, x - 1, y - 1);
    match (a, b, c) {
        (None, None, None) => last,
        (Some(v), None, None) | (None, Some(v), None) | (None, None, Some(v)) => v,
        _ => {
            let [a, b, c] = [a, b, c].map(|v| v.unwrap_or(last));
            let med = |p: i32, q: i32, r: i32| p.max(q).min(p.min(q).max(r));
            (med(a.0, b.0, c.0), med(a.1, b.1, c.1))
        }
    }
}

/// Vector del bloque que contiene (x, y) si es una hoja predicha del
/// fotograma anterior (con vector propio o con el movimiento global).
fn inter_mv_at(p: &Plane, x: isize, y: isize) -> Option<(i32, i32)> {
    p.mv_at(x, y, MARK_INTER)
        .or_else(|| p.mv_at(x, y, MARK_WARP))
}

fn inter_ctx(p: &Plane, x0: usize, y0: usize) -> usize {
    let it = |v: Option<(u8, u8, bool)>| {
        v.is_some_and(|(m, _, _)| m == MARK_INTER || m == MARK_WARP) as usize
    };
    it(p.neighbor(x0, y0, true)) + it(p.neighbor(x0, y0, false))
}

fn warp_ctx(p: &Plane, x0: usize, y0: usize) -> usize {
    let it = |v: Option<(u8, u8, bool)>| v.is_some_and(|(m, _, _)| m == MARK_WARP) as usize;
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
    warp: bool,
) -> (Kind, bool) {
    let n = w.max(h);
    let sz = size_index(n);
    let mut kind = None;
    if inter
        && c.bit(
            &mut ctx.inter[(pc * 5 + sz) * 3 + inter_ctx(p, x0, y0)],
            matches!(leaf.kind, Kind::Inter(_) | Kind::Warp),
        )
    {
        if warp
            && c.bit(
                &mut ctx.warp[(pc * 5 + sz) * 3 + warp_ctx(p, x0, y0)],
                matches!(leaf.kind, Kind::Warp),
            )
        {
            kind = Some(Kind::Warp);
        } else {
            let mv = match leaf.kind {
                Kind::Inter(m) => m,
                _ => (0, 0),
            };
            let mut pred = mv_pred(p, x0, y0, w, st.last_mv);
            let mv = ctx.mv.code(c, pc, mv, &mut pred);
            st.last_mv = mv;
            kind = Some(Kind::Inter(mv));
        }
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
        Kind::Warp => match ip {
            Some(ip) if ip.warp.is_some() => ip.predict_warp(x0, y0, w, h, out),
            _ => out[..w * h].fill(128),
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
    /// Submuestreo del plano respecto a la luma (1 o 2).
    d: usize,
    /// Movimiento global de la tesela, si lo lleva.
    warp: Option<Warp>,
    /// Plano de la referencia larga (fotograma dorado), si la hay.
    gdata: Option<&'a [u8]>,
}

/// Movimiento global (afín) de un fotograma, en coordenadas de luma del
/// lienzo: el vector en cuartos de píxel en (X, Y) es
/// `((a·X + b·Y + c) / 2^16, (d·X + e·Y + f) / 2^16)`, redondeado.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Warp {
    pub p: [i32; 6],
    /// Predice de la referencia larga (fotograma dorado) en vez del anterior.
    pub gold: bool,
}

/// Límites de los parámetros del movimiento global (el decodificador los recorta).
const WARP_LIN: i32 = 1 << 20;
const WARP_OFF: i32 = 1 << 30;

impl Warp {
    fn clamped(mut self) -> Self {
        for (i, v) in self.p.iter_mut().enumerate() {
            let m = if i % 3 == 2 { WARP_OFF } else { WARP_LIN };
            *v = (*v).clamp(-m, m);
        }
        self
    }

    /// Vector de luma (1/16 de píxel) en el punto (X, Y) del lienzo.
    fn mv_luma(&self, x: i64, y: i64) -> (i32, i32) {
        let p = self.p.map(|v| v as i64);
        let r = |v: i64| ((v + (1 << 13)) >> 14).clamp(-(1 << 22), 1 << 22) as i32;
        (r(p[0] * x + p[1] * y + p[2]), r(p[3] * x + p[4] * y + p[5]))
    }

    /// Escribe o lee los parámetros (al principio de la tesela).
    fn code<C: Coder>(c: &mut C, w: Option<Warp>) -> Option<Warp> {
        if !c.bypass(w.is_some()) {
            return None;
        }
        let w = w.unwrap_or_default();
        let gold = c.bypass(w.gold);
        let p = w.p.map(|v| code_sint_bypass(c, v));
        Some(Warp { p, gold }.clamped())
    }
}

/// Entero con signo sin contexto: número de bits (6), magnitud y signo.
fn code_sint_bypass<C: Coder>(c: &mut C, v: i32) -> i32 {
    let m = v.unsigned_abs().min(u32::MAX >> 1);
    let n = c.bypass_bits(32 - m.leading_zeros(), 6).min(31);
    if n == 0 {
        return 0;
    }
    // El bit más alto es 1: solo se escriben los n − 1 restantes.
    let rest = c.bypass_bits(m & ((1 << (n - 1)) - 1), n - 1);
    let mag = (1u32 << (n - 1)) | rest;
    let neg = c.bypass(v < 0);
    if neg { -(mag as i32) } else { mag as i32 }
}

/// Filtros de 1/16 de píxel para el movimiento global (seno cardinal con
/// ventana coseno², redondeado a enteros que suman 64; simétricos).
const WARP16: [[i32; 8]; 16] = [
    [0, 0, 0, 64, 0, 0, 0, 0],
    [0, 1, -3, 63, 4, -1, 0, 0],
    [-1, 2, -6, 63, 8, -3, 1, 0],
    [-1, 3, -8, 60, 13, -4, 1, 0],
    [-1, 3, -9, 57, 18, -6, 2, 0],
    [-1, 4, -10, 53, 23, -7, 2, 0],
    [-1, 4, -11, 48, 29, -8, 3, 0],
    [-1, 4, -11, 45, 34, -9, 3, -1],
    [-1, 4, -10, 39, 39, -10, 4, -1],
    [-1, 3, -9, 34, 45, -11, 4, -1],
    [0, 3, -8, 29, 48, -11, 4, -1],
    [0, 2, -7, 23, 53, -10, 4, -1],
    [0, 2, -6, 18, 57, -9, 3, -1],
    [0, 1, -4, 13, 60, -8, 3, -1],
    [0, 1, -3, 8, 63, -6, 2, -1],
    [0, 0, -1, 4, 63, -3, 1, 0],
];

/// Filtros de interpolación de cuarto de píxel (los de luma de HEVC; suman 64).
const QPEL: [[i32; 8]; 4] = [
    [0, 0, 0, 64, 0, 0, 0, 0],
    [-1, 4, -10, 58, 17, -5, 1, 0],
    [-1, 4, -11, 40, 40, -11, 4, -1],
    [0, 1, -5, 17, 58, -10, 4, -1],
];

impl InterPlane<'_> {
    /// Vector del bloque de 4 × 4 que empieza en (x0, y0) según el movimiento
    /// global, redondeado a cuartos de píxel (el que ven los vecinos).
    fn warp_mv(&self, x0: usize, y0: usize) -> (i32, i32) {
        let (x, y) = self.warp_mv16(x0, y0);
        ((x + 2) >> 2, (y + 2) >> 2)
    }

    /// Vector del bloque de 4 × 4 que empieza en (x0, y0) (coordenadas de la
    /// tesela en este plano) según el movimiento global, en 1/16 de píxel
    /// de este plano.
    fn warp_mv16(&self, x0: usize, y0: usize) -> (i32, i32) {
        let w = self.warp.unwrap_or_default();
        let d = self.d as i64;
        let cx = (self.ox + x0) as i64 * d + 2 * d;
        let cy = (self.oy + y0) as i64 * d + 2 * d;
        let (mx, my) = w.mv_luma(cx, cy);
        if self.d == 2 {
            ((mx + 1) >> 1, (my + 1) >> 1)
        } else {
            (mx, my)
        }
    }

    /// Predicción con el movimiento global: un vector por bloque de 4 × 4.
    fn predict_warp(&self, x0: usize, y0: usize, w: usize, h: usize, out: &mut [i32]) {
        // Con la referencia larga, las mismas coordenadas en su plano.
        let ip = match (self.warp.is_some_and(|w| w.gold), self.gdata) {
            (true, Some(g)) => InterPlane { data: g, ..*self },
            _ => *self,
        };
        let mut sub = [0i32; 16];
        for by in (0..h).step_by(4) {
            for bx in (0..w).step_by(4) {
                let mv = ip.warp_mv16(x0 + bx, y0 + by);
                ip.predict_with(x0 + bx, y0 + by, 4, 4, mv, 4, &WARP16, &mut sub);
                for y in 0..4 {
                    out[(by + y) * w + bx..(by + y) * w + bx + 4]
                        .copy_from_slice(&sub[y * 4..y * 4 + 4]);
                }
            }
        }
    }

    /// Guarda en el mapa del plano los vectores de una hoja con el movimiento global.
    fn set_warp_mvs(&self, p: &mut Plane, x0: usize, y0: usize, w: usize, h: usize) {
        for by in (0..h).step_by(4) {
            for bx in (0..w).step_by(4) {
                p.set_mv(x0 + bx, y0 + by, 4, 4, self.warp_mv(x0 + bx, y0 + by));
            }
        }
    }

    /// Copia desplazada por un vector en cuartos de píxel, con el filtro
    /// separable de 8 coeficientes (horizontal y luego vertical, en enteros);
    /// fuera del lienzo se repite el borde.
    fn predict(&self, x0: usize, y0: usize, w: usize, h: usize, mv: (i32, i32), out: &mut [i32]) {
        self.predict_with(x0, y0, w, h, mv, 2, &QPEL, out)
    }

    /// Como `predict`, con vectores en 1/2^`shift` de píxel y la tabla de filtros dada.
    #[allow(clippy::too_many_arguments)]
    fn predict_with(
        &self,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        mv: (i32, i32),
        shift: u32,
        table: &[[i32; 8]],
        out: &mut [i32],
    ) {
        let mask = (1 << shift) - 1;
        let bx = (self.ox + x0) as i64 + (mv.0 >> shift) as i64;
        let by = (self.oy + y0) as i64 + (mv.1 >> shift) as i64;
        let (fx, fy) = ((mv.0 & mask) as usize, (mv.1 & mask) as usize);
        let (pw, ph) = (self.w as i64, self.h as i64);
        debug_assert!(w <= 64 && h <= 64);
        // Ventana de la referencia (h + 7) × (w + 7) desde (bx − 3, by − 3),
        // repitiendo el borde fuera del plano. Si cae dentro, se lee directa.
        let sw = w + 7;
        PRED_BUF.with_borrow_mut(|(win, tmp)| {
            let (wx0, wy0) = (bx - 3, by - 3);
            if wx0 >= 0 && wy0 >= 0 && wx0 + sw as i64 <= pw && wy0 + (h + 7) as i64 <= ph {
                for r in 0..h + 7 {
                    let o = (wy0 as usize + r) * self.w + wx0 as usize;
                    win[r * sw..r * sw + sw].copy_from_slice(&self.data[o..o + sw]);
                }
            } else {
                for r in 0..h + 7 {
                    let yy = (wy0 + r as i64).clamp(0, ph - 1) as usize * self.w;
                    for c in 0..sw {
                        win[r * sw + c] =
                            self.data[yy + (wx0 + c as i64).clamp(0, pw - 1) as usize];
                    }
                }
            }
            if fx == 0 && fy == 0 {
                for y in 0..h {
                    for x in 0..w {
                        out[y * w + x] = win[(y + 3) * sw + x + 3] as i32;
                    }
                }
                return;
            }
            // Pasada horizontal sobre h + 7 filas (3 por encima y 4 por debajo),
            // luego vertical; con fracción 0 el filtro es 64 · muestra.
            let (cx, cy) = (&table[fx], &table[fy]);
            for r in 0..h + 7 {
                let row = &win[r * sw..r * sw + sw];
                let t = &mut tmp[r * w..r * w + w];
                if fx == 0 {
                    for x in 0..w {
                        t[x] = 64 * row[x + 3] as i32;
                    }
                } else {
                    for x in 0..w {
                        let p = &row[x..x + 8];
                        let mut s = 0;
                        for k in 0..8 {
                            s += cx[k] * p[k] as i32;
                        }
                        t[x] = s;
                    }
                }
            }
            for y in 0..h {
                let o = &mut out[y * w..y * w + w];
                if fy == 0 {
                    for x in 0..w {
                        o[x] = ((64 * tmp[(y + 3) * w + x] + 2048) >> 12).clamp(0, 255);
                    }
                } else {
                    for x in 0..w {
                        let mut s = 0;
                        for k in 0..8 {
                            s += cy[k] * tmp[(y + k) * w + x];
                        }
                        o[x] = ((s + 2048) >> 12).clamp(0, 255);
                    }
                }
            }
        });
    }
}

thread_local! {
    /// Búferes de la interpolación (ventana de la referencia y pasada
    /// horizontal), reutilizados para no ponerlos a cero en cada bloque.
    static PRED_BUF: std::cell::RefCell<(Vec<u8>, Vec<i32>)> =
        std::cell::RefCell::new((vec![0; 71 * 71], vec![0; 71 * 64]));
}

/// Fotograma anterior (lienzo completo) en el espacio de color del fotograma
/// actual, y dónde empieza el fotograma dentro del lienzo (coordenadas pares).
pub struct RefCanvas {
    planes: Vec<(Vec<u8>, usize, usize)>,
    hashes: Vec<Option<tools::HashIndex>>,
    x: usize,
    y: usize,
    sub: bool,
    /// Solo en el codificador: movimiento global elegido para el fotograma.
    warp: Option<Warp>,
    /// Planos de la referencia larga (mismo espacio de color), si la hay.
    gold: Option<RefPlanes>,
    /// Solo en el codificador: dq extra por celda AQ del fotograma (más
    /// grueso donde el fotograma anterior ya lo predice bien) y su ancho.
    tdq: Option<(Vec<i32>, usize)>,
}

/// Planos de un lienzo o de un fotograma: (muestras, ancho, alto).
type RefPlanes = Vec<(Vec<u8>, usize, usize)>;

/// Espacio de color de los planos de un archivo: (sin pérdida, croma 4:2:0).
fn ref_kind(h: &Header) -> (bool, bool) {
    let lossless = h.tools & TOOL_LOSSLESS != 0;
    (lossless, h.subsample && !lossless)
}

/// Planos de una imagen RGB en el espacio de color de `header`, redondeados a 8 bits.
fn planes_u8(rgb: &[u8], w: usize, h: usize, header: &Header) -> RefPlanes {
    color_planes(rgb, w, h, header)
        .into_iter()
        .map(|(p, pw, ph)| {
            let v = p
                .iter()
                .map(|v| v.round().clamp(0.0, 255.0) as u8)
                .collect();
            (v, pw, ph)
        })
        .collect()
}

/// Copia `src` (sw × sh) en `dst` (dw × dh) a partir de (x, y), recortando.
#[allow(clippy::too_many_arguments)]
fn paste_plane(
    dst: &mut [u8],
    dw: usize,
    dh: usize,
    x: usize,
    y: usize,
    src: &[u8],
    sw: usize,
    sh: usize,
) {
    if x >= dw || y >= dh {
        return;
    }
    let n = sw.min(dw - x);
    for r in 0..sh.min(dh - y) {
        dst[(y + r) * dw + x..(y + r) * dw + x + n].copy_from_slice(&src[r * sw..r * sw + n]);
    }
}

/// Planos vacíos para una imagen de `w × h` con el espacio de color `kind`.
fn empty_planes(kind: (bool, bool), w: usize, h: usize) -> RefPlanes {
    let (cw, ch) = if kind.1 {
        (w.div_ceil(2), h.div_ceil(2))
    } else {
        (w, h)
    };
    vec![
        (vec![0; w * h], w, h),
        (vec![0; cw * ch], cw, ch),
        (vec![0; cw * ch], cw, ch),
    ]
}

/// Pega los planos de una tesela (o de un fotograma) que empieza en (x, y).
fn paste_planes(dst: &mut RefPlanes, src: &RefPlanes, x: usize, y: usize, sub: bool) {
    for (pi, (d, s)) in dst.iter_mut().zip(src).enumerate() {
        let k = if pi > 0 && sub { 2 } else { 1 };
        paste_plane(&mut d.0, d.1, d.2, x / k, y / k, &s.0, s.1, s.2);
    }
}

/// Estado de una animación mientras se decodifica (o se codifica): el lienzo
/// RGBA que se muestra y los planos de referencia para predecir el siguiente
/// fotograma (sección 1.3 de la especificación v2).
#[derive(Clone)]
pub struct AnimState {
    pub canvas: Vec<u8>,
    pub width: usize,
    pub height: usize,
    refp: Option<((bool, bool), RefPlanes)>,
    /// Referencia larga (fotograma dorado): planos y su espacio de color.
    gold: Option<((bool, bool), RefPlanes)>,
    /// ¿El último fotograma pasó a ser la referencia larga?
    last_gold: bool,
}

impl AnimState {
    pub fn new(width: usize, height: usize) -> Self {
        AnimState {
            canvas: vec![0; width * height * 4],
            width,
            height,
            refp: None,
            gold: None,
            last_gold: false,
        }
    }

    /// Planos de la referencia larga si están en el espacio de color de `h`.
    fn gold_for(&self, h: &Header) -> Option<RefPlanes> {
        self.gold
            .as_ref()
            .filter(|g| g.0 == ref_kind(h))
            .map(|g| g.1.clone())
    }

    /// Antes de un fotograma: si los planos de referencia no están en su
    /// espacio de color, se recalculan a partir del lienzo.
    fn prepare(&mut self, h: &Header) {
        let kind = ref_kind(h);
        if self.refp.as_ref().map(|r| r.0) != Some(kind) {
            let rgb: Vec<u8> = self
                .canvas
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            self.refp = Some((kind, planes_u8(&rgb, self.width, self.height, h)));
        }
    }

    /// Tras un fotograma: lo pega en el lienzo y sus planos en los de
    /// referencia; si `f.gold`, los de referencia pasan a ser también la
    /// referencia larga.
    fn commit(&mut self, f: &FrameInfo, d: &Decoded, planes: &RefPlanes) {
        compose(&mut self.canvas, self.width, d, f);
        if let Some((kind, rp)) = self.refp.as_mut() {
            if kind.1 && (f.x | f.y) & 1 != 0 {
                // La croma no queda alineada: se recalculará del lienzo.
                self.refp = None;
            } else {
                paste_planes(rp, planes, f.x, f.y, kind.1);
            }
        }
        if f.gold {
            self.gold = self.refp.clone();
        }
        self.last_gold = f.gold;
    }
}

impl RefCanvas {
    /// `planes`: planos de referencia del lienzo (w × h). `src`: lienzo
    /// anterior original (solo el codificador, para buscar coincidencias exactas).
    fn new(
        planes: RefPlanes,
        src: Option<&[u8]>,
        w: usize,
        h: usize,
        header: &Header,
        x: usize,
        y: usize,
    ) -> Self {
        let hashes = match src {
            Some(s) => color_planes(s, w, h, header)
                .into_iter()
                .map(|(p, pw, ph)| Some(tools::HashIndex::build(&p, pw, ph)))
                .collect(),
            None => vec![None, None, None],
        };
        RefCanvas {
            planes,
            hashes,
            x,
            y,
            sub: header.subsample && header.tools & TOOL_LOSSLESS == 0,
            warp: None,
            tdq: None,
            gold: None,
        }
    }

    /// Plano `pi` para la tesela que empieza en (tx, ty) dentro del fotograma.
    fn plane(&self, pi: usize, tx: usize, ty: usize) -> InterPlane<'_> {
        let d = if pi > 0 && self.sub { 2 } else { 1 };
        let (data, w, h) = &self.planes[pi];
        InterPlane {
            data,
            w: *w,
            h: *h,
            ox: (self.x + tx) / d,
            oy: (self.y + ty) / d,
            hash: self.hashes[pi].as_ref(),
            d,
            warp: self.warp,
            gdata: self.gold.as_ref().map(|g| &g[pi].0[..]),
        }
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
    /// Tamaño visible del plano: lo que queda fuera (relleno hasta el
    /// superbloque) no se ve, así que su distorsión no cuenta.
    vw: usize,
    vh: usize,
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
    /// La tesela lleva movimiento global.
    warp: bool,
}

/// Fuente de un bloque con lo que queda fuera de la parte visible (`vis`)
/// sustituido por la predicción.
fn mask_src(src: &[f32], pred: &[i32], w: usize, h: usize, vis: (usize, usize)) -> Vec<f32> {
    let mut out = src.to_vec();
    for y in 0..h {
        for x in 0..w {
            if x >= vis.0 || y >= vis.1 {
                out[y * w + x] = pred[y * w + x] as f32;
            }
        }
    }
    out
}

fn zero_outside(res: &mut [f32], w: usize, h: usize, vis: (usize, usize)) {
    for y in 0..h {
        for x in 0..w {
            if x >= vis.0 || y >= vis.1 {
                res[y * w + x] = 0.0;
            }
        }
    }
}

/// SAD solo sobre la parte visible del bloque.
fn sad_vis(src: &[f32], pred: &[i32], w: usize, _h: usize, vis: (usize, usize)) -> f32 {
    let mut s = 0.0;
    for y in 0..vis.1 {
        for x in 0..vis.0 {
            s += (src[y * w + x] - pred[y * w + x] as f32).abs();
        }
    }
    s
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
            warp: self.warp,
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
            self.warp,
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
        vis: (usize, usize),
    ) -> (f32, Vec<i32>, bool) {
        let nn = w * h;
        // Fuera de la parte visible, la fuente pasa a ser la predicción:
        // residuo nulo y sin distorsión.
        let masked;
        let src = if vis == (w, h) {
            src
        } else {
            masked = mask_src(src, pred, w, h, vis);
            &masked[..]
        };
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
        let vis = (
            pe.vw.saturating_sub(x0).min(w),
            pe.vh.saturating_sub(y0).min(h),
        );
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
        if let Some(ip) = pe.inter {
            // Predicción desde el fotograma anterior: vectores candidatos (el
            // nulo, el último, coincidencias exactas del hash y una búsqueda
            // por pasos alrededor del mejor), ordenados por SAD + bits.
            let mvp = mv_pred(&pe.plane, x0, y0, w, pe.sc.last_mv);
            let mut seen: Vec<(i32, i32)> = Vec::new();
            let mut scored: Vec<(f32, (i32, i32))> = Vec::new();
            let mut score = |mv: (i32, i32),
                             this: &mut Self,
                             scored: &mut Vec<(f32, (i32, i32))>,
                             pred: &mut [i32]| {
                if seen.contains(&mv) || mv.0.abs() > 16384 || mv.1.abs() > 16384 {
                    return f32::INFINITY;
                }
                seen.push(mv);
                ip.predict(x0, y0, w, h, mv, pred);
                let sad = sad_vis(&src, pred, w, h, vis);
                let s = sad + slam * this.ctx.mv.cost(pc, mv, mvp);
                scored.push((s, mv));
                s
            };
            // Vectores en cuartos de píxel.
            let mut start = vec![mvp, (0, 0), pe.sc.last_mv];
            if ip.warp.is_some() {
                start.push(ip.warp_mv(x0 + w / 2 - 2, y0 + h / 2 - 2));
            }
            let (xi, yi) = (x0 as isize, y0 as isize);
            for (nx, ny) in [(xi - 1, yi), (xi, yi - 1), (xi - 1, yi - 1)] {
                if let Some(v) = inter_mv_at(&pe.plane, nx, ny) {
                    start.push(v);
                }
            }
            if let Some(hi) = ip.hash.filter(|_| w.min(h) >= 8) {
                for c in hi
                    .candidates(pe.src, pe.plane.w, x0, y0)
                    .into_iter()
                    .take(8)
                {
                    start.push((4 * (c.0 - ip.ox as i32), 4 * (c.1 - ip.oy as i32)));
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
                // Píxeles enteros (8, 4, 2, 1) y después medio y cuarto de píxel.
                for step in [32, 16, 8, 4, 2, 1] {
                    let mut improved = true;
                    while improved {
                        improved = false;
                        let c = best_mv.1;
                        for (dx, dy) in [
                            (step, 0),
                            (-step, 0),
                            (0, step),
                            (0, -step),
                            (step, step),
                            (-step, -step),
                            (step, -step),
                            (-step, step),
                        ] {
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
                let (jr, lv, ts) =
                    self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true, vis);
                let kind = Kind::Inter(mv);
                let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                consider(&mut best, jr + lambda * hb, kind, lv, ts);
            }
            if self.warp {
                ip.predict_warp(x0, y0, w, h, &mut pred);
                let (jr, lv, ts) =
                    self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true, vis);
                let kind = Kind::Warp;
                let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                consider(&mut best, jr + lambda * hb, kind, lv, ts);
            }
        }
        // Si copiar del fotograma anterior ya es exacto (sin residuo), no se
        // prueban la predicción intra ni las herramientas sintéticas.
        let skip_intra =
            self.effort < 9 && best.as_ref().is_some_and(|b| b.2.iter().all(|&l| l == 0));
        if !skip_intra {
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
                    if vis != (w, h) {
                        zero_outside(&mut res, w, h, vis);
                    }
                    let mut cc = CostCounter::default();
                    code_mode(&mut self.ctx, &mut cc, pc, n, mpm, mode);
                    let s = dct::satd_rect(&res, w, h);
                    cands.push((s + slam * cc.bits, mode, cc.bits));
                }
            }
            cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

            // Solo pasan a RDO completo los modos cuyo coste SATD está cerca del mejor.
            let satd_limit = cands[0].0 * 1.3 + slam * 4.0;
            for &(sc, mode, _) in cands.iter().take(self.topk) {
                if sc > satd_limit {
                    break;
                }
                predict_mode(&pe.plane, &refs, lref, mode, x0, y0, w, h, &mut pred);
                let (jr, lv, ts) =
                    self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true, vis);
                let kind = Kind::Intra(mode);
                let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                consider(&mut best, jr + lambda * hb, kind, lv, ts);
            }
            if self.screen && n <= 32 {
                // Paleta
                for pal in tools::palette_candidates(&src, w, h) {
                    tools::predict_palette(&pal, &mut pred);
                    let (jr, lv, ts) =
                        self.rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true, vis);
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
                        let sad = sad_vis(&src, &pred, w, h, vis);
                        let bits = self.ctx.bv.cost(pc, bv, pe.sc.last_bv);
                        scored.push((sad + slam * bits, bv));
                    }
                    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                    for &(_, bv) in scored.iter().take(2) {
                        tools::predict_ibc(&pe.plane, x0, y0, w, h, bv, &mut pred);
                        let (jr, lv, ts) = self
                            .rd_residual(pc, w, h, cctx, &src, &pred, q, lambda, try_ts, true, vis);
                        let kind = Kind::Ibc(bv);
                        let hb = self.head_bits(pe, x0, y0, w, h, &kind, ts);
                        consider(&mut best, jr + lambda * hb, kind, lv, ts);
                    }
                }
            }
        }
        let (j, kind, levels, ts) = best.expect("al menos un candidato");
        predict_leaf(
            &pe.plane,
            &refs,
            lref,
            &kind,
            x0,
            y0,
            w,
            h,
            pe.inter.as_ref(),
            &mut pred,
        );
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
        match (&ev.leaf.kind, pe.inter.as_ref()) {
            (Kind::Inter(mv), _) => pe.plane.set_mv(x0, y0, w, h, *mv),
            (Kind::Warp, Some(ip)) => ip.set_warp_mvs(&mut pe.plane, x0, y0, w, h),
            _ => {}
        }
        match &ev.leaf.kind {
            Kind::Palette(p) => pe.sc.last_pal = Some(p.colors.clone()),
            Kind::Ibc(bv) => pe.sc.last_bv = *bv,
            Kind::Inter(mv) => pe.sc.last_mv = *mv,
            Kind::Intra(_) | Kind::Warp => {}
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
            self.warp,
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

/// v2, fotogramas predichos: dq de cada celda AQ deducido de la actividad de
/// la referencia en la posición de la tesela (`w × h`, rellenada hasta el
/// superbloque repitiendo el borde), en enteros para que codificador y
/// decodificador coincidan bit a bit. Por bloque de 8 × 8, la varianza
/// v = (64·Σx² − (Σx)²) / 4096 y su logaritmo en 1/16: con n = ⌊log2(1 + v)⌋,
/// l = 16n + ((1 + v)·16 >> n) − 16. La actividad de la celda es la media de
/// sus l; el dq, la diferencia con la media de las celdas, en dieciseisavos,
/// redondeada y recortada a ±8.
fn aq_from_ref(ip: &InterPlane, w: usize, h: usize) -> Vec<i32> {
    let (pw, ph) = (w.div_ceil(SB) * SB, h.div_ceil(SB) * SB);
    let (sw, sh) = (pw / AQ, ph / AQ);
    let (rw, rh) = (ip.w as i64, ip.h as i64);
    let px = |x: usize, y: usize| -> i64 {
        let xx = (ip.ox + x.min(w - 1)) as i64;
        let yy = (ip.oy + y.min(h - 1)) as i64;
        ip.data[yy.clamp(0, rh - 1) as usize * ip.w + xx.clamp(0, rw - 1) as usize] as i64
    };
    let k = AQ / 8;
    let mut act = vec![0i64; sw * sh];
    for sy in 0..sh {
        for sx in 0..sw {
            let mut tot = 0i64;
            for by in 0..k {
                for bx in 0..k {
                    let (x0, y0) = (sx * AQ + bx * 8, sy * AQ + by * 8);
                    let (mut s1, mut s2) = (0i64, 0i64);
                    for y in 0..8 {
                        for x in 0..8 {
                            let v = px(x0 + x, y0 + y);
                            s1 += v;
                            s2 += v * v;
                        }
                    }
                    let v = ((64 * s2 - s1 * s1) / 4096) as u64 + 1;
                    let n = 63 - v.leading_zeros() as i64;
                    tot += 16 * n + (((v << 4) >> n) as i64) - 16;
                }
            }
            act[sy * sw + sx] = tot / (k * k) as i64;
        }
    }
    let mean = act.iter().sum::<i64>() / act.len() as i64;
    act.iter()
        .map(|&a| {
            let d = a - mean;
            ((d + 8 * d.signum()) / 16).clamp(-8, 8) as i32
        })
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
    let meta = &meta.without_srgb();
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

/// Fotograma de una animación: el estado (lienzo y planos de referencia), el
/// lienzo anterior original (RGB, para buscar coincidencias), el rectángulo
/// del fotograma y si predice del anterior.
struct AnimFrame<'a> {
    st: &'a mut AnimState,
    src: &'a [u8],
    f: FrameInfo,
    inter: bool,
    /// Movimiento global candidato respecto al anterior y a la referencia larga.
    warps: [Option<Warp>; 2],
    tdq: Option<(Vec<i32>, usize)>,
}

fn encode_with_alpha_ref(
    rgb: &[u8],
    alpha: Option<&[u8]>,
    width: usize,
    height: usize,
    p: &Params,
    mut anim: Option<AnimFrame>,
) -> Encoded {
    let mut header = make_header(rgb, width, height, p);
    if alpha.is_some() {
        header.tools |= TOOL_ALPHA;
    }
    let mut rc = None;
    if let Some(a) = anim.as_mut() {
        a.st.prepare(&header);
        if a.inter {
            header.tools |= TOOL_INTER;
            let planes = a.st.refp.as_ref().unwrap().1.clone();
            let (cw, ch) = (a.st.width, a.st.height);
            let mut r = RefCanvas::new(planes, Some(a.src), cw, ch, &header, a.f.x, a.f.y);
            r.gold = a.st.gold_for(&header);
            let (warp, stale) = choose_warp(&r, rgb, width, height, &header, a.warps);
            r.warp = warp;
            // Si la referencia larga ya predice claramente peor que el
            // fotograma anterior (o no hay movimiento común con ella), este
            // fotograma la sustituye.
            a.f.gold = stale;
            r.tdq = a.tdq.take();
            if r.tdq.is_some() {
                header.aq = true;
            }
            rc = Some(r);
        }
    }
    let want = anim.is_some();
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
            want,
        );
        (r.0, r.1, r.2, r.4)
    });
    let mut recon_rgb = vec![0u8; width * height * 3];
    let mut recon_alpha = alpha.map(|_| vec![0u8; width * height]);
    let kind = ref_kind(&header);
    let mut fplanes = if want {
        empty_planes(kind, width, height)
    } else {
        Vec::new()
    };
    for (t, r) in tiles.iter().zip(&res) {
        paste(&mut recon_rgb, 3, width, t, &r.1);
        if let (Some(dst), Some(src)) = (recon_alpha.as_mut(), r.2.as_ref()) {
            paste(dst, 1, width, t, src);
        }
        if want {
            paste_planes(&mut fplanes, &r.3, t.x, t.y, kind.1);
        }
    }
    if let Some(a) = anim {
        let d = Decoded {
            width,
            height,
            rgb: recon_rgb.clone(),
            alpha: recon_alpha.clone(),
            meta: Metadata::default(),
        };
        a.st.commit(&a.f, &d, &fplanes);
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
                    let i = cy * qm_ro.w + cx;
                    let p = qm_ro.pred.get(i).copied().unwrap_or(0);
                    ctx.dq.code(enc, qm_ro.dq[i] - p);
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
            vw: if base.v2 { w } else { pw },
            vh: if base.v2 { h } else { ph },
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
        pred: Vec::new(),
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
    let r = encode_tile_ref(rgb, alpha, width, height, header, p, None, false);
    (r.0, r.1, r.2, r.3)
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
    want_planes: bool,
) -> (
    Vec<u8>,
    Vec<u8>,
    Option<Vec<u8>>,
    Vec<(u32, u32)>,
    RefPlanes,
) {
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
        warp: false,
    };
    let mut enc = Encoder::new();
    if let Some((rc, _, _)) = iref {
        st.warp = Warp::code(&mut enc, rc.warp).is_some();
    }
    let mut planes: Vec<Plane> = Vec::new();
    let mut qm = QMap {
        dq: Vec::new(),
        w: 0,
        h: 0,
        base: base_q,
        cq,
        sub,
        pred: Vec::new(),
    };
    let mut lref: Option<LumaRef> = None;
    let mut srcs = Vec::new();
    let mut vis = Vec::new();
    for (pi, (src, w, h)) in color_planes(rgb, width, height, header).iter().enumerate() {
        vis.push((*w, *h));
        let pc = (pi > 0) as usize;
        if pi == 0 {
            let (padded, pw, ph) = pad_plane(src, *w, *h);
            qm.dq = compute_aq(&padded, pw, ph, if header.aq { p.aq_strength } else { 0.0 });
            qm.w = pw / AQ;
            qm.h = ph / AQ;
            if let Some((rc, tx, ty)) = iref.filter(|_| header.aq) {
                // Fotograma predicho: el mapa se deduce de la referencia (el
                // decodificador hace lo mismo) y no cuesta bits.
                qm.pred = aq_from_ref(&rc.plane(0, tx, ty), *w, *h);
                qm.dq = qm.pred.clone();
                if let Some((t, tw)) = rc.tdq.as_ref() {
                    let th = t.len() / tw;
                    for cy in 0..qm.h {
                        for cx in 0..qm.w {
                            let (fx, fy) = (tx / AQ + cx, ty / AQ + cy);
                            if fx < *tw && fy < th {
                                let v = &mut qm.dq[cy * qm.w + cx];
                                *v = (*v + t[fy * tw + fx]).clamp(-64, 64);
                            }
                        }
                    }
                }
            }
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
    if header.version >= 2 {
        for (plane, &(vw, vh)) in planes.iter_mut().zip(&vis) {
            repad(plane, vw, vh);
        }
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
            warp: false,
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
    let (recon, fp) = finish_planes(header, &planes, width, height, want_planes);
    (enc.finish(), recon, recon_alpha, stats, fp)
}

/// v2: tras decodificar un plano, lo que queda fuera de la parte visible
/// (relleno hasta el superbloque) se sustituye repitiendo el borde, antes de
/// la restauración. Así el codificador puede dejar ahí cualquier cosa.
fn repad(p: &mut Plane, vw: usize, vh: usize) {
    let (pw, ph) = (p.w, p.h);
    if vw == 0 || vh == 0 || (vw >= pw && vh >= ph) {
        return;
    }
    for y in 0..vh.min(ph) {
        let v = p.data[y * pw + vw - 1];
        p.data[y * pw + vw..y * pw + pw].fill(v);
    }
    for y in vh..ph {
        p.data.copy_within((vh - 1) * pw..vh * pw, y * pw);
    }
}

fn crop_plane(p: &Plane, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        out.extend_from_slice(&p.data[y * p.w..y * p.w + w]);
    }
    out
}

/// Planos finales (tras el desbloqueo) recortados a la imagen: la referencia
/// del fotograma siguiente en una animación.
fn final_planes(h: &Header, planes: &[Plane], w: usize, ht: usize) -> RefPlanes {
    let sub = ref_kind(h).1;
    planes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (pw, ph) = if i > 0 && sub {
                (w.div_ceil(2), ht.div_ceil(2))
            } else {
                (w, ht)
            };
            (crop_plane(p, pw, ph), pw, ph)
        })
        .collect()
}

/// RGB de la tesela y, si `want`, sus planos finales.
fn finish_planes(
    h: &Header,
    planes: &[Plane],
    w: usize,
    ht: usize,
    want: bool,
) -> (Vec<u8>, RefPlanes) {
    if h.tools & TOOL_LOSSLESS != 0 {
        let (g, b, r) = (&planes[0], &planes[1], &planes[2]);
        let mut out = Vec::with_capacity(w * ht * 3);
        for y in 0..ht {
            for x in 0..w {
                out.extend_from_slice(&[r.px(x, y), g.px(x, y), b.px(x, y)]);
            }
        }
        let fp = if want {
            final_planes(h, planes, w, ht)
        } else {
            Vec::new()
        };
        return (out, fp);
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
    let rgb = color::ycbcr_to_rgb(
        &ps[0].data,
        ps[0].w,
        &ps[1].data,
        &ps[2].data,
        ps[1].w,
        w,
        ht,
        h.subsample,
    );
    let fp = if want {
        final_planes(h, &ps, w, ht)
    } else {
        Vec::new()
    };
    (rgb, fp)
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
    /// Movimiento global de la tesela.
    warp: bool,
    gw: Option<Warp>,
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
            self.warp,
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
        let ip = self.rc.as_ref().map(|rc| InterPlane {
            warp: self.gw,
            ..rc.plane(pi, tx, ty)
        });
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
        match (&kind, ip.as_ref()) {
            (Kind::Inter(mv), _) => p.set_mv(x0, y0, w, h, *mv),
            (Kind::Warp, Some(ip)) => ip.set_warp_mvs(p, x0, y0, w, h),
            _ => {}
        }
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
    let r = decode_tile_ref(payload, w, ht, h, None, 0, 0, false);
    (r.0, r.1)
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
    want_planes: bool,
) -> (Vec<u8>, Option<Vec<u8>>, RefPlanes) {
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
        warp: false,
        gw: None,
        rc,
        cur: (0, tx, ty),
    };
    let mut dec = Decoder::new(payload);
    if st.inter {
        st.gw = Warp::code(&mut dec, None);
        st.warp = st.gw.is_some();
    }
    let mut planes = Vec::new();
    let mut qm = QMap {
        dq: Vec::new(),
        w: 0,
        h: 0,
        base: base_q,
        cq,
        sub,
        pred: Vec::new(),
    };
    let mut lref: Option<LumaRef> = None;
    for (pi, &(pw0, ph0)) in dims.iter().enumerate() {
        if pi == 0 {
            qm.w = pw0.div_ceil(SB) * SB / AQ;
            qm.h = ph0.div_ceil(SB) * SB / AQ;
            qm.dq = vec![0; qm.w * qm.h];
            if let (true, Some(rc)) = (h.aq, st.rc.as_ref()) {
                qm.pred = aq_from_ref(&rc.plane(0, tx, ty), pw0, ph0);
            }
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
    if h.version >= 2 {
        for (plane, &(vw, vh)) in planes.iter_mut().zip(&dims) {
            repad(plane, vw, vh);
        }
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
            warp: false,
            gw: None,
            rc: None,
            cur: (0, 0, 0),
        };
        let mut aq = unit_qmap(w, ht);
        let plane = decode_plane(&mut ast, &mut dec, w, ht, 0, None, &mut aq, false);
        crop_plane(&plane, w, ht)
    });
    let (rgb, fp) = finish_planes(h, &planes, w, ht, want_planes);
    (rgb, alpha, fp)
}

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    // Una animación se decodifica como su primer fotograma (que cubre todo el lienzo).
    if let Some(a) = anim_info(data)? {
        let d = decode_frame(data, &a.frames[0], None)?;
        return Ok(Decoded { meta: a.meta, ..d });
    }
    decode_with_ref(data, None, false).map(|r| r.0)
}

/// Decodifica un archivo fijo; si es un fotograma con predicción entre
/// fotogramas, necesita el fotograma anterior (`rc`).
fn decode_with_ref(
    data: &[u8],
    rc: Option<std::sync::Arc<RefCanvas>>,
    want_planes: bool,
) -> Result<(Decoded, RefPlanes), String> {
    let inf = info(data)?;
    if inf.header.tools & TOOL_INTER != 0 && rc.is_none() {
        return Err("fotograma que depende del anterior: decodifica la animación completa".into());
    }
    let (w, ht) = (inf.header.width as usize, inf.header.height as usize);
    let idx: Vec<usize> = (0..inf.tiles.len()).collect();
    let parts = par_map(&idx, |&i| {
        let t = inf.tiles[i];
        decode_tile_ref(
            &data[inf.ranges[i].clone()],
            t.w,
            t.h,
            &inf.header,
            rc.clone(),
            t.x,
            t.y,
            want_planes,
        )
    });
    let mut rgb = vec![0u8; w * ht * 3];
    let mut alpha = (inf.header.tools & TOOL_ALPHA != 0).then(|| vec![255u8; w * ht]);
    let kind = ref_kind(&inf.header);
    let mut fplanes = if want_planes {
        empty_planes(kind, w, ht)
    } else {
        Vec::new()
    };
    for (t, (prgb, palpha, pp)) in inf.tiles.iter().zip(&parts) {
        paste(&mut rgb, 3, w, t, prgb);
        if let (Some(dst), Some(src)) = (alpha.as_mut(), palpha.as_ref()) {
            paste(dst, 1, w, t, src);
        }
        if want_planes {
            paste_planes(&mut fplanes, pp, t.x, t.y, kind.1);
        }
    }
    let d = Decoded {
        width: w,
        height: ht,
        rgb,
        alpha,
        meta: inf.meta.clone(),
    };
    Ok((d, fplanes))
}

// ------------------------------------------------------------- Animación

/// Un fotograma de una animación: cuánto dura y qué rectángulo del lienzo
/// sustituye. Sus datos son un archivo `.tsl` de ese rectángulo: `prefix`
/// seguido de `data[range]` (en la forma abreviada, `prefix` es la cabecera
/// y la tabla de teselas reconstruidas; en la completa, va vacío).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameInfo {
    pub duration_ms: u32,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub range: std::ops::Range<usize>,
    pub prefix: Vec<u8>,
    /// Tras decodificarlo, pasa a ser la referencia larga (siempre en el primero).
    pub gold: bool,
}

impl FrameInfo {
    /// El archivo `.tsl` del rectángulo.
    fn file<'a>(&self, data: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        if self.prefix.is_empty() {
            std::borrow::Cow::Borrowed(&data[self.range.clone()])
        } else {
            let mut v = self.prefix.clone();
            v.extend_from_slice(&data[self.range.clone()]);
            std::borrow::Cow::Owned(v)
        }
    }
}

/// Entero sin signo LEB128 (7 bits por byte, el bit alto indica que sigue).
fn write_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<usize, String> {
    let mut v: u64 = 0;
    for i in 0..5 {
        let b = *data.get(*pos).ok_or("archivo truncado")?;
        *pos += 1;
        v |= ((b & 0x7F) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return u32::try_from(v)
                .map(|v| v as usize)
                .map_err(|_| "entero demasiado grande".into());
        }
    }
    Err("entero demasiado largo".into())
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
/// `u16 repeticiones | u16 reservado (0) | u32 n` y, por fotograma, con
/// enteros LEB128 (v): `v duración (ms) | v x | v y | v ancho | v alto | u8 forma`.
/// Forma 0: `v longitud | archivo .tsl completo`. Forma 1 (abreviada): `u8 tools
/// | v longitud de cada tesela | datos de las teselas`, con la cabecera del
/// fotograma anterior salvo el ancho, el alto y `tools`.
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
    // Cada fotograma ocupa al menos 8 bytes: n no puede ser absurdo.
    if n == 0 || n > (data.len() - pos.min(data.len())) / 8 {
        return Err("animación no válida".into());
    }
    let mut frames = Vec::with_capacity(n);
    let mut prev: Option<Header> = None;
    for i in 0..n {
        let mut f = [0usize; 5];
        for v in f.iter_mut() {
            *v = read_varint(data, &mut pos)?;
        }
        let [dur, fx, fy, fw, fh] = f;
        let full = fx == 0 && fy == 0 && fw == w && fh == h;
        if fw == 0 || fh == 0 || fx + fw > w || fy + fh > h || (i == 0 && !full) {
            return Err("fotograma fuera del lienzo".into());
        }
        let form = *data.get(pos).ok_or("archivo truncado")?;
        pos += 1;
        if form & !3 != 0 {
            return Err("forma de fotograma desconocida".into());
        }
        let gold = i == 0 || form & 2 != 0;
        let (range, prefix, hdr) = match form & 1 {
            0 => {
                let len = read_varint(data, &mut pos)?;
                let end = pos
                    .checked_add(len)
                    .filter(|&e| e <= data.len())
                    .ok_or("archivo truncado")?;
                let hdr = Header::read(&data[pos..end])?;
                (pos..end, Vec::new(), hdr)
            }
            1 => {
                let p = prev.as_ref().ok_or("animación no válida")?;
                let tools = *data.get(pos).ok_or("archivo truncado")?;
                pos += 1;
                if tools & (TOOL_PREVIEW | TOOL_META | TOOL_ANIM) != 0 {
                    return Err("fotograma abreviado no válido".into());
                }
                let mut hdr = Header {
                    width: fw as u32,
                    height: fh as u32,
                    tools,
                    ..p.clone()
                };
                let mut prefix = Vec::new();
                hdr.write(&mut prefix);
                // Valida la cabecera reconstruida como cualquier otra.
                hdr = Header::read(&prefix)?;
                let ntiles = tile_grid(fw, fh, hdr.tile_size()).len();
                let mut total = 0usize;
                for _ in 0..ntiles {
                    let l = read_varint(data, &mut pos)?;
                    total = total.checked_add(l).ok_or("archivo truncado")?;
                    prefix.extend_from_slice(&(l as u32).to_le_bytes());
                }
                let end = pos
                    .checked_add(total)
                    .filter(|&e| e <= data.len())
                    .ok_or("archivo truncado")?;
                (pos..end, prefix, hdr)
            }
            _ => return Err("forma de fotograma desconocida".into()),
        };
        pos = range.end;
        prev = Some(hdr);
        frames.push(FrameInfo {
            duration_ms: dur as u32,
            x: fx,
            y: fy,
            w: fw,
            h: fh,
            range,
            prefix,
            gold,
        });
    }
    Ok(Some(AnimInfo {
        width: w,
        height: h,
        loops,
        frames,
        meta,
    }))
}

/// Decodifica el rectángulo de un fotograma. Con `st` (el estado de la
/// animación hasta el fotograma anterior), además lo compone en el lienzo y
/// actualiza la referencia; sin él solo se pueden decodificar fotogramas que
/// no predicen del anterior.
pub fn decode_frame(
    data: &[u8],
    f: &FrameInfo,
    st: Option<&mut AnimState>,
) -> Result<Decoded, String> {
    let file = f.file(data);
    let inner = &file[..];
    let h = Header::read(inner)?;
    if h.tools & TOOL_ANIM != 0 || h.width as usize != f.w || h.height as usize != f.h {
        return Err("fotograma no válido".into());
    }
    let inter = h.tools & TOOL_INTER != 0;
    let Some(st) = st else {
        if inter {
            return Err("fotograma que depende del anterior".into());
        }
        return decode_with_ref(inner, None, false).map(|r| r.0);
    };
    if inter && h.subsample && (f.x | f.y) & 1 != 0 {
        return Err("fotograma con predicción en coordenadas impares".into());
    }
    st.prepare(&h);
    let rc = inter.then(|| {
        let planes = st.refp.as_ref().unwrap().1.clone();
        let mut r = RefCanvas::new(planes, None, st.width, st.height, &h, f.x, f.y);
        r.gold = st.gold_for(&h);
        std::sync::Arc::new(r)
    });
    let (d, planes) = decode_with_ref(inner, rc, true)?;
    st.commit(f, &d, &planes);
    Ok(d)
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
    let mut st = AnimState::new(a.width, a.height);
    let mut out = Vec::with_capacity(a.frames.len());
    for f in &a.frames {
        decode_frame(data, f, Some(&mut st))?;
        out.push((st.canvas.clone(), f.duration_ms));
    }
    Ok((a, out))
}

/// Estima el movimiento global (afín) entre dos lienzos RGBA de `cw × ch`
/// dentro del rectángulo `r`: busca cada bloque de una rejilla en el
/// fotograma anterior y ajusta el modelo por mínimos cuadrados, descartando
/// los bloques que no lo siguen. None si no hay un movimiento común claro.
fn estimate_warp(
    prev: &[u8],
    cur: &[u8],
    cw: usize,
    ch: usize,
    r: (usize, usize, usize, usize),
) -> Option<Warp> {
    const B: usize = 16;
    const R: i64 = 24;
    let luma = |img: &[u8]| -> Vec<i32> {
        img.chunks_exact(4)
            .map(|p| (77 * p[0] as i32 + 150 * p[1] as i32 + 29 * p[2] as i32) >> 8)
            .collect()
    };
    let (lp, lc) = (luma(prev), luma(cur));
    let (rx, ry, rw, rh) = r;
    if rw < 2 * B || rh < 2 * B {
        return None;
    }
    // Rejilla de como mucho ~24 × 24 bloques.
    let step = |n: usize| (n / 24).max(B);
    let at = |img: &[i32], x: i64, y: i64| {
        img[y.clamp(0, ch as i64 - 1) as usize * cw + x.clamp(0, cw as i64 - 1) as usize]
    };
    let mut pts: Vec<(f64, f64, f64, f64)> = Vec::new();
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut by = ry;
    while by + B <= ry + rh {
        let mut bx = rx;
        while bx + B <= rx + rw {
            // Bloques con textura (si no, cualquier vector vale).
            let mut grad = 0i64;
            for y in by..by + B {
                for x in bx + 1..bx + B {
                    grad += (lc[y * cw + x] - lc[y * cw + x - 1]).abs() as i64;
                }
            }
            if grad >= (B * B) as i64 * 3 {
                let sad = |dx: i64, dy: i64, lim: i64| {
                    let mut s = 0i64;
                    for y in 0..B as i64 {
                        for x in 0..B as i64 {
                            let (cx, cy) = (bx as i64 + x, by as i64 + y);
                            s += (lc[cy as usize * cw + cx as usize] - at(&lp, cx + dx, cy + dy))
                                .abs() as i64;
                        }
                        if s >= lim {
                            return s;
                        }
                    }
                    s
                };
                let mut best = (sad(0, 0, i64::MAX), 0i64, 0i64);
                for dy in -R..=R {
                    for dx in -R..=R {
                        let v = sad(dx, dy, best.0);
                        if v < best.0 {
                            best = (v, dx, dy);
                        }
                    }
                }
                let (_, dx, dy) = best;
                // Subpíxel: parábola por los vecinos.
                let par = |a: i64, b: i64, c: i64| {
                    let den = (a - 2 * b + c) as f64;
                    if den > 0.0 {
                        (0.5 * (a - c) as f64 / den).clamp(-0.5, 0.5)
                    } else {
                        0.0
                    }
                };
                let c0 = best.0;
                let fx = par(sad(dx - 1, dy, i64::MAX), c0, sad(dx + 1, dy, i64::MAX));
                let fy = par(sad(dx, dy - 1, i64::MAX), c0, sad(dx, dy + 1, i64::MAX));
                let (px, py) = (bx as f64 + B as f64 / 2.0, by as f64 + B as f64 / 2.0);
                pts.push((px, py, dx as f64 + fx, dy as f64 + fy));
                blocks.push((bx, by));
            }
            bx += step(rw);
        }
        by += step(rh);
    }
    if pts.len() < 8 {
        return None;
    }
    // Mínimos cuadrados (x, y, 1) → desplazamiento, con descarte iterativo.
    let fit = |pts: &[&(f64, f64, f64, f64)]| -> Option<[f64; 6]> {
        let mut m = [[0f64; 3]; 3];
        let (mut vx, mut vy) = ([0f64; 3], [0f64; 3]);
        for p in pts {
            let f = [p.0, p.1, 1.0];
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] += f[i] * f[j];
                }
                vx[i] += f[i] * p.2;
                vy[i] += f[i] * p.3;
            }
        }
        let solve = |m: [[f64; 3]; 3], v: [f64; 3]| -> Option<[f64; 3]> {
            let det = |m: [[f64; 3]; 3]| {
                m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                    - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                    + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
            };
            let d = det(m);
            if d.abs() < 1e-9 {
                return None;
            }
            let mut out = [0f64; 3];
            for (k, o) in out.iter_mut().enumerate() {
                let mut mk = m;
                for i in 0..3 {
                    mk[i][k] = v[i];
                }
                *o = det(mk) / d;
            }
            Some(out)
        };
        let a = solve(m, vx)?;
        let b = solve(m, vy)?;
        Some([a[0], a[1], a[2], b[0], b[1], b[2]])
    };
    let mut inl: Vec<&(f64, f64, f64, f64)> = pts.iter().collect();
    let mut model = fit(&inl)?;
    for _ in 0..4 {
        let err = |p: &(f64, f64, f64, f64)| {
            let ex = model[0] * p.0 + model[1] * p.1 + model[2] - p.2;
            let ey = model[3] * p.0 + model[4] * p.1 + model[5] - p.3;
            (ex * ex + ey * ey).sqrt()
        };
        let mut errs: Vec<f64> = pts.iter().map(err).collect();
        errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let th = (2.0 * errs[errs.len() / 2]).max(0.35);
        inl = pts.iter().filter(|p| err(p) <= th).collect();
        if inl.len() < 8 {
            return None;
        }
        model = fit(&inl)?;
    }
    if inl.len() * 10 < pts.len() * 4 {
        return None;
    }
    // Candidatos: el afín, solo traslación y traslación entera. Se mide el
    // error real de la predicción en los bloques con textura y se prefiere el
    // más simple si es casi igual de bueno (un paneo de píxeles enteros debe
    // copiar exacto, sin interpolar).
    let k = 4.0 * 65536.0;
    let n = inl.len() as f64;
    let (tx, ty) = (
        inl.iter().map(|p| p.2).sum::<f64>() / n,
        inl.iter().map(|p| p.3).sum::<f64>() / n,
    );
    let to_w = |m: [f64; 6]| Warp {
        p: m.map(|v| (v * k).round() as i32),
        gold: false,
    };
    let cands = [
        to_w([0.0, 0.0, tx.round(), 0.0, 0.0, ty.round()]),
        to_w([0.0, 0.0, tx, 0.0, 0.0, ty]),
        to_w(model),
    ];
    let prev8: Vec<u8> = lp.iter().map(|&v| v.clamp(0, 255) as u8).collect();
    let err = |w: &Warp| -> f64 {
        let ip = InterPlane {
            data: &prev8,
            w: cw,
            h: ch,
            ox: 0,
            oy: 0,
            hash: None,
            d: 1,
            warp: Some(*w),
            gdata: None,
        };
        let mut pred = vec![0i32; B * B];
        let mut e = 0f64;
        for &(bx, by) in &blocks {
            ip.predict_warp(bx, by, B, B, &mut pred);
            for y in 0..B {
                for x in 0..B {
                    e += (lc[(by + y) * cw + bx + x] - pred[y * B + x]).abs() as f64;
                }
            }
        }
        e
    };
    let errs: Vec<f64> = cands.iter().map(err).collect();
    let best = errs.iter().cloned().fold(f64::INFINITY, f64::min);
    let i = if errs[0] <= best * 1.02 {
        0
    } else if errs[1] <= best * 1.01 {
        1
    } else {
        2
    };
    let w = cands[i];
    // Sin movimiento también vale (respecto a la referencia larga es útil).
    (w.clamped() == w).then_some(w)
}

/// Elige entre los movimientos globales candidatos (respecto al fotograma
/// anterior y a la referencia larga) el que mejor predice la luma del
/// fotograma con los planos reconstruidos de verdad (así cuenta la pérdida
/// acumulada de encadenar interpolaciones). Devuelve también si la
/// referencia larga se ha quedado vieja (predice un 25 % peor o no sirve).
fn choose_warp(
    rc: &RefCanvas,
    rgb: &[u8],
    w: usize,
    h: usize,
    header: &Header,
    cands: [Option<Warp>; 2],
) -> (Option<Warp>, bool) {
    const B: usize = 16;
    let luma = color_planes(rgb, w, h, header).swap_remove(0).0;
    let mut errs = [f64::INFINITY; 2];
    let mut best: Option<(f64, Warp)> = None;
    for c in cands.into_iter().flatten() {
        if c.gold && rc.gold.is_none() {
            continue;
        }
        let ip = InterPlane {
            warp: Some(c),
            hash: None,
            ..rc.plane(0, 0, 0)
        };
        let mut pred = vec![0i32; B * B];
        let mut e = 0f64;
        for by in (0..h.saturating_sub(B - 1)).step_by(B) {
            for bx in (0..w.saturating_sub(B - 1)).step_by(B) {
                ip.predict_warp(bx, by, B, B, &mut pred);
                for y in 0..B {
                    for x in 0..B {
                        e += (luma[(by + y) * w + bx + x] - pred[y * B + x] as f32).abs() as f64;
                    }
                }
            }
        }
        errs[c.gold as usize] = e;
        // Ante la duda, la referencia larga (no acumula interpolaciones).
        let e = if c.gold { e * 0.99 } else { e };
        if best.is_none_or(|b| e < b.0) {
            best = Some((e, c));
        }
    }
    let stale = !(errs[1] <= errs[0] * 1.25);
    (best.map(|b| b.1), stale)
}

/// dq extra por celda AQ (en coordenadas del fotograma `r`): `d` donde el
/// fotograma anterior (desplazado con el movimiento global, si lo hay) ya
/// predice bien el actual, la mitad donde lo predice regular y 0 en el
/// contenido nuevo. Así lo que solo se copia no gasta bits en corregir el
/// error del fotograma anterior, y lo nuevo conserva la calidad pedida.
fn temporal_dq(
    prev: &[u8],
    cur: &[u8],
    cw: usize,
    ch: usize,
    r: (usize, usize, usize, usize),
    warp: Option<Warp>,
    d: i32,
) -> (Vec<i32>, usize) {
    let luma = |img: &[u8]| -> Vec<u8> {
        img.chunks_exact(4)
            .map(|p| ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32) >> 8) as u8)
            .collect()
    };
    let (lp, lc) = (luma(prev), luma(cur));
    let (rx, ry, rw, rh) = r;
    let (gw, gh) = (rw.div_ceil(AQ), rh.div_ceil(AQ));
    let ip = InterPlane {
        data: &lp,
        w: cw,
        h: ch,
        ox: rx,
        oy: ry,
        hash: None,
        d: 1,
        warp: Some(warp.unwrap_or_default()),
        gdata: None,
    };
    let mut out = vec![0; gw * gh];
    let mut pred = vec![0i32; AQ * AQ];
    for gy in 0..gh {
        for gx in 0..gw {
            let (x0, y0) = (gx * AQ, gy * AQ);
            let (bw, bh) = (AQ.min(rw - x0), AQ.min(rh - y0));
            ip.predict_warp(x0, y0, AQ, AQ, &mut pred);
            let mut e = 0i64;
            for y in 0..bh {
                for x in 0..bw {
                    let c = lc[(ry + y0 + y) * cw + rx + x0 + x] as i32;
                    e += (c - pred[y * AQ + x]).abs() as i64;
                }
            }
            let mad = e as f32 / (bw * bh) as f32;
            out[gy * gw + gx] = if mad < 2.0 {
                d
            } else if mad < 5.0 {
                d / 2
            } else {
                0
            };
        }
    }
    (out, gw)
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
    encode_animation_recon(frames, durations_ms, width, height, loops, p).map(|r| r.0)
}

/// Como `encode_animation`, y además el lienzo reconstruido tras cada
/// fotograma guardado, tal como lo verá el decodificador (para comprobar
/// que codificador y decodificador coinciden byte a byte).
#[allow(clippy::type_complexity)]
#[doc(hidden)]
pub fn encode_animation_recon(
    frames: &[Vec<u8>],
    durations_ms: &[u32],
    width: usize,
    height: usize,
    loops: u16,
    p: &Params,
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let mut recons = Vec::new();
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
    let use_warp = std::env::var_os("TESELA_NOWARP").is_none();
    let boost_gold = std::env::var_os("TESELA_NOGOLDBOOST").is_none();
    // dq extra (unidades de 1/6 de octava del paso) en las celdas bien predichas.
    let taq: i32 = std::env::var("TESELA_TAQ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let rgb_of = |f: &[u8]| -> Vec<u8> {
        f.chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect()
    };
    // Lienzo y referencia tal como los verá el decodificador.
    let mut st = AnimState::new(width, height);
    // Fotograma original de la referencia larga.
    let mut gold_idx = 0;
    // ¿Contenido fotográfico? Se decide una vez con el primer fotograma
    // completo (los rectángulos que cambian pueden ser muy pequeños).
    let photo = !p
        .screen
        .unwrap_or_else(|| tools::looks_synthetic(&rgb_of(&frames[0]), width, height));
    // (duración, x, y, w, h, datos, nueva referencia larga)
    let mut list: Vec<(u32, usize, usize, usize, usize, Vec<u8>, bool)> = Vec::new();
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
                let prev_src = rgb_of(&frames[i.max(1) - 1]);
                let inter = i > 0 && use_inter;
                // En contenido fotográfico, los fotogramas que predicen del
                // anterior se cuantizan más gruesos: su residuo es sobre todo el
                // error del fotograma previo, y corregirlo cuesta bits sin
                // mejorar lo que se ve (como hacen los códecs de vídeo). En
                // capturas no: el residuo es contenido nuevo (texto).
                let dq: f32 = std::env::var("TESELA_INTER_DQ")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(if photo { 15.0 } else { 0.0 });
                let mut pf = if inter && dq != 0.0 {
                    Params {
                        quality: (sub.quality - dq).max(1.0),
                        ..sub.clone()
                    }
                } else {
                    sub.clone()
                };
                // La restauración apenas mejora lo que se copia del fotograma
                // anterior y sus parámetros cuestan ~25 % de cada fotograma.
                if inter && std::env::var_os("TESELA_REST_INTER").is_none() {
                    pf.restore = false;
                }
                if inter && std::env::var_os("TESELA_NOAQ_INTER").is_some() {
                    pf.aq_strength = 0.0;
                }
                let fi = FrameInfo {
                    duration_ms: 0,
                    x,
                    y,
                    w,
                    h,
                    range: 0..0,
                    prefix: Vec::new(),
                    gold: i == 0,
                };
                let rect = (x, y, w, h);
                let warp = if inter && use_warp {
                    estimate_warp(&frames[i - 1], f, width, height, rect)
                        .filter(|w| *w != Warp::default())
                } else {
                    None
                };
                let wgold = if inter && use_warp {
                    estimate_warp(&frames[gold_idx], f, width, height, rect)
                        .map(|w| Warp { gold: true, ..w })
                } else {
                    None
                };
                let tdq = (inter && taq > 0).then(|| {
                    temporal_dq(&frames[i - 1], f, width, height, (x, y, w, h), warp, taq)
                });
                // Si el fotograma acaba siendo la nueva referencia larga y la
                // anterior duró (el movimiento es lento respecto al encuadre),
                // se vuelve a codificar con la calidad pedida (sin el −dq de
                // los predichos): muchos de los siguientes predicen de él. En
                // un paneo la referencia se renueva cada pocos fotogramas y no
                // compensa.
                let snapshot = (pf.quality != sub.quality).then(|| st.clone());
                let enc = |st: &mut AnimState, pf: &Params, tdq| {
                    let af = AnimFrame {
                        st,
                        src: &prev_src,
                        f: fi.clone(),
                        inter,
                        warps: [warp, wgold],
                        tdq,
                    };
                    encode_with_alpha_ref(
                        &crgb,
                        has_alpha.then_some(&alpha[..]),
                        w,
                        h,
                        pf,
                        Some(af),
                    )
                };
                let mut e = enc(&mut st, &pf, tdq.clone());
                if st.last_gold && i > 0 && boost_gold && i - gold_idx >= 8 {
                    if let Some(snap) = snapshot {
                        st = snap;
                        e = enc(&mut st, &sub, tdq);
                    }
                }
                recons.push(st.canvas.clone());
                if st.last_gold {
                    gold_idx = i;
                }
                list.push((durations_ms[i], x, y, w, h, e.bytes, st.last_gold && i > 0));
            }
        }
    }
    let rgb: Vec<u8> = frames[0]
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    let mut header = make_header(&rgb, width, height, &sub);
    header.tools = (header.tools & !(TOOL_PREVIEW | TOOL_META)) | TOOL_ANIM;
    let meta = p.meta.without_srgb();
    if !meta.is_empty() {
        header.tools |= TOOL_META;
    }
    let mut out = Vec::new();
    header.write(&mut out);
    if !meta.is_empty() {
        meta.write(&mut out);
    }
    out.extend_from_slice(&loops.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(list.len() as u32).to_le_bytes());
    let mut prev: Option<Header> = None;
    for (d, x, y, w, h, bytes, gold) in &list {
        let gbit = if *gold { 2 } else { 0 };
        for v in [*d, *x as u32, *y as u32, *w as u32, *h as u32] {
            write_varint(&mut out, v);
        }
        let hdr = Header::read(bytes).map_err(|e| format!("fotograma: {e}"))?;
        // Forma abreviada si la cabecera es la del fotograma anterior salvo
        // el tamaño y `tools` (sin vista previa ni metadatos).
        let abbrev = prev.as_ref().is_some_and(|p| {
            hdr.tools & (TOOL_PREVIEW | TOOL_META | TOOL_ANIM) == 0
                && Header {
                    width: p.width,
                    height: p.height,
                    tools: p.tools,
                    ..hdr.clone()
                } == *p
        });
        if abbrev {
            out.push(1 | gbit);
            out.push(hdr.tools);
            let ntiles = tile_grid(*w, *h, hdr.tile_size()).len();
            for k in 0..ntiles {
                let o = HEADER_LEN + 4 * k;
                write_varint(
                    &mut out,
                    u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()),
                );
            }
            out.extend_from_slice(&bytes[HEADER_LEN + 4 * ntiles..]);
        } else {
            out.push(gbit);
            write_varint(&mut out, bytes.len() as u32);
            out.extend_from_slice(bytes);
        }
        prev = Some(hdr);
    }
    Ok((out, recons))
}
