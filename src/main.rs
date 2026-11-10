use tesela::{Params, metrics};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
tesela - códec de imagen (formato .tsl)

USO:
  tesela encode <entrada.png> <salida.tsl> [-q CALIDAD] [-e ESFUERZO] [--tune balanced|perceptual|psnr] [--444|--420]
             [--aq F] [--cq F] [--qm F] [--qmc F] [--lambda F] [--deblock N]
  tesela decode <entrada.tsl> <salida.png>
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

fn save_png(path: &str, rgb: Vec<u8>, w: usize, h: usize) -> Result<(), String> {
    image::RgbImage::from_raw(w as u32, h as u32, rgb)
        .ok_or("buffer")?
        .save(path)
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
            "--deblock" => {
                p.deblock = Some(num(i)? as u8);
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
            let (rgb, w, h) = load_png(&args[1])?;
            let p = parse_params(&args[3..])?;
            let t = Instant::now();
            let e = tesela::encode(&rgb, w, h, &p);
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
            let d = tesela::decode(&data)?;
            save_png(&args[2], d.rgb, d.width, d.height)?;
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
            let (rgb, w, h) = load_png(&args[1])?;
            let p = parse_params(&args[2..])?;
            let t = Instant::now();
            let e = tesela::encode(&rgb, w, h, &p);
            let te = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let d = tesela::decode(&e.bytes)?;
            let td = t.elapsed().as_secs_f64();
            if d.rgb != e.recon_rgb {
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
/// modelo (ponderado por su nº de observaciones).
fn train(out: &str, imgs: &[String]) -> Result<(), String> {
    use rayon::prelude::*;
    use tesela::codec::{UNTRAINED, encode_tile_stats, make_header, quality_band, quality_to_qstep};
    UNTRAINED.store(true, std::sync::atomic::Ordering::Relaxed);
    let images: Vec<(Vec<u8>, usize, usize)> = imgs.iter().map(|p| load_png(p)).collect::<Result<_, _>>()?;
    let qualities = [92.0, 85.0, 78.0, 70.0, 62.0, 55.0, 47.0, 40.0, 32.0, 24.0, 16.0];
    let mut tables = vec![Vec::<u16>::new(); 4];
    for band in 0..4 {
        let qs: Vec<f32> = qualities.iter().copied().filter(|&q| quality_band(quality_to_qstep(q)) == band).collect();
        let jobs: Vec<(usize, f32)> = (0..images.len()).flat_map(|i| qs.iter().map(move |&q| (i, q))).collect();
        let stats: Vec<Vec<(u32, u32)>> = jobs
            .par_iter()
            .map(|&(i, q)| {
                let (rgb, w, h) = &images[i];
                let p = Params { quality: q, tile: 4096, ..Params::default() };
                let hd = make_header(*w, *h, &p);
                encode_tile_stats(rgb, *w, *h, &hd, &p).2
            })
            .collect();
        let n = stats[0].len();
        let mut t = vec![0u16; n];
        for k in 0..n {
            let (mut sw, mut sp) = (0f64, 0f64);
            for s in &stats {
                let (p, c) = s[k];
                if c > 0 {
                    sw += c as f64;
                    sp += c as f64 * p as f64;
                }
            }
            t[k] = if sw > 0.0 { (sp / sw).round() as u16 } else { 32768 };
        }
        eprintln!("banda {band}: {} modelos, {} codificaciones", n, stats.len());
        tables[band] = t;
    }
    let mut s = String::from(
        "//! Probabilidades iniciales entrenadas (generado con `tesela train`).\n\
         //! Una tabla por banda de calidad; si no coincide con el nº de modelos\n\
         //! actual se ignora y todos los contextos empiezan en 0,5.\n\n\
         pub static TABLES: [&[u16]; 4] = [\n",
    );
    for t in &tables {
        s.push_str("    &[");
        for (i, v) in t.iter().enumerate() {
            if i % 16 == 0 {
                s.push_str("\n        ");
            }
            s.push_str(&format!("{v}, "));
        }
        s.push_str("\n    ],\n");
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
