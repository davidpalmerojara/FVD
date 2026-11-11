use std::process::ExitCode;
use std::time::Instant;
use tesela::{Params, metrics};

const USAGE: &str = "\
tesela - códec de imagen (formato .tsl)

USO:
  tesela encode <entrada.png|jpg|gif|webp> <salida.tsl> [-q CALIDAD] [-e ESFUERZO] [--tune balanced|perceptual|psnr] [--screen|--no-screen] [--lossless] [--preview] [--v1] [--444|--420]
             [--aq F] [--cq F] [--qm F] [--qmc F] [--lambda F] [--sin-metadatos]
  tesela decode <entrada.tsl> <salida.png|salida.gif>   (.gif: todos los fotogramas de una animación)
  tesela compare <original.png> <distorsionada.png>      (imprime JSON)
  tesela roundtrip <entrada.png> [opciones de encode]     (imprime JSON con bpp y métricas)
";

fn load_png(path: &str) -> Result<(Vec<u8>, usize, usize), String> {
    let img = image::open(path)
        .map_err(|e| format!("{path}: {e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w as usize, h as usize))
}

/// Carga RGBA (alfa 255 si la imagen no tiene transparencia) con sus
/// metadatos. La orientación EXIF se aplica a los píxeles (la foto nunca sale
/// girada) y en el EXIF guardado queda como «normal».
fn load_rgba(path: &str) -> Result<(Vec<u8>, usize, usize, tesela::codec::Metadata), String> {
    use image::ImageDecoder;
    let err = |e: image::ImageError| format!("{path}: {e}");
    let mut dec = image::ImageReader::open(path)
        .map_err(|e| format!("{path}: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("{path}: {e}"))?
        .into_decoder()
        .map_err(err)?;
    let icc = dec.icc_profile().ok().flatten();
    let mut exif = dec.exif_metadata().ok().flatten();
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).map_err(err)?;
    img.apply_orientation(orientation);
    if let Some(e) = exif.as_mut() {
        reset_exif_orientation(e);
    }
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    Ok((
        img.into_raw(),
        w as usize,
        h as usize,
        tesela::codec::Metadata { icc, exif },
    ))
}

/// Fotogramas de una animación (GIF, APNG o WebP animado), ya compuestos sobre
/// el lienzo completo, con su duración en ms. `None` si la imagen no es animada.
#[allow(clippy::type_complexity)]
fn load_animation(path: &str) -> Result<Option<(Vec<Vec<u8>>, Vec<u32>, usize, usize)>, String> {
    use image::AnimationDecoder;
    let err = |e: image::ImageError| format!("{path}: {e}");
    let open = || -> Result<std::io::BufReader<std::fs::File>, String> {
        Ok(std::io::BufReader::new(
            std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?,
        ))
    };
    let mut head = [0u8; 16];
    {
        use std::io::Read;
        let _ = open()?.read(&mut head);
    }
    let frames = if head.starts_with(b"GIF8") {
        image::codecs::gif::GifDecoder::new(open()?)
            .map_err(err)?
            .into_frames()
    } else if head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        let d = image::codecs::webp::WebPDecoder::new(open()?).map_err(err)?;
        if !d.has_animation() {
            return Ok(None);
        }
        d.into_frames()
    } else if head.starts_with(b"\x89PNG") {
        let d = image::codecs::png::PngDecoder::new(open()?).map_err(err)?;
        if !d.is_apng().map_err(err)? {
            return Ok(None);
        }
        d.apng().map_err(err)?.into_frames()
    } else {
        return Ok(None);
    };
    let frames = frames.collect_frames().map_err(err)?;
    if frames.len() < 2 {
        return Ok(None);
    }
    let (w, h) = frames[0].buffer().dimensions();
    let durations = frames
        .iter()
        .map(|f| {
            let (n, d) = f.delay().numer_denom_ms();
            (n / d.max(1)).max(10)
        })
        .collect();
    let rgba = frames
        .into_iter()
        .map(|f| f.into_buffer().into_raw())
        .collect();
    Ok(Some((rgba, durations, w as usize, h as usize)))
}

/// Guarda todos los fotogramas en un GIF animado (para verlos en cualquier visor).
fn save_gif(path: &str, data: &[u8]) -> Result<(), String> {
    let (a, frames) = tesela::codec::decode_animation(data, 1 << 30)?;
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = image::codecs::gif::GifEncoder::new_with_speed(std::io::BufWriter::new(file), 10);
    enc.set_repeat(if a.loops == 0 {
        image::codecs::gif::Repeat::Infinite
    } else {
        image::codecs::gif::Repeat::Finite(a.loops)
    })
    .map_err(|e| e.to_string())?;
    for (rgba, ms) in frames {
        let buf =
            image::RgbaImage::from_raw(a.width as u32, a.height as u32, rgba).ok_or("buffer")?;
        let frame = image::Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(ms, 1));
        enc.encode_frame(frame).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Pone a 1 («normal») la etiqueta de orientación (0x0112) del IFD0 de un
/// bloque EXIF, que puede empezar por «Exif\0\0» o directamente por la
/// cabecera TIFF.
fn reset_exif_orientation(exif: &mut [u8]) {
    let base = if exif.starts_with(b"Exif\0\0") { 6 } else { 0 };
    let t = &mut exif[base..];
    if t.len() < 8 {
        return;
    }
    let le = match &t[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return,
    };
    let rd16 = |b: &[u8], i: usize| {
        if le {
            u16::from_le_bytes([b[i], b[i + 1]])
        } else {
            u16::from_be_bytes([b[i], b[i + 1]])
        }
    };
    let ifd = if le {
        u32::from_le_bytes(t[4..8].try_into().unwrap())
    } else {
        u32::from_be_bytes(t[4..8].try_into().unwrap())
    } as usize;
    if ifd + 2 > t.len() {
        return;
    }
    let n = rd16(t, ifd) as usize;
    for k in 0..n {
        let e = ifd + 2 + 12 * k;
        if e + 12 > t.len() {
            return;
        }
        if rd16(t, e) == 0x0112 {
            let one = if le {
                1u16.to_le_bytes()
            } else {
                1u16.to_be_bytes()
            };
            t[e + 8..e + 10].copy_from_slice(&one);
        }
    }
}

/// Guarda en PNG, con el perfil de color y el EXIF del archivo si los tiene.
fn save_decoded(path: &str, d: &tesela::Decoded) -> Result<(), String> {
    use image::ImageEncoder;
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    if let Some(icc) = &d.meta.icc {
        enc.set_icc_profile(icc.clone()).ok();
    }
    if let Some(exif) = &d.meta.exif {
        enc.set_exif_metadata(exif.clone()).ok();
    }
    let (buf, ty) = if d.alpha.is_some() {
        (d.rgba(), image::ExtendedColorType::Rgba8)
    } else {
        (d.rgb.clone(), image::ExtendedColorType::Rgb8)
    };
    enc.write_image(&buf, d.width as u32, d.height as u32, ty)
        .map_err(|e| format!("{path}: {e}"))
}

fn parse_params(args: &[String]) -> Result<Params, String> {
    let mut p = Params::default();
    let mut i = 0;
    let num = |i: usize| -> Result<f32, String> {
        args.get(i + 1)
            .ok_or("falta valor")?
            .parse::<f32>()
            .map_err(|e| e.to_string())
    };
    while i < args.len() {
        match args[i].as_str() {
            "-q" => {
                p.quality = num(i)?;
                i += 1;
            }
            "-e" => {
                p.effort = num(i)? as u8;
                i += 1;
            }
            "--tune" => {
                match args.get(i + 1).map(String::as_str) {
                    Some("psnr") => p = p.tune_psnr(),
                    Some("perceptual") => p = p.tune_perceptual(),
                    Some("balanced") => {}
                    _ => return Err("--tune admite: balanced | perceptual | psnr".into()),
                }
                i += 1;
            }
            "--no-restore" => p.restore = false,
            "--screen" => p.screen = Some(true),
            "--lossless" => p.lossless = true,
            "--preview" => p.preview = true,
            "--v1" => p.version = 1,
            "--threads" => {
                p.threads = num(i)? as usize;
                i += 1;
            }
            "--no-screen" => p.screen = Some(false),
            "--444" => p.subsample = Some(false),
            "--420" => p.subsample = Some(true),
            "--aq" => {
                p.aq_strength = num(i)?;
                i += 1;
            }
            "--cq" => {
                p.chroma_q = num(i)?;
                i += 1;
            }
            "--qm" => {
                p.qm_luma = num(i)?;
                i += 1;
            }
            "--qmc" => {
                p.qm_chroma = num(i)?;
                i += 1;
            }
            "--lambda" => {
                p.lambda_k = num(i)?;
                i += 1;
            }
            "--tile" => {
                p.tile = num(i)? as usize;
                i += 1;
            }
            o => return Err(format!("opción desconocida: {o}")),
        }
        i += 1;
    }
    Ok(p)
}

fn metrics_json(a: &[u8], b: &[u8], w: usize, h: usize) -> String {
    format!(
        "\"psnr\": {:.4}, \"psnr_y\": {:.4}, \"ssim_y\": {:.6}, \"ssimulacra2\": {:.4}",
        metrics::psnr_rgb(a, b),
        metrics::psnr_y(a, b),
        metrics::ssim_y(a, b, w, h),
        metrics::ssimulacra2(a, b, w, h)
    )
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("encode") if args.len() >= 3 => {
            let (rgba, w, h, meta) = load_rgba(&args[1])?;
            // --sin-metadatos: no guarda el perfil de color ni el EXIF.
            let strip = args[3..].iter().any(|a| a == "--sin-metadatos");
            let rest: Vec<String> = args[3..]
                .iter()
                .filter(|a| *a != "--sin-metadatos")
                .cloned()
                .collect();
            let mut p = parse_params(&rest)?;
            if !strip {
                p.meta = meta;
            }
            let t = Instant::now();
            if let Some((frames, durations, w, h)) = load_animation(&args[1])? {
                let bytes = tesela::codec::encode_animation(&frames, &durations, w, h, 0, &p)?;
                std::fs::write(&args[2], &bytes).map_err(|e| e.to_string())?;
                let a = tesela::codec::anim_info(&bytes)?.ok_or("animación")?;
                eprintln!(
                    "animación de {} fotogramas ({} guardados), {} bytes, {:.2} s",
                    frames.len(),
                    a.frames.len(),
                    bytes.len(),
                    t.elapsed().as_secs_f64()
                );
                return Ok(());
            }
            let e = tesela::encode_rgba(&rgba, w, h, &p);
            std::fs::write(&args[2], &e.bytes).map_err(|e| e.to_string())?;
            eprintln!(
                "{} bytes, {:.4} bpp, {:.2} s",
                e.bytes.len(),
                e.bytes.len() as f64 * 8.0 / (w * h) as f64,
                t.elapsed().as_secs_f64()
            );
        }
        Some("decode") if args.len() == 3 => {
            let data = std::fs::read(&args[1]).map_err(|e| e.to_string())?;
            if let Some(a) = tesela::codec::anim_info(&data)? {
                if args[2].ends_with(".gif") {
                    return save_gif(&args[2], &data);
                }
                if args[2].ends_with('/') {
                    // Carpeta: un PNG por fotograma (000.png, 001.png…) y sus duraciones.
                    let (an, frames) = tesela::codec::decode_animation(&data, 1 << 30)?;
                    std::fs::create_dir_all(&args[2]).map_err(|e| e.to_string())?;
                    let mut durs = String::new();
                    for (i, (rgba, ms)) in frames.into_iter().enumerate() {
                        image::RgbaImage::from_raw(an.width as u32, an.height as u32, rgba)
                            .ok_or("buffer")?
                            .save(format!("{}{i:03}.png", args[2]))
                            .map_err(|e| e.to_string())?;
                        durs += &format!("{ms}\n");
                    }
                    return std::fs::write(format!("{}duraciones_ms", args[2]), durs)
                        .map_err(|e| e.to_string());
                }
                eprintln!(
                    "animación de {} fotogramas: se guarda el primero (usa una salida .gif para verlos todos)",
                    a.frames.len()
                );
            }
            let d = tesela::decode(&data)?;
            if args[2].ends_with(".rgb") {
                // RGB crudo (para pruebas de determinismo)
                std::fs::write(&args[2], &d.rgb).map_err(|e| e.to_string())?;
            } else {
                save_decoded(&args[2], &d)?;
            }
        }
        Some("compare") if args.len() == 3 => {
            let (a, w, h) = load_png(&args[1])?;
            let (b, w2, h2) = load_png(&args[2])?;
            if (w, h) != (w2, h2) {
                return Err("dimensiones distintas".into());
            }
            println!("{{{}}}", metrics_json(&a, &b, w, h));
        }
        Some("roundtrip") if args.len() >= 2 => {
            let (rgba, w, h, _) = load_rgba(&args[1])?;
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let p = parse_params(&args[2..])?;
            let t = Instant::now();
            let e = tesela::encode_rgba(&rgba, w, h, &p);
            let te = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let d = tesela::decode(&e.bytes)?;
            let td = t.elapsed().as_secs_f64();
            if p.lossless && d.rgba() != rgba {
                return Err("¡el modo sin pérdida no reproduce el original!".into());
            }
            if d.rgb != e.recon_rgb || d.alpha != e.recon_alpha {
                return Err(
                    "¡el decodificador no coincide con la reconstrucción del codificador!".into(),
                );
            }
            println!(
                "{{\"bytes\": {}, \"bpp\": {:.5}, \"enc_s\": {:.3}, \"dec_s\": {:.3}, {}}}",
                e.bytes.len(),
                e.bytes.len() as f64 * 8.0 / (w * h) as f64,
                te,
                td,
                metrics_json(&rgb, &d.rgb, w, h)
            );
        }
        Some("train") if args.len() >= 3 => train(&args[1], &args[2..])?,
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

/// Entrena las probabilidades iniciales de los contextos: codifica las
/// imágenes en varias calidades por banda y promedia el estado final de cada
/// modelo (ponderado por su nº de observaciones). Hay un juego de tablas para
/// fotografía y otro para contenido sintético (según la detección automática).
fn train(out: &str, imgs: &[String]) -> Result<(), String> {
    use rayon::prelude::*;
    use tesela::codec::{
        TOOL_SCREEN, UNTRAINED, encode_tile_stats, make_header, quality_band, quality_to_qstep,
    };
    UNTRAINED.store(true, std::sync::atomic::Ordering::Relaxed);
    let images: Vec<(Vec<u8>, usize, usize)> =
        imgs.iter().map(|p| load_png(p)).collect::<Result<_, _>>()?;
    let qualities = [
        97.0, 93.0, 89.0, 85.0, 80.0, 74.0, 66.0, 58.0, 48.0, 38.0, 28.0, 20.0,
    ];
    let mut tables = vec![vec![Vec::<u16>::new(); 4]; 2];
    for (kind, kind_tables) in tables.iter_mut().enumerate() {
        let sel: Vec<usize> = (0..images.len())
            .filter(|&i| {
                let (rgb, w, h) = &images[i];
                let hd = make_header(rgb, *w, *h, &Params::default());
                ((hd.tools & TOOL_SCREEN != 0) as usize) == kind
            })
            .collect();
        eprintln!(
            "{}: {} imágenes",
            ["fotografía", "sintético"][kind],
            sel.len()
        );
        if sel.is_empty() {
            continue;
        }
        for (band, table) in kind_tables.iter_mut().enumerate() {
            let qs: Vec<f32> = qualities
                .iter()
                .copied()
                .filter(|&q| quality_band(quality_to_qstep(q)) == band)
                .collect();
            let jobs: Vec<(usize, f32)> = sel
                .iter()
                .flat_map(|&i| qs.iter().map(move |&q| (i, q)))
                .collect();
            if jobs.is_empty() {
                continue;
            }
            let stats: Vec<Vec<(u32, u32)>> = jobs
                .par_iter()
                .map(|&(i, q)| {
                    let (rgb, w, h) = &images[i];
                    let p = Params {
                        quality: q,
                        tile: 4096,
                        ..Params::default()
                    };
                    let hd = make_header(rgb, *w, *h, &p);
                    encode_tile_stats(rgb, None, *w, *h, &hd, &p).3
                })
                .collect();
            let n = stats[0].len();
            *table = (0..n)
                .map(|k| {
                    let (mut sw, mut sp) = (0f64, 0f64);
                    for s in &stats {
                        let (p, c) = s[k];
                        if c > 0 {
                            sw += c as f64;
                            sp += c as f64 * p as f64;
                        }
                    }
                    if sw > 0.0 {
                        (sp / sw).round() as u16
                    } else {
                        32768
                    }
                })
                .collect();
            eprintln!(
                "  banda {band}: {n} modelos, {} codificaciones",
                stats.len()
            );
        }
    }
    let mut s = String::from(
        "//! Probabilidades iniciales de la v2 (generado con `tesela train`).\n\
         //! TABLES[sintético][banda de calidad], en el orden de visita de los modelos.\n\n\
         pub static TABLES: [[&[u16]; 4]; 2] = [\n",
    );
    for kind in &tables {
        s.push_str("    [\n");
        for t in kind {
            s.push_str("        &[");
            for (i, v) in t.iter().enumerate() {
                if i % 16 == 0 {
                    s.push_str("\n            ");
                }
                s.push_str(&format!("{v}, "));
            }
            s.push_str("\n        ],\n");
        }
        s.push_str("    ],\n");
    }
    s.push_str("];\n");
    std::fs::write(out, s).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
