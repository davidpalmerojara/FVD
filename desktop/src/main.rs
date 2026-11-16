//! Tesela para escritorio: visor y conversor de imágenes .tsl.
//!
//! El códec se ejecuta aquí, en Rust nativo (el crate `tesela` del
//! repositorio); la interfaz (`ui/`) es HTML, CSS y JS sin compilar, que
//! recibe los píxeles ya decodificados por IPC binario.

// En Windows, sin ventana de consola detrás de la aplicación.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod load;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::Serialize;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

/// Formatos que acepta el conversor (las funciones de `image` activadas en Cargo.toml).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Límite de memoria para reproducir una animación: fotogramas × píxeles del
/// lienzo (2²⁷ píxeles RGBA = 512 MB). Por encima se muestra solo el primero.
const MAX_ANIM_PIXELS: u64 = 1 << 27;

/// Archivo abierto en el visor.
struct Doc {
    path: PathBuf,
    width: usize,
    height: usize,
    /// Lienzos RGBA completos con su duración en ms (uno solo si es fija).
    frames: Vec<(Vec<u8>, u32)>,
    meta: tesela::codec::Metadata,
}

#[derive(Default)]
struct AppState {
    doc: Mutex<Option<Doc>>,
    /// Archivo con el que se abrió la aplicación (argumento o, en macOS, el
    /// evento `Opened`) hasta que la interfaz lo pide.
    pending: Mutex<Vec<String>>,
    /// La interfaz ya escucha el evento `abrir`.
    ready: AtomicBool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocInfo {
    path: String,
    name: String,
    width: usize,
    height: usize,
    bytes: u64,
    /// Duración de cada fotograma (vacío si la imagen es fija).
    durations: Vec<u32>,
    /// Repeticiones de la animación (0 = sin fin).
    loops: u16,
    /// Fotogramas que tiene el archivo (puede ser > durations.len() si la
    /// animación no cabía en memoria y solo se muestra el primero).
    frame_count: usize,
    alpha: bool,
    lossless: bool,
    version: u8,
    /// Posición en su carpeta (1..=total) entre los .tsl.
    index: usize,
    total: usize,
    decode_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Converted {
    input: String,
    output: String,
    original_bytes: u64,
    tsl_bytes: u64,
    width: usize,
    height: usize,
    frames: usize,
    millis: u64,
}

fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Los .tsl de la carpeta de `path`, ordenados por nombre (sin distinguir
/// mayúsculas, con los números en orden natural: «2» antes que «10»).
fn siblings(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else {
        return vec![path.to_path_buf()];
    };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && has_ext(p, &["tsl"]))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_cached_key(|p| natural_key(&file_name(p)));
    v
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Chunk {
    Num(u64, usize),
    Text(String),
}

fn natural_key(s: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(d);
                chars.next();
            }
            out.push(Chunk::Num(n.parse().unwrap_or(u64::MAX), n.len()));
        } else {
            let mut t = String::new();
            while let Some(&d) = chars.peek().filter(|d| !d.is_ascii_digit()) {
                t.extend(d.to_lowercase());
                chars.next();
            }
            out.push(Chunk::Text(t));
        }
    }
    out
}

fn decode_doc(path: &Path) -> Result<(Doc, DocInfo), String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let t = Instant::now();
    let header = tesela::codec::Header::read(&data)?;
    let anim = tesela::codec::anim_info(&data)?;
    let (width, height, frames, meta, loops, frame_count, alpha) = match &anim {
        Some(a) if a.frames.len() as u64 * (a.width * a.height) as u64 <= MAX_ANIM_PIXELS => {
            let (a, frames) = tesela::codec::decode_animation(&data, MAX_ANIM_PIXELS)?;
            let n = frames.len();
            let alpha = frames
                .iter()
                .any(|(f, _)| f.chunks_exact(4).any(|p| p[3] != 255));
            (a.width, a.height, frames, a.meta, a.loops, n, alpha)
        }
        _ => {
            // Imagen fija (o animación demasiado grande: solo el primer fotograma).
            let d = tesela::decode(&data)?;
            let alpha = d.alpha.is_some();
            let rgba = d.rgba();
            let n = anim.as_ref().map_or(1, |a| a.frames.len());
            let loops = anim.as_ref().map_or(0, |a| a.loops);
            (d.width, d.height, vec![(rgba, 0)], d.meta, loops, n, alpha)
        }
    };
    let decode_ms = t.elapsed().as_millis() as u64;
    let list = siblings(path);
    let index = list.iter().position(|p| p == path).map_or(1, |i| i + 1);
    let info = DocInfo {
        path: path.to_string_lossy().into_owned(),
        name: file_name(path),
        width,
        height,
        bytes: data.len() as u64,
        durations: if anim.is_some() && frames.len() > 1 {
            frames.iter().map(|f| f.1).collect()
        } else {
            Vec::new()
        },
        loops,
        frame_count,
        alpha,
        lossless: header.tools & tesela::codec::TOOL_LOSSLESS != 0,
        version: header.version,
        index,
        total: list.len().max(index),
        decode_ms,
    };
    let doc = Doc {
        path: path.to_path_buf(),
        width,
        height,
        frames,
        meta,
    };
    Ok((doc, info))
}

/// Abre un .tsl: lo decodifica (fuera del hilo de la interfaz) y lo deja en
/// memoria para que la interfaz pida los fotogramas con `frame`.
#[tauri::command]
async fn open_tsl(path: String, state: State<'_, AppState>) -> Result<DocInfo, String> {
    let p = PathBuf::from(&path);
    let p = std::fs::canonicalize(&p).unwrap_or(p);
    let (doc, info) = tauri::async_runtime::spawn_blocking(move || decode_doc(&p))
        .await
        .map_err(|e| e.to_string())??;
    *state.doc.lock().unwrap() = Some(doc);
    Ok(info)
}

/// Píxeles RGBA de un fotograma del archivo abierto, en binario.
#[tauri::command]
fn frame(index: usize, state: State<'_, AppState>) -> Result<Response, String> {
    let doc = state.doc.lock().unwrap();
    let doc = doc.as_ref().ok_or("no hay ningún archivo abierto")?;
    let f = doc.frames.get(index).ok_or("fotograma inexistente")?;
    Ok(Response::new(f.0.clone()))
}

/// El .tsl anterior (`step` = -1) o siguiente (+1) de la misma carpeta, con
/// la lista circular.
#[tauri::command]
fn neighbor(path: String, step: i32) -> Option<String> {
    let p = PathBuf::from(&path);
    let list = siblings(&p);
    if list.is_empty() {
        return None;
    }
    let n = list.len() as i64;
    let i = match list.iter().position(|q| *q == p) {
        Some(i) => (i as i64 + step as i64).rem_euclid(n),
        None => 0,
    };
    Some(list[i as usize].to_string_lossy().into_owned())
}

/// Guarda el fotograma `index` del archivo abierto como PNG, preguntando
/// dónde. Devuelve la ruta, o `None` si se cancela el diálogo.
#[tauri::command]
async fn save_png(
    index: usize,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let (dir, stem) = {
        let doc = state.doc.lock().unwrap();
        let doc = doc.as_ref().ok_or("no hay ningún archivo abierto")?;
        let stem = doc
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "imagen".into());
        let stem = if doc.frames.len() > 1 {
            format!("{stem}-{:03}", index + 1)
        } else {
            stem
        };
        (doc.path.parent().map(Path::to_path_buf), stem)
    };
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Guardar como PNG")
        .add_filter("PNG", &["png"])
        .set_file_name(format!("{stem}.png"));
    if let Some(d) = dir {
        dialog = dialog.set_directory(d);
    }
    let Some(out) = dialog.blocking_save_file() else {
        return Ok(None);
    };
    let mut out = out.into_path().map_err(|e| e.to_string())?;
    if !has_ext(&out, &["png"]) {
        out.set_extension("png");
    }
    let doc = state.doc.lock().unwrap();
    let doc = doc.as_ref().ok_or("no hay ningún archivo abierto")?;
    let rgba = &doc.frames.get(index).ok_or("fotograma inexistente")?.0;
    write_png(&out, rgba, doc.width, doc.height, &doc.meta)?;
    Ok(Some(out.to_string_lossy().into_owned()))
}

/// PNG con el perfil de color y el EXIF del archivo, como `tesela decode`
/// (RGB si todo es opaco, RGBA si hay transparencia).
fn write_png(
    path: &Path,
    rgba: &[u8],
    w: usize,
    h: usize,
    meta: &tesela::codec::Metadata,
) -> Result<(), String> {
    use image::ImageEncoder;
    let err = |e: image::ImageError| format!("{}: {e}", path.display());
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    if let Some(icc) = &meta.icc {
        enc.set_icc_profile(icc.clone()).ok();
    }
    if let Some(exif) = &meta.exif {
        enc.set_exif_metadata(exif.clone()).ok();
    }
    if rgba.chunks_exact(4).all(|p| p[3] == 255) {
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        enc.write_image(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)
            .map_err(err)
    } else {
        enc.write_image(rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
            .map_err(err)
    }
}

/// Primera ruta libre: `foto.tsl`, `foto (2).tsl`, `foto (3).tsl`…
fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.tsl"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}).tsl")))
        .find(|p| !p.exists())
        .unwrap()
}

fn convert_file(
    input: &Path,
    quality: f32,
    lossless: bool,
    out_dir: Option<&Path>,
) -> Result<Converted, String> {
    let name = input.to_string_lossy().into_owned();
    let original_bytes = std::fs::metadata(input)
        .map_err(|e| format!("{name}: {e}"))?
        .len();
    let t = Instant::now();
    // Lo mismo que `tesela encode entrada salida -q CALIDAD [--lossless]`.
    let (rgba, w, h, meta) = load::load_rgba(&name)?;
    let p = tesela::Params {
        quality,
        lossless,
        meta,
        ..tesela::Params::default()
    };
    let (bytes, frames) = match load::load_animation(&name)? {
        Some((frames, durations, w, h)) => {
            let n = frames.len();
            (
                tesela::codec::encode_animation(&frames, &durations, w, h, 0, &p)?,
                n,
            )
        }
        None => (tesela::encode_rgba(&rgba, w, h, &p).bytes, 1),
    };
    let dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => input
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "imagen".into());
    let output = free_path(&dir, &stem);
    std::fs::write(&output, &bytes).map_err(|e| format!("{}: {e}", output.display()))?;
    Ok(Converted {
        input: name,
        output: output.to_string_lossy().into_owned(),
        original_bytes,
        tsl_bytes: bytes.len() as u64,
        width: w,
        height: h,
        frames,
        millis: t.elapsed().as_millis() as u64,
    })
}

/// Convierte una imagen a .tsl en un hilo aparte (la interfaz sigue
/// respondiendo). Sin `out_dir`, el .tsl se escribe junto al original.
#[tauri::command]
async fn convert(
    path: String,
    quality: f32,
    lossless: bool,
    out_dir: Option<String>,
) -> Result<Converted, String> {
    tauri::async_runtime::spawn_blocking(move || {
        convert_file(
            Path::new(&path),
            quality.clamp(0.0, 100.0),
            lossless,
            out_dir.as_deref().map(Path::new),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pick_tsl(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .set_title("Abrir imagen Tesela")
        .add_filter("Imagen Tesela", &["tsl"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn pick_images(app: AppHandle) -> Vec<String> {
    app.dialog()
        .file()
        .set_title("Elegir imágenes para convertir")
        .add_filter("Imágenes", IMAGE_EXTS)
        .blocking_pick_files()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .set_title("Carpeta de destino")
        .blocking_pick_folder()
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Rutas con las que se abrió la aplicación (doble clic, «Abrir con» o la
/// línea de órdenes). A partir de aquí llegan con el evento `abrir`.
#[tauri::command]
fn initial_paths(state: State<'_, AppState>) -> Vec<String> {
    state.ready.store(true, Ordering::SeqCst);
    std::mem::take(&mut *state.pending.lock().unwrap())
}

/// Revela un archivo en el gestor de archivos del sistema.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{}", p.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(&p).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = std::process::Command::new("xdg-open")
        .arg(p.parent().unwrap_or(Path::new(".")))
        .spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// Encola rutas para la interfaz o, si ya está lista, se las envía.
#[cfg_attr(not(any(target_os = "macos", target_os = "ios")), allow(dead_code))]
fn deliver(app: &AppHandle, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let state = app.state::<AppState>();
    if state.ready.load(Ordering::SeqCst) {
        let _ = app.emit("abrir", paths);
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.set_focus();
        }
    } else {
        state.pending.lock().unwrap().extend(paths);
    }
}

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        // macOS antiguo añade «-psn_…»; las opciones no son archivos.
        .filter(|p| !p.to_string_lossy().starts_with('-'))
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            pending: Mutex::new(args),
            ..AppState::default()
        })
        .invoke_handler(tauri::generate_handler![
            open_tsl,
            frame,
            neighbor,
            save_png,
            convert,
            pick_tsl,
            pick_images,
            pick_folder,
            initial_paths,
            reveal
        ])
        .build(tauri::generate_context!())
        .expect("no se pudo iniciar Tesela");

    app.run(|_app, _event| {
        // macOS entrega los archivos abiertos desde el Finder con este evento,
        // no como argumentos.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let tauri::RunEvent::Opened { urls } = _event {
            let paths = urls
                .into_iter()
                .filter_map(|u| u.to_file_path().ok())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            deliver(_app, paths);
        }
    });
}
