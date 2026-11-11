//! Interfaz WebAssembly (ABI C, sin dependencias de bindgen).
//!
//! Convención: las funciones devuelven un puntero a un búfer reservado aquí
//! cuyo contenido empieza por su longitud total (u32 LE, incluida la propia
//! cabecera de 4 bytes). JavaScript lo lee y lo libera con `tsl_free`.
//! Un puntero nulo indica error (mensaje en `tsl_last_error`).

use crate::codec::{self, Header, Params};
use std::cell::RefCell;

thread_local! {
    static LAST_ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_error(msg: &str) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.as_bytes().to_vec());
}

/// Empaqueta `body` en un búfer [len u32][body] y lo cede a JavaScript.
fn give(body: &[u8]) -> *mut u8 {
    let mut v = Vec::with_capacity(body.len() + 4);
    v.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    v.extend_from_slice(body);
    Box::into_raw(v.into_boxed_slice()) as *mut u8
}

unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn tsl_alloc(len: usize) -> *mut u8 {
    Box::into_raw(vec![0u8; len.max(1)].into_boxed_slice()) as *mut u8
}

/// Libera un búfer de `tsl_alloc` (con su longitud) o devuelto por otra función
/// (con la longitud leída de su cabecera).
///
/// # Safety
/// `ptr`/`len` deben describir un búfer reservado por este módulo.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tsl_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len.max(1))) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn tsl_last_error() -> *mut u8 {
    LAST_ERROR.with(|e| give(&e.borrow()))
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

fn alpha_of(rgba: &[u8]) -> Option<Vec<u8>> {
    let a: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
    a.iter().any(|&v| v != 255).then_some(a)
}

fn to_rgba(rgb: &[u8], alpha: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for (i, p) in rgb.chunks_exact(3).enumerate() {
        out.extend_from_slice(&[p[0], p[1], p[2], alpha.map_or(255, |a| a[i])]);
    }
    out
}

fn params(quality: f32, effort: u32, tune: u32) -> Params {
    let p = Params {
        quality,
        effort: effort as u8,
        ..Params::default()
    };
    match tune {
        1 => p.tune_perceptual(),
        2 => p.tune_psnr(),
        _ => p,
    }
}

/// Cabecera para una imagen RGBA de `w × h` con estos parámetros (analiza el
/// contenido para decidir las herramientas).
///
/// # Safety
/// `rgba` debe apuntar a `w*h*4` bytes válidos.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tsl_header(
    rgba: *const u8,
    w: u32,
    h: u32,
    quality: f32,
    effort: u32,
    tune: u32,
) -> *mut u8 {
    let px = unsafe { slice(rgba, w as usize * h as usize * 4) };
    let mut hd = codec::make_header(
        &rgba_to_rgb(px),
        w as usize,
        h as usize,
        &params(quality, effort, tune),
    );
    if alpha_of(px).is_some() {
        hd.tools |= codec::TOOL_ALPHA;
    }
    give(&codec::assemble(&hd, &[])[..codec::HEADER_SIZE])
}

/// Codifica una imagen RGBA completa. Devuelve el archivo .tsl.
///
/// # Safety
/// `rgba` debe apuntar a `w*h*4` bytes válidos.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tsl_encode(
    rgba: *const u8,
    w: u32,
    h: u32,
    quality: f32,
    effort: u32,
    tune: u32,
) -> *mut u8 {
    let (w, h) = (w as usize, h as usize);
    let e = codec::encode_rgba(
        unsafe { slice(rgba, w * h * 4) },
        w,
        h,
        &params(quality, effort, tune),
    );
    give(&e.bytes)
}

/// Codifica una tesela RGBA (`tw × th`) con la cabecera de su imagen
/// (de `tsl_header`). Devuelve la carga útil (para ensamblar en JavaScript).
///
/// # Safety
/// `rgba` debe apuntar a `tw*th*4` bytes y `hdr` a `hdr_len` bytes válidos.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn tsl_encode_tile(
    rgba: *const u8,
    tw: u32,
    th: u32,
    hdr: *const u8,
    hdr_len: usize,
    quality: f32,
    effort: u32,
    tune: u32,
) -> *mut u8 {
    let (tw, th) = (tw as usize, th as usize);
    let px = unsafe { slice(rgba, tw * th * 4) };
    let rgb = rgba_to_rgb(px);
    let alpha: Vec<u8> = px.chunks_exact(4).map(|p| p[3]).collect();
    let p = params(quality, effort, tune);
    let hd = match Header::read(unsafe { slice(hdr, hdr_len) }) {
        Ok(h) => h,
        Err(e) => {
            set_error(&e);
            return std::ptr::null_mut();
        }
    };
    let a = (hd.tools & codec::TOOL_ALPHA != 0).then_some(&alpha[..]);
    let (payload, _, _) = codec::encode_tile(&rgb, a, tw, th, &hd, &p);
    give(&payload)
}

/// Decodifica la carga útil de una tesela de `tw × th` a RGBA.
///
/// # Safety
/// Punteros y longitudes deben ser válidos.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tsl_decode_tile(
    hdr: *const u8,
    hdr_len: usize,
    payload: *const u8,
    payload_len: usize,
    tw: u32,
    th: u32,
) -> *mut u8 {
    let hd = match Header::read(unsafe { slice(hdr, hdr_len) }) {
        Ok(h) => h,
        Err(e) => {
            set_error(&e);
            return std::ptr::null_mut();
        }
    };
    let (rgb, alpha) = codec::decode_tile(
        unsafe { slice(payload, payload_len) },
        tw as usize,
        th as usize,
        &hd,
    );
    give(&to_rgba(&rgb, alpha.as_deref()))
}

/// Decodifica un archivo completo. Devuelve [w u32][h u32][RGBA].
///
/// # Safety
/// `data` debe apuntar a `len` bytes válidos.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tsl_decode(data: *const u8, len: usize) -> *mut u8 {
    match codec::decode(unsafe { slice(data, len) }) {
        Ok(d) => {
            let mut body = Vec::with_capacity(8 + d.width * d.height * 4);
            body.extend_from_slice(&(d.width as u32).to_le_bytes());
            body.extend_from_slice(&(d.height as u32).to_le_bytes());
            body.extend_from_slice(&d.rgba());
            give(&body)
        }
        Err(e) => {
            set_error(&e);
            std::ptr::null_mut()
        }
    }
}

/// Métricas entre dos imágenes RGBA: [psnr, psnr_y, ssim_y, ssimulacra2] (f64).
///
/// # Safety
/// `a` y `b` deben apuntar a `w*h*4` bytes válidos.
#[cfg(feature = "metrics")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tsl_metrics(a: *const u8, b: *const u8, w: u32, h: u32) -> *mut u8 {
    use crate::metrics;
    let (w, h) = (w as usize, h as usize);
    let a = rgba_to_rgb(unsafe { slice(a, w * h * 4) });
    let b = rgba_to_rgb(unsafe { slice(b, w * h * 4) });
    let vals = [
        metrics::psnr_rgb(&a, &b),
        metrics::psnr_y(&a, &b),
        metrics::ssim_y(&a, &b, w, h),
        if w >= 8 && h >= 8 {
            metrics::ssimulacra2(&a, &b, w, h)
        } else {
            f64::NAN
        },
    ];
    let body: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
    give(&body)
}
