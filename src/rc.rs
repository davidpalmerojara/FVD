//! Codificador aritmético binario (range coder estilo LZMA) con modelos de
//! probabilidad adaptativos de doble velocidad.
//!
//! Toda la sintaxis del bitstream se escribe una sola vez contra el trait
//! [`Coder`], que tiene tres implementaciones:
//! - [`Encoder`]: escribe bits y adapta los modelos.
//! - [`Decoder`]: lee bits y adapta los modelos exactamente igual.
//! - [`CostCounter`]: no escribe nada; suma el coste estimado en bits (para RDO).
//!
//! Las funciones de sintaxis reciben el valor a codificar y devuelven el valor
//! (de)codificado: el codificador devuelve lo que recibió y el decodificador
//! ignora el argumento y devuelve lo leído.


const PROB_BITS: u32 = 16;
const PROB_ONE: u32 = 1 << PROB_BITS;
const PROB_MIN: u32 = 48;
const PROB_MAX: u32 = PROB_ONE - PROB_MIN;

/// Modelo adaptativo de un bit: mezcla una estimación rápida y una lenta.
/// Las primeras observaciones se adaptan más deprisa (tasa por conteo).
#[derive(Clone, Copy, Debug)]
pub struct Model {
    fast: u32,
    slow: u32,
    n: u32,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// Recorrido determinista de todos los modelos de un conjunto de contextos
/// (para inicializarlos con probabilidades entrenadas).
pub trait Visit {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model));
}

impl Visit for Model {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        f(self)
    }
}

impl<T: Visit> Visit for [T] {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        for x in self.iter_mut() {
            x.visit(f);
        }
    }
}

impl<T: Visit, const N: usize> Visit for [T; N] {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.as_mut_slice().visit(f)
    }
}

impl<T: Visit> Visit for Vec<T> {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.as_mut_slice().visit(f)
    }
}

/// Observaciones con las que arranca un modelo inicializado con una
/// probabilidad entrenada (frena la adaptación inicial).
const INIT_N: u32 = 3;

impl Model {
    /// Fija una probabilidad inicial entrenada (de 1, en 2^-16).
    pub fn set_init(&mut self, p1: u16) {
        let p = (p1 as u32).clamp(PROB_MIN, PROB_MAX);
        *self = Model {
            fast: p,
            slow: p,
            n: INIT_N,
        };
    }

    /// Probabilidad actual y nº de observaciones (para entrenamiento).
    pub fn stats(&self) -> (u32, u32) {
        (self.p1(), self.n)
    }

    pub const fn new() -> Self {
        Model {
            fast: PROB_ONE / 2,
            slow: PROB_ONE / 2,
            n: 0,
        }
    }

    /// Probabilidad de que el bit sea 1, en unidades de 2^-16.
    #[inline]
    pub fn p1(&self) -> u32 {
        ((self.fast + self.slow) >> 1).clamp(PROB_MIN, PROB_MAX)
    }

    #[inline]
    pub fn update(&mut self, bit: bool) {
        let sf = (self.n + 1).min(4);
        let ss = (self.n + 1).min(7);
        if bit {
            self.fast += (PROB_ONE - self.fast) >> sf;
            self.slow += (PROB_ONE - self.slow) >> ss;
        } else {
            self.fast -= self.fast >> sf;
            self.slow -= self.slow >> ss;
        }
        if self.n < 30 {
            self.n += 1;
        }
    }

    /// Coste estimado (bits) de codificar `bit` con este modelo.
    #[inline]
    pub fn cost(&self, bit: bool) -> f32 {
        let p1 = self.p1();
        let p = if bit { p1 } else { PROB_ONE - p1 };
        cost_table()[(p >> 4) as usize]
    }
}

/// -log2(p) en `f64`, evaluable en tiempo de compilación: normaliza a [1, 2)
/// (exacto) y usa ln(m) = 2·atanh((m-1)/(m+1)). El error (~1e-16) no cambia el
/// redondeo a `f32` (lo comprueba la prueba `cost_table_exacta`).
const fn neg_log2(p: f64) -> f64 {
    const LN2: f64 = core::f64::consts::LN_2;
    let (mut m, mut e) = (p, 0.0);
    while m < 1.0 {
        m *= 2.0;
        e -= 1.0;
    }
    let z = (m - 1.0) / (m + 1.0);
    let z2 = z * z;
    let (mut term, mut sum, mut k) = (z, 0.0, 1.0);
    while k < 60.0 {
        sum += term / k;
        term *= z2;
        k += 2.0;
    }
    -(e + 2.0 * sum / LN2)
}

#[allow(long_running_const_eval)]
const fn build_cost_table() -> [f32; 4096] {
    let mut t = [0f32; 4096];
    let mut i = 0;
    while i < 4096 {
        t[i] = neg_log2((i as f64 + 0.5) / 4096.0) as f32;
        i += 1;
    }
    t
}

/// Coste en bits de un suceso de probabilidad (i + 0.5)/4096.
static COST_TABLE: [f32; 4096] = build_cost_table();

#[inline(always)]
fn cost_table() -> &'static [f32; 4096] {
    &COST_TABLE
}

pub trait Coder {
    fn bit(&mut self, m: &mut Model, b: bool) -> bool;
    fn bypass(&mut self, b: bool) -> bool;

    /// `n` bits sin contexto, MSB primero.
    fn bypass_bits(&mut self, v: u32, n: u32) -> u32 {
        let mut out = 0;
        for i in (0..n).rev() {
            out = (out << 1) | self.bypass((v >> i) & 1 == 1) as u32;
        }
        out
    }
}

// ---------------------------------------------------------------- Encoder

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Encoder {
            low: 0,
            range: 0xFFFF_FFFF,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        }
    }

    #[inline]
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    #[inline]
    fn encode(&mut self, p1: u32, bit: bool) {
        let bound = (self.range >> PROB_BITS) * p1;
        if bit {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

impl Coder for Encoder {
    #[inline]
    fn bit(&mut self, m: &mut Model, b: bool) -> bool {
        self.encode(m.p1(), b);
        m.update(b);
        b
    }
    #[inline]
    fn bypass(&mut self, b: bool) -> bool {
        self.encode(PROB_ONE / 2, b);
        b
    }
}

// ---------------------------------------------------------------- Decoder

pub struct Decoder<'a> {
    code: u32,
    range: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = Decoder {
            code: 0,
            range: 0xFFFF_FFFF,
            data,
            pos: 0,
        };
        for _ in 0..5 {
            d.code = (d.code << 8) | d.next_byte() as u32;
        }
        d
    }

    #[inline]
    fn next_byte(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    #[inline]
    fn decode(&mut self, p1: u32) -> bool {
        let bound = (self.range >> PROB_BITS) * p1;
        let bit = if self.code < bound {
            self.range = bound;
            true
        } else {
            self.code -= bound;
            self.range -= bound;
            false
        };
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next_byte() as u32;
        }
        bit
    }
}

impl Coder for Decoder<'_> {
    #[inline]
    fn bit(&mut self, m: &mut Model, _b: bool) -> bool {
        let b = self.decode(m.p1());
        m.update(b);
        b
    }
    #[inline]
    fn bypass(&mut self, _b: bool) -> bool {
        self.decode(PROB_ONE / 2)
    }
}

// ---------------------------------------------------------------- CostCounter

/// Estima el coste en bits sin modificar los modelos.
#[derive(Default)]
pub struct CostCounter {
    pub bits: f32,
}

impl Coder for CostCounter {
    #[inline]
    fn bit(&mut self, m: &mut Model, b: bool) -> bool {
        self.bits += m.cost(b);
        b
    }
    #[inline]
    fn bypass(&mut self, b: bool) -> bool {
        self.bits += 1.0;
        b
    }
}

// ---------------------------------------------------------------- Enteros

/// Contextos para enteros sin signo con un Exp-Golomb adaptativo:
/// prefijo unario (longitud en bits) con un contexto por posición y el bit
/// de mantisa más significativo también con contexto.
#[derive(Clone)]
pub struct UintCtx {
    prefix: [Model; 24],
    mant: [Model; 24],
}

impl Default for UintCtx {
    fn default() -> Self {
        UintCtx {
            prefix: [Model::new(); 24],
            mant: [Model::new(); 24],
        }
    }
}

impl Visit for UintCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.prefix.visit(f);
        self.mant.visit(f);
    }
}

impl Visit for SintCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        f(&mut self.zero);
        f(&mut self.sign);
        self.mag.visit(f);
    }
}

impl Visit for TreeCtx {
    fn visit(&mut self, f: &mut dyn FnMut(&mut Model)) {
        self.nodes.visit(f);
    }
}

impl UintCtx {
    pub fn code<C: Coder>(&mut self, c: &mut C, v: u32) -> u32 {
        let x_enc = v.wrapping_add(1);
        let k_enc = 31 - x_enc.max(1).leading_zeros();
        let mut k = 0u32;
        while k < 23 && c.bit(&mut self.prefix[k as usize], k_enc > k) {
            k += 1;
        }
        let mut x = 1u32;
        if k > 0 {
            let b = c.bit(&mut self.mant[k as usize], (x_enc >> (k - 1)) & 1 == 1);
            x = (x << 1) | b as u32;
            if k > 1 {
                let rest = c.bypass_bits(x_enc, k - 1);
                x = (x << (k - 1)) | rest;
            }
        }
        x - 1
    }

    pub fn cost(&self, v: u32) -> f32 {
        let x = v + 1;
        let k = 31 - x.leading_zeros();
        let mut bits = 0.0;
        for i in 0..k.min(23) {
            bits += self.prefix[i as usize].cost(true);
        }
        if k < 23 {
            bits += self.prefix[k as usize].cost(false);
        }
        if k > 0 {
            bits += self.mant[k as usize].cost((x >> (k - 1)) & 1 == 1) + (k - 1) as f32;
        }
        bits
    }
}

/// Entero con signo: cero / signo / magnitud.
#[derive(Clone, Default)]
pub struct SintCtx {
    zero: Model,
    sign: Model,
    mag: UintCtx,
}

impl SintCtx {
    pub fn code<C: Coder>(&mut self, c: &mut C, v: i32) -> i32 {
        if !c.bit(&mut self.zero, v != 0) {
            return 0;
        }
        let neg = c.bit(&mut self.sign, v < 0);
        let m = self.mag.code(c, v.unsigned_abs().saturating_sub(1)) + 1;
        if neg { -(m as i32) } else { m as i32 }
    }
}

/// Símbolo pequeño (0..2^bits) codificado como árbol binario con contexto por nodo.
#[derive(Clone)]
pub struct TreeCtx {
    bits: u32,
    nodes: Vec<Model>,
}

impl TreeCtx {
    pub fn new(bits: u32) -> Self {
        TreeCtx {
            bits,
            nodes: vec![Model::new(); 1 << bits],
        }
    }

    pub fn code<C: Coder>(&mut self, c: &mut C, v: u32) -> u32 {
        let mut node = 1usize;
        for i in (0..self.bits).rev() {
            let b = c.bit(&mut self.nodes[node], (v >> i) & 1 == 1);
            node = (node << 1) | b as usize;
        }
        (node - (1 << self.bits)) as u32
    }

    pub fn cost(&self, v: u32) -> f32 {
        let mut node = 1usize;
        let mut bits = 0.0;
        for i in (0..self.bits).rev() {
            let b = (v >> i) & 1 == 1;
            bits += self.nodes[node].cost(b);
            node = (node << 1) | b as usize;
        }
        bits
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cost_table_exacta() {
        for (i, &v) in super::COST_TABLE.iter().enumerate() {
            let p = (i as f64 + 0.5) / 4096.0;
            assert_eq!(v, (-p.log2()) as f32, "entrada {i}");
        }
    }


    use super::*;

    #[test]
    fn roundtrip_bits_and_ints() {
        let mut rng = 12345u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let vals: Vec<(bool, u32, i32)> = (0..20000)
            .map(|_| {
                let r = next();
                (
                    (r & 7) == 0,
                    (r >> 8) as u32 % 300,
                    ((r >> 20) as i32 % 50) - 25,
                )
            })
            .collect();

        let mut enc = Encoder::new();
        let mut m = Model::new();
        let mut u = UintCtx::default();
        let mut s = SintCtx::default();
        let mut t = TreeCtx::new(6);
        for &(b, v, sv) in &vals {
            enc.bit(&mut m, b);
            u.code(&mut enc, v);
            s.code(&mut enc, sv);
            t.code(&mut enc, v % 35);
            enc.bypass_bits(v, 9);
        }
        let data = enc.finish();

        let mut dec = Decoder::new(&data);
        let mut m = Model::new();
        let mut u = UintCtx::default();
        let mut s = SintCtx::default();
        let mut t = TreeCtx::new(6);
        for &(b, v, sv) in &vals {
            assert_eq!(dec.bit(&mut m, false), b);
            assert_eq!(u.code(&mut dec, 0), v);
            assert_eq!(s.code(&mut dec, 0), sv);
            assert_eq!(t.code(&mut dec, 0), v % 35);
            assert_eq!(dec.bypass_bits(0, 9), v & 511);
        }
    }
}
