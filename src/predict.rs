//! Predicción intra: planar, DC, 33 direcciones angulares (estilo HEVC) y,
//! para croma, predicción de croma a partir de luma (CCLM) con parámetros
//! deducidos de los vecinos (no se transmiten).

pub const MODE_PLANAR: u8 = 0;
pub const MODE_DC: u8 = 1;
pub const MODE_HOR: u8 = 10;
pub const MODE_VER: u8 = 26;
pub const NUM_ANGULAR_MODES: u8 = 35;
pub const MODE_CCLM: u8 = 35;
/// v2: variantes del modo planar (estilo AV1).
pub const MODE_SMOOTH: u8 = 36;
pub const MODE_SMOOTH_V: u8 = 37;
pub const MODE_SMOOTH_H: u8 = 38;

/// Pesos de SMOOTH (los de AV1), por lado: 4, 8, 16, 32, 64.
const SM_W: [u8; 124] = [
    255, 149, 85, 64, // 4
    255, 197, 146, 105, 73, 50, 37, 32, // 8
    255, 225, 196, 170, 145, 123, 102, 84, 68, 54, 43, 33, 26, 20, 17, 16, // 16
    255, 240, 225, 210, 196, 182, 169, 157, 145, 133, 122, 111, 101, 92, 83, 74, 66, 59, 52, 45,
    39, 34, 29, 25, 21, 17, 14, 12, 10, 9, 8, 8, // 32
    255, 248, 240, 233, 225, 218, 210, 203, 196, 189, 182, 176, 169, 163, 156, 150, 144, 138, 133,
    127, 121, 116, 111, 106, 101, 96, 91, 86, 82, 77, 73, 69, 65, 61, 57, 54, 50, 47, 44, 41, 38,
    35, 32, 29, 27, 25, 22, 20, 18, 16, 15, 13, 12, 10, 9, 8, 7, 6, 6, 5, 5, 4, 4, 4, // 64
];

fn sm_weights(n: usize) -> &'static [u8] {
    &SM_W[n - 4..2 * n - 4]
}

/// SMOOTH, SMOOTH_V y SMOOTH_H: interpolación cuadrática entre la fila de
/// arriba y la última muestra de la izquierda (vertical) y entre la columna de
/// la izquierda y la última muestra de arriba (horizontal).
fn predict_smooth(r: &Refs, mode: u8, w: usize, h: usize, out: &mut [i32]) {
    let (wx, wy) = (sm_weights(w), sm_weights(h));
    let bl = r.left[h];
    let tr = r.top[w];
    for y in 0..h {
        let a = wy[y] as i32;
        for x in 0..w {
            let b = wx[x] as i32;
            let v = a * r.top[x + 1] + (256 - a) * bl;
            let hz = b * r.left[y + 1] + (256 - b) * tr;
            out[y * w + x] = match mode {
                MODE_SMOOTH => (v + hz + 256) >> 9,
                MODE_SMOOTH_V => (v + 128) >> 8,
                _ => (hz + 128) >> 8,
            };
        }
    }
}

const ANGLE: [i32; 33] = [
    32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26, -32, -26, -21, -17, -13, -9,
    -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32,
];

/// Interpolación cúbica de VVC (fase en 1/32).
const FC: [[i32; 4]; 32] = [
    [0, 64, 0, 0],
    [-1, 63, 2, 0],
    [-2, 62, 4, 0],
    [-2, 60, 7, -1],
    [-2, 58, 10, -2],
    [-3, 57, 12, -2],
    [-4, 56, 14, -2],
    [-4, 55, 15, -2],
    [-4, 54, 16, -2],
    [-5, 53, 18, -2],
    [-6, 52, 20, -2],
    [-6, 49, 24, -3],
    [-6, 46, 28, -4],
    [-5, 44, 29, -4],
    [-4, 42, 30, -4],
    [-4, 39, 33, -4],
    [-4, 36, 36, -4],
    [-4, 33, 39, -4],
    [-4, 30, 42, -4],
    [-4, 29, 44, -5],
    [-4, 28, 46, -6],
    [-3, 24, 49, -6],
    [-2, 20, 52, -6],
    [-2, 18, 53, -5],
    [-2, 16, 54, -4],
    [-2, 15, 55, -4],
    [-2, 14, 56, -4],
    [-2, 12, 57, -3],
    [-2, 10, 58, -2],
    [-1, 7, 60, -2],
    [0, 4, 62, -2],
    [0, 2, 63, -1],
];

fn inv_angle(a: i32) -> i32 {
    match a {
        -2 => -4096,
        -5 => -1638,
        -9 => -910,
        -13 => -630,
        -17 => -482,
        -21 => -390,
        -26 => -315,
        -32 => -256,
        _ => 0,
    }
}

/// Plano de reconstrucción con mapa de disponibilidad y de información de
/// bloques a granularidad 4×4.
#[derive(Clone)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
    pub gw: usize,
    pub gh: usize,
    pub avail: Vec<bool>,
    pub modes: Vec<u8>,
    pub sizes: Vec<u8>,
    pub nz: Vec<bool>,
    /// Vector de movimiento de las hojas predichas del fotograma anterior.
    pub mvs: Vec<(i32, i32)>,
}

impl Plane {
    pub fn new(w: usize, h: usize) -> Self {
        let gw = w / 4;
        let gh = h / 4;
        Plane {
            w,
            h,
            data: vec![0; w * h],
            gw,
            gh,
            avail: vec![false; gw * gh],
            modes: vec![MODE_DC; gw * gh],
            sizes: vec![64; gw * gh],
            nz: vec![false; gw * gh],
            mvs: vec![(0, 0); gw * gh],
        }
    }

    #[inline]
    pub fn px(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }

    #[inline]
    fn is_avail(&self, x: isize, y: isize) -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < self.w
            && (y as usize) < self.h
            && self.avail[(y as usize / 4) * self.gw + x as usize / 4]
    }

    /// Marca un bloque `w × h` como decodificado (el tamaño guardado es el lado menor).
    pub fn set_block(&mut self, x0: usize, y0: usize, w: usize, h: usize, mode: u8, nz: bool) {
        for gy in y0 / 4..(y0 + h) / 4 {
            for gx in x0 / 4..(x0 + w) / 4 {
                let i = gy * self.gw + gx;
                self.avail[i] = true;
                self.modes[i] = mode;
                self.sizes[i] = w.min(h) as u8;
                self.nz[i] = nz;
            }
        }
    }

    /// Guarda el vector de movimiento de un bloque.
    pub fn set_mv(&mut self, x0: usize, y0: usize, w: usize, h: usize, mv: (i32, i32)) {
        for gy in y0 / 4..(y0 + h) / 4 {
            for gx in x0 / 4..(x0 + w) / 4 {
                self.mvs[gy * self.gw + gx] = mv;
            }
        }
    }

    /// Vector del bloque que contiene (x, y), si está disponible y tiene la marca dada.
    pub fn mv_at(&self, x: isize, y: isize, mark: u8) -> Option<(i32, i32)> {
        if !self.is_avail(x, y) {
            return None;
        }
        let i = (y as usize / 4) * self.gw + x as usize / 4;
        (self.modes[i] == mark).then_some(self.mvs[i])
    }

    pub fn clear_avail(&mut self, x0: usize, y0: usize, w: usize, h: usize) {
        for gy in y0 / 4..(y0 + h) / 4 {
            for gx in x0 / 4..(x0 + w) / 4 {
                self.avail[gy * self.gw + gx] = false;
            }
        }
    }

    /// Modo y tamaño del bloque vecino (izquierda, arriba) si existe.
    pub fn neighbor(&self, x0: usize, y0: usize, left: bool) -> Option<(u8, u8, bool)> {
        let (x, y) = if left {
            (x0 as isize - 1, y0 as isize)
        } else {
            (x0 as isize, y0 as isize - 1)
        };
        if self.is_avail(x, y) {
            let i = (y as usize / 4) * self.gw + x as usize / 4;
            Some((self.modes[i], self.sizes[i], self.nz[i]))
        } else {
            None
        }
    }

    /// Copia los píxeles y los mapas de una región desde otro plano del mismo tamaño.
    pub fn copy_region(&mut self, from: &Plane, x0: usize, y0: usize, w: usize, h: usize) {
        for y in y0..(y0 + h).min(self.h) {
            let a = y * self.w + x0;
            let e = (x0 + w).min(self.w) - x0;
            self.data[a..a + e].copy_from_slice(&from.data[a..a + e]);
        }
        for gy in y0 / 4..((y0 + h) / 4).min(self.gh) {
            for gx in x0 / 4..((x0 + w) / 4).min(self.gw) {
                let i = gy * self.gw + gx;
                self.avail[i] = from.avail[i];
                self.modes[i] = from.modes[i];
                self.sizes[i] = from.sizes[i];
                self.nz[i] = from.nz[i];
                self.mvs[i] = from.mvs[i];
            }
        }
    }

    pub fn read_block(&self, x0: usize, y0: usize, w: usize, h: usize, out: &mut [u8]) {
        for y in 0..h {
            out[y * w..y * w + w].copy_from_slice(&self.data[(y0 + y) * self.w + x0..][..w]);
        }
    }

    pub fn write_block(&mut self, x0: usize, y0: usize, w: usize, h: usize, src: &[u8]) {
        for y in 0..h {
            self.data[(y0 + y) * self.w + x0..][..w].copy_from_slice(&src[y * w..y * w + w]);
        }
    }
}

/// Muestras de referencia de un bloque `w × h`: `top[0]` = esquina,
/// `top[1+i]` = fila superior (w + h muestras, incluida la parte de arriba a
/// la derecha) y `left[1+i]` = columna izquierda (h + w muestras).
pub struct Refs {
    pub top: [i32; 130],
    pub left: [i32; 130],
    pub any: bool,
    /// Herramientas intra de la v2 (PDPC, interpolación cúbica).
    pub v2: bool,
}

pub fn build_refs(p: &Plane, x0: usize, y0: usize, w: usize, h: usize, v2: bool) -> Refs {
    let (x0i, y0i) = (x0 as isize, y0 as isize);
    let (nl, nt) = (h + w, w + h);
    // Orden de sustitución HEVC: abajo-izquierda -> esquina -> arriba-derecha.
    let total = nl + 1 + nt;
    let mut vals = [0i32; 257];
    let mut av = [false; 257];
    // La disponibilidad es por celdas de 4×4 y los lados del bloque son
    // múltiplos de 4 (el plano también): una consulta por grupo de 4 muestras.
    for g in 0..nl / 4 {
        let y = y0i + (nl - 1 - 4 * g) as isize;
        if p.is_avail(x0i - 1, y) {
            for t in 0..4 {
                av[4 * g + t] = true;
                vals[4 * g + t] = p.px(x0 - 1, y as usize - t) as i32;
            }
        }
    }
    if p.is_avail(x0i - 1, y0i - 1) {
        av[nl] = true;
        vals[nl] = p.px(x0 - 1, y0 - 1) as i32;
    }
    for g in 0..nt / 4 {
        let x = x0 + 4 * g;
        if p.is_avail(x as isize, y0i - 1) {
            let k = nl + 1 + 4 * g;
            av[k..k + 4].fill(true);
            for t in 0..4 {
                vals[k + t] = p.px(x + t, y0 - 1) as i32;
            }
        }
    }
    let any = av[..total].iter().any(|&a| a);
    if !any {
        vals[..total].fill(128);
    } else {
        let first = av.iter().position(|&a| a).unwrap();
        let v = vals[first];
        for k in 0..first {
            vals[k] = v;
        }
        for k in first + 1..total {
            if !av[k] {
                vals[k] = vals[k - 1];
            }
        }
    }
    let mut r = Refs {
        top: [0; 130],
        left: [0; 130],
        any,
        v2,
    };
    r.top[0] = vals[nl];
    r.left[0] = vals[nl];
    for i in 0..nl {
        r.left[1 + i] = vals[nl - 1 - i];
    }
    for i in 0..nt {
        r.top[1 + i] = vals[nl + 1 + i];
    }
    r.top[nt + 1] = r.top[nt];
    r.left[nl + 1] = r.left[nl];
    r
}

/// Tamaño "equivalente" de un bloque w×h para decidir el filtrado (VVC).
fn eq_size(w: usize, h: usize) -> usize {
    1 << ((w.trailing_zeros() + h.trailing_zeros()) / 2)
}

fn needs_filter(mode: u8, n: usize) -> bool {
    if n < 8 || mode == MODE_DC {
        return false;
    }
    if mode == MODE_PLANAR {
        return true;
    }
    let d = (mode as i32 - 26).abs().min((mode as i32 - 10).abs());
    let thr = match n {
        8 => 7,
        16 => 1,
        _ => 0,
    };
    d > thr
}

fn filter_refs(r: &Refs, w: usize, h: usize) -> Refs {
    // Concatenación: left[nl]..left[1], esquina, top[1]..top[nt]
    let (nl, nt) = (h + w, w + h);
    let total = nl + 1 + nt;
    let mut a = [0i32; 257];
    for i in 0..nl {
        a[i] = r.left[nl - i];
    }
    a[nl] = r.top[0];
    for i in 0..nt {
        a[nl + 1 + i] = r.top[1 + i];
    }
    let mut f = a;
    for i in 1..total - 1 {
        f[i] = (a[i - 1] + 2 * a[i] + a[i + 1] + 2) >> 2;
    }
    let mut o = Refs {
        top: [0; 130],
        left: [0; 130],
        any: r.any,
        v2: r.v2,
    };
    o.top[0] = f[nl];
    o.left[0] = f[nl];
    for i in 0..nl {
        o.left[1 + i] = f[nl - 1 - i];
    }
    for i in 0..nt {
        o.top[1 + i] = f[nl + 1 + i];
    }
    o.top[nt + 1] = o.top[nt];
    o.left[nl + 1] = o.left[nl];
    o
}

/// Predicción planar/DC/angular de un bloque `w × h` en `out` (orden fila).
/// En bloques cuadrados coincide bit a bit con la formulación de HEVC; en la
/// v2 se añaden las variantes SMOOTH, la interpolación cúbica y el PDPC.
pub fn predict(raw: &Refs, mode: u8, w: usize, h: usize, out: &mut [i32]) {
    if mode >= MODE_SMOOTH {
        predict_smooth(raw, mode, w, h, out);
        return;
    }
    let filtered;
    let nf = needs_filter(mode, eq_size(w, h));
    let r = if nf {
        filtered = filter_refs(raw, w, h);
        &filtered
    } else {
        raw
    };
    predict_base(r, mode, w, h, raw.v2 && !nf, out);
    if raw.v2 {
        match mode {
            MODE_PLANAR | MODE_DC | MODE_HOR | MODE_VER => pdpc(r, mode, w, h, out),
            2..=9 | 27..=34 => pdpc_ang(r, mode, w, h, out),
            _ => {}
        }
    }
}

/// Filtro de contorno dependiente de la posición (PDPC, como en VVC): mezcla
/// la predicción con las referencias izquierda y superior con pesos que
/// decaen con la distancia al borde. En los modos horizontal y vertical
/// suma el gradiente de la otra referencia respecto a la esquina.
fn pdpc(r: &Refs, mode: u8, w: usize, h: usize, out: &mut [i32]) {
    let scale = (w.trailing_zeros() + h.trailing_zeros() - 2) >> 2;
    let tl = r.top[0];
    // Los pesos se anulan a partir de 3·2^scale muestras del borde.
    let reach = 3usize << scale;
    // Todo cabe en 16 bits (píxeles 0..=255, pesos ≤ 32, mezcla ≤ 64·255+32):
    // en lanes de 16 bits el compilador vectoriza el bucle (sin pmulld en SSE2).
    let mut top16 = [0i16; 64];
    let mut wlx = [0i16; 64];
    for x in 0..w {
        top16[x] = r.top[x + 1] as i16;
        wlx[x] = (32 >> ((x << 1) >> scale).min(31)) as i16;
    }
    for y in 0..h {
        let wt = 32 >> ((y << 1) >> scale).min(31);
        let row = &mut out[y * w..y * w + w.min(if wt == 0 { reach } else { w })];
        let ly = r.left[y + 1];
        let n = row.len();
        match mode {
            MODE_VER => {
                let dl = (ly - tl) as i16;
                for (o, &wl) in row.iter_mut().zip(&wlx[..n]) {
                    *o = (*o + ((wl * dl + 32) >> 6) as i32).clamp(0, 255);
                }
            }
            MODE_HOR => {
                let wt = wt as i16;
                let tl = tl as i16;
                for (o, &t) in row.iter_mut().zip(&top16[..n]) {
                    *o = (*o + ((wt * (t - tl) + 32) >> 6) as i32).clamp(0, 255);
                }
            }
            _ => {
                let (ly, wt) = (ly as i16, wt as i16);
                for ((o, &wl), &t) in row.iter_mut().zip(&wlx[..n]).zip(&top16[..n]) {
                    let v = wl * ly + wt * t + (64 - wl - wt) * (*o as i16) + 32;
                    *o = (v >> 6) as i32;
                }
            }
        }
    }
}

/// PDPC de los modos angulares que predicen desde abajo a la izquierda
/// (2-9) o desde arriba a la derecha (27-34): mezcla con la muestra del otro
/// lado en la prolongación de la dirección (VVC).
fn pdpc_ang(r: &Refs, mode: u8, w: usize, h: usize, out: &mut [i32]) {
    let angle = ANGLE[mode as usize - 2];
    let inv = (16384 + angle / 2) / angle;
    let vertical = mode >= 18;
    let side = if vertical { h } else { w };
    let lg = 31 - ((3 * inv - 2) as u32).leading_zeros() as i32;
    let scale = (side.trailing_zeros() as i32 - lg + 8).min(2);
    if scale < 0 {
        return;
    }
    let lim = w + h;
    // Distancia al lado (columna en los verticales, fila en los horizontales)
    // a partir de la cual el peso es 0.
    let (nd, na) = if vertical { (w, h) } else { (h, w) };
    for dist in 0..nd.min(3 << scale) {
        let wgt = 32 >> ((dist << 1) >> scale);
        let d = (((dist as i32 + 1) * inv + 256) >> 9) as usize;
        for along in 0..na {
            let k = along + d;
            if k >= lim {
                break;
            }
            let (rv, i) = if vertical {
                (r.left[k + 1], along * w + dist)
            } else {
                (r.top[k + 1], dist * w + along)
            };
            out[i] = (rv * wgt + (64 - wgt) * out[i] + 32) >> 6;
        }
    }
}

/// Planar, DC y angulares sobre las referencias `r` (ya filtradas si toca).
/// Con `cubic`, los modos angulares con fracción interpolan con 4
/// coeficientes en lugar de 2.
fn predict_base(r: &Refs, mode: u8, w: usize, h: usize, cubic: bool, out: &mut [i32]) {
    let (lw, lh) = (w.trailing_zeros(), h.trailing_zeros());
    match mode {
        MODE_PLANAR => {
            let tr = r.top[w + 1];
            let bl = r.left[h + 1];
            let top = &r.top[1..=w];
            for (y, row) in out[..w * h].chunks_exact_mut(w).enumerate() {
                let (hy, by, ly) = ((h - 1 - y) as i32, (y + 1) as i32 * bl, r.left[y + 1]);
                for (x, (o, &t)) in row.iter_mut().zip(top).enumerate() {
                    let pv = (hy * t + by) << lw;
                    let ph = ((w - 1 - x) as i32 * ly + (x + 1) as i32 * tr) << lh;
                    *o = (pv + ph + (w * h) as i32) >> (lw + lh + 1);
                }
            }
        }
        MODE_DC => {
            let s: i32 = r.top[1..=w].iter().sum::<i32>() + r.left[1..=h].iter().sum::<i32>();
            let dc = if w == h {
                (s + w as i32) >> (lw + 1)
            } else {
                (s + ((w + h) / 2) as i32) / (w + h) as i32
            };
            out[..w * h].fill(dc);
        }
        _ => {
            let angle = ANGLE[mode as usize - 2];
            let vertical = mode >= 18;
            // Eje principal: columnas (vertical) o filas (horizontal).
            let (main, side, len_main, len_side) = if vertical {
                (&r.top, &r.left, w, h)
            } else {
                (&r.left, &r.top, h, w)
            };
            // buffer con índice desplazado: refb[off + k] = ref[k], k en [-len_side, len_main+len_side+1]
            let off = len_side + 1;
            // Las muestras son píxeles (0..255): con 16 bits el compilador
            // vectoriza las filas y las sumas ponderadas no desbordan.
            let mut refb = [0i16; 64 * 3 + 5];
            for (d, &m) in refb[off..=off + len_main + len_side + 1]
                .iter_mut()
                .zip(&main[..=len_main + len_side + 1])
            {
                *d = m as i16;
            }
            refb[off + len_main + len_side + 2] = main[len_main + len_side + 1] as i16;
            if angle < 0 {
                let inv = inv_angle(angle);
                let last = (len_side as i32 * angle) >> 5;
                if last < -1 || cubic {
                    let mut k = -1;
                    let end = if cubic { last - 1 } else { last };
                    while k >= end {
                        let idx = ((k * inv + 128) >> 8).max(0) as usize;
                        refb[(off as i32 + k) as usize] = side[idx.min(len_side + len_main)] as i16;
                        k -= 1;
                    }
                }
            }
            let mut row = [0i16; 64];
            let row = &mut row[..len_main];
            let cub = cubic && angle.abs() != 32 && angle != 0;
            for j in 0..len_side {
                let pos = (j as i32 + 1) * angle;
                let idx = pos >> 5;
                let f = pos & 31;
                let b0 = (off as i32 + idx + 1) as usize;
                // Fila de salida (columna en los modos horizontales) con las
                // referencias desplazadas como rebanadas, para vectorizar.
                if cub {
                    let c = FC[f as usize].map(|v| v as i16);
                    let n = len_main;
                    let (r0, r1) = (&refb[b0 - 1..b0 - 1 + n], &refb[b0..b0 + n]);
                    let (r2, r3) = (&refb[b0 + 1..b0 + 1 + n], &refb[b0 + 2..b0 + 2 + n]);
                    let it = row.iter_mut().zip(r0).zip(r1).zip(r2).zip(r3);
                    for ((((o, &a), &b), &cc), &d) in it {
                        *o = ((c[0] * a + c[1] * b + c[2] * cc + c[3] * d + 32) >> 6).clamp(0, 255);
                    }
                } else {
                    let n = len_main;
                    let f = f as i16;
                    let (r0, r1) = (&refb[b0..b0 + n], &refb[b0 + 1..b0 + 1 + n]);
                    for ((o, &a), &b) in row.iter_mut().zip(r0).zip(r1) {
                        *o = ((32 - f) * a + f * b + 16) >> 5;
                    }
                }
                if vertical {
                    for (o, &v) in out[j * w..j * w + len_main].iter_mut().zip(row.iter()) {
                        *o = v as i32;
                    }
                } else {
                    for (o, &v) in out[j..].iter_mut().step_by(w).zip(row.iter()) {
                        *o = v as i32;
                    }
                }
            }
        }
    }
}

/// CCLM: croma = alpha·luma + beta con alpha y beta ajustados por mínimos
/// cuadrados sobre las muestras vecinas ya reconstruidas.
pub fn predict_cclm(
    chroma: &Plane,
    luma: &[u8],
    lw: usize,
    lh: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
    let lpx = |x: usize, y: usize| luma[y.min(lh - 1) * lw + x.min(lw - 1)] as f64;
    let mut sx = 0f64;
    let mut sy = 0f64;
    let mut sxx = 0f64;
    let mut sxy = 0f64;
    let mut cnt = 0f64;
    if chroma.is_avail(x0 as isize, y0 as isize - 1) {
        for i in 0..w {
            let l = lpx(x0 + i, y0 - 1);
            let c = chroma.px(x0 + i, y0 - 1) as f64;
            sx += l;
            sy += c;
            sxx += l * l;
            sxy += l * c;
            cnt += 1.0;
        }
    }
    if chroma.is_avail(x0 as isize - 1, y0 as isize) {
        for i in 0..h {
            let l = lpx(x0 - 1, y0 + i);
            let c = chroma.px(x0 - 1, y0 + i) as f64;
            sx += l;
            sy += c;
            sxx += l * l;
            sxy += l * c;
            cnt += 1.0;
        }
    }
    let (alpha, beta) = if cnt == 0.0 {
        (0.0, 128.0)
    } else {
        let den = cnt * sxx - sx * sx;
        let a = if den > 1e-6 {
            ((cnt * sxy - sx * sy) / den).clamp(-4.0, 4.0)
        } else {
            0.0
        };
        (a, (sy - a * sx) / cnt)
    };
    for y in 0..h {
        for x in 0..w {
            let v = alpha * lpx(x0 + x, y0 + y) + beta;
            out[y * w + x] = v.round().clamp(0.0, 255.0) as i32;
        }
    }
}

// ------------------------------------------------------------ CCLM de la v2

/// v2: variantes de CCLM (sección 3.5 de la especificación). `_T` y `_L`
/// ajustan el modelo solo con la fila de arriba o con la columna de la
/// izquierda, de 2w o 2h muestras (el MDLM de VVC); `MMLM` usa dos modelos
/// separados por la media de la fuente en los vecinos; `CB` predice el Cr
/// desde el Cb reconstruido en lugar de la luma, y `YCB` desde los dos.
pub const MODE_CCLM_T: u8 = 39;
pub const MODE_CCLM_L: u8 = 40;
pub const MODE_MMLM: u8 = 41;
pub const MODE_CCLM_CB: u8 = 42;
pub const MODE_CCLM_YCB: u8 = 43;

pub fn is_cclm(m: u8) -> bool {
    m == MODE_CCLM || (MODE_CCLM_T..=MODE_CCLM_YCB).contains(&m)
}

/// Fuente de la predicción de croma (luma submuestreada o Cb), de `w × h`;
/// las coordenadas se recortan al plano.
#[derive(Clone, Copy)]
pub struct Src<'a> {
    pub data: &'a [u8],
    pub w: usize,
    pub h: usize,
}

impl Src<'_> {
    #[inline]
    fn px(&self, x: usize, y: usize) -> i32 {
        self.data[y.min(self.h - 1) * self.w + x.min(self.w - 1)] as i32
    }

    /// Fila `y` del bloque desde `x0`, de `n` muestras.
    fn row(&self, x0: usize, y: usize, n: usize, out: &mut [i32]) {
        let r = &self.data[y.min(self.h - 1) * self.w..][..self.w];
        if x0 + n <= self.w {
            for (o, &v) in out[..n].iter_mut().zip(&r[x0..x0 + n]) {
                *o = v as i32;
            }
        } else {
            for (i, o) in out[..n].iter_mut().enumerate() {
                *o = r[(x0 + i).min(self.w - 1)] as i32;
            }
        }
    }
}

/// Posiciones vecinas de la croma con las que se ajusta el modelo: `top`
/// muestras de la fila de arriba y `left` de la columna de la izquierda,
/// cada tramo hasta la primera que no esté disponible.
fn lm_neighbors(
    chroma: &Plane,
    x0: usize,
    y0: usize,
    top: usize,
    left: usize,
) -> Vec<(usize, usize)> {
    let mut v = Vec::with_capacity(top + left);
    for i in 0..top {
        if !chroma.is_avail((x0 + i) as isize, y0 as isize - 1) {
            break;
        }
        v.push((x0 + i, y0 - 1));
    }
    for i in 0..left {
        if !chroma.is_avail(x0 as isize - 1, (y0 + i) as isize) {
            break;
        }
        v.push((x0 - 1, y0 + i));
    }
    v
}

/// Mínimos cuadrados en enteros sobre (n, Σs, Σc, Σs², Σs·c): (a, b) en
/// 1/2^16, con la predicción `(a·s + b + 2^15) >> 16`.
fn lm_fit(n: i64, sx: i64, sy: i64, sxx: i64, sxy: i64) -> (i32, i32) {
    if n == 0 {
        return (0, 128 << 16);
    }
    let den = n * sxx - sx * sx;
    let a = if den > 0 {
        (((n * sxy - sx * sy) << 16) / den).clamp(-4 << 16, 4 << 16)
    } else {
        0
    };
    (a as i32, ((sy << 16) - a * sx).div_euclid(n) as i32)
}

/// Variantes de una sola fuente (`MODE_CCLM_T`, `_L`, `MMLM` y `_CB`).
#[allow(clippy::too_many_arguments)]
pub fn predict_lm(
    chroma: &Plane,
    src: Src,
    mode: u8,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
    let (top, left) = match mode {
        MODE_CCLM_T => (2 * w, 0),
        MODE_CCLM_L => (0, 2 * h),
        _ => (w, h),
    };
    let pairs: Vec<(i64, i64)> = lm_neighbors(chroma, x0, y0, top, left)
        .into_iter()
        .map(|(x, y)| (src.px(x, y) as i64, chroma.px(x, y) as i64))
        .collect();
    let fit = |sel: &dyn Fn(i64) -> bool| {
        let mut s = [0i64; 5];
        for &(l, c) in pairs.iter().filter(|p| sel(p.0)) {
            s[0] += 1;
            s[1] += l;
            s[2] += c;
            s[3] += l * l;
            s[4] += l * c;
        }
        (lm_fit(s[0], s[1], s[2], s[3], s[4]), s[0])
    };
    let (m0, n) = fit(&|_| true);
    let mut row = [0i32; 64];
    if mode == MODE_MMLM && n > 0 {
        let thr = pairs.iter().map(|p| p.0).sum::<i64>().div_euclid(n);
        let (lo, nlo) = fit(&|l| l <= thr);
        let (hi, nhi) = fit(&|l| l > thr);
        let lo = if nlo >= 2 { lo } else { m0 };
        let hi = if nhi >= 2 { hi } else { m0 };
        let thr = thr as i32;
        for y in 0..h {
            src.row(x0, y0 + y, w, &mut row);
            for (o, &l) in out[y * w..(y + 1) * w].iter_mut().zip(&row[..w]) {
                let (a, b) = if l <= thr { lo } else { hi };
                *o = ((a * l + b + (1 << 15)) >> 16).clamp(0, 255);
            }
        }
        return;
    }
    let (a, b) = m0;
    for y in 0..h {
        src.row(x0, y0 + y, w, &mut row);
        for (o, &l) in out[y * w..(y + 1) * w].iter_mut().zip(&row[..w]) {
            *o = ((a * l + b + (1 << 15)) >> 16).clamp(0, 255);
        }
    }
}

/// `MODE_CCLM_YCB`: Cr = a·Y + b·Cb + c por mínimos cuadrados sobre los
/// vecinos de arriba (w) y de la izquierda (h).
#[allow(clippy::too_many_arguments)]
pub fn predict_ycb(
    chroma: &Plane,
    luma: Src,
    cb: Src,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
    // n, ΣY, ΣB, ΣC, ΣY², ΣB², ΣYB, ΣYC, ΣBC
    let mut s = [0i64; 9];
    for (x, y) in lm_neighbors(chroma, x0, y0, w, h) {
        let (a, b, c) = (
            luma.px(x, y) as i64,
            cb.px(x, y) as i64,
            chroma.px(x, y) as i64,
        );
        for (k, v) in [1, a, b, c, a * a, b * b, a * b, a * c, b * c]
            .into_iter()
            .enumerate()
        {
            s[k] += v;
        }
    }
    let n = s[0];
    let (a, b, c) = if n == 0 {
        (0, 0, 128 << 16)
    } else {
        let cov = |i: usize, j: usize, ij: usize| (n * s[ij] - s[i] * s[j]) as i128;
        let (syy, sbb, syb) = (cov(1, 1, 4), cov(2, 2, 5), cov(1, 2, 6));
        let (syc, sbc) = (cov(1, 3, 7), cov(2, 3, 8));
        let det = syy * sbb - syb * syb;
        let (a, b) = if det > 0 {
            (
                ((syc * sbb - sbc * syb) << 16) / det,
                ((sbc * syy - syc * syb) << 16) / det,
            )
        } else if sbb > 0 {
            (0, (sbc << 16) / sbb)
        } else {
            (0, 0)
        };
        let lim = 4i128 << 16;
        let (a, b) = (a.clamp(-lim, lim) as i64, b.clamp(-lim, lim) as i64);
        (a, b, ((s[3] << 16) - a * s[1] - b * s[2]).div_euclid(n))
    };
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let (mut ry, mut rb) = ([0i32; 64], [0i32; 64]);
    for y in 0..h {
        luma.row(x0, y0 + y, w, &mut ry);
        cb.row(x0, y0 + y, w, &mut rb);
        let it = out[y * w..(y + 1) * w]
            .iter_mut()
            .zip(&ry[..w])
            .zip(&rb[..w]);
        for ((o, &l), &v) in it {
            *o = ((a * l + b * v + c + (1 << 15)) >> 16).clamp(0, 255);
        }
    }
}
