//! Predicción intra: planar, DC, 33 direcciones angulares (estilo HEVC) y,
//! para croma, predicción de croma a partir de luma (CCLM) con parámetros
//! deducidos de los vecinos (no se transmiten).

pub const MODE_PLANAR: u8 = 0;
pub const MODE_DC: u8 = 1;
pub const MODE_HOR: u8 = 10;
pub const MODE_VER: u8 = 26;
pub const NUM_ANGULAR_MODES: u8 = 35;
pub const MODE_CCLM: u8 = 35;

const ANGLE: [i32; 33] = [
    32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26, -32, -26, -21, -17, -13, -9,
    -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32,
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
}

pub fn build_refs(p: &Plane, x0: usize, y0: usize, w: usize, h: usize) -> Refs {
    let (x0i, y0i) = (x0 as isize, y0 as isize);
    let (nl, nt) = (h + w, w + h);
    // Orden de sustitución HEVC: abajo-izquierda -> esquina -> arriba-derecha.
    let total = nl + 1 + nt;
    let mut vals = [0i32; 257];
    let mut av = [false; 257];
    for k in 0..total {
        let (x, y) = if k < nl {
            (x0i - 1, y0i + (nl - 1 - k) as isize)
        } else if k == nl {
            (x0i - 1, y0i - 1)
        } else {
            (x0i + (k - nl - 1) as isize, y0i - 1)
        };
        if p.is_avail(x, y) {
            av[k] = true;
            vals[k] = p.px(x as usize, y as usize) as i32;
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
/// En bloques cuadrados coincide bit a bit con la formulación de HEVC.
pub fn predict(raw: &Refs, mode: u8, w: usize, h: usize, out: &mut [i32]) {
    let filtered;
    let r = if needs_filter(mode, eq_size(w, h)) {
        filtered = filter_refs(raw, w, h);
        &filtered
    } else {
        raw
    };
    let (lw, lh) = (w.trailing_zeros(), h.trailing_zeros());
    match mode {
        MODE_PLANAR => {
            let tr = r.top[w + 1];
            let bl = r.left[h + 1];
            for y in 0..h {
                for x in 0..w {
                    let pv = ((h - 1 - y) as i32 * r.top[x + 1] + (y + 1) as i32 * bl) << lw;
                    let ph = ((w - 1 - x) as i32 * r.left[y + 1] + (x + 1) as i32 * tr) << lh;
                    out[y * w + x] = (pv + ph + (w * h) as i32) >> (lw + lh + 1);
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
            let off = len_side;
            let mut refb = [0i32; 64 * 3 + 2];
            for k in 0..=len_main + len_side + 1 {
                refb[off + k] = main[k];
            }
            if angle < 0 {
                let inv = inv_angle(angle);
                let last = (len_side as i32 * angle) >> 5;
                if last < -1 {
                    let mut k = -1;
                    while k >= last {
                        let idx = ((k * inv + 128) >> 8) as usize;
                        refb[(off as i32 + k) as usize] = side[idx.min(len_side + len_main)];
                        k -= 1;
                    }
                }
            }
            for j in 0..len_side {
                let pos = (j as i32 + 1) * angle;
                let idx = pos >> 5;
                let f = pos & 31;
                for i in 0..len_main {
                    let base = (off as i32 + i as i32 + idx + 1) as usize;
                    let v = ((32 - f) * refb[base] + f * refb[base + 1] + 16) >> 5;
                    if vertical {
                        out[j * w + i] = v;
                    } else {
                        out[i * w + j] = v;
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
