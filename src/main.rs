use tesela::{Params, metrics};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
tesela - códec de imagen (formato .tsl)

USO:
  tesela encode <entrada.png> <salida.tsl> [-q CALIDAD] [-e ESFUERZO] [--tune perceptual|psnr] [--444|--420]
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
                    Some("perceptual") => {}
                    _ => return Err("--tune admite: perceptual | psnr".into()),
                }
                i += 1;
            }
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
        _ => return Err(USAGE.into()),
    }
    Ok(())
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
