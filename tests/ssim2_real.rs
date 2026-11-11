//! SSIMULACRA2 por filas frente al crate con fotos reales (si están las de
//! bench/data/kodak). `cargo test --release -- --ignored ssim2`
use std::time::Instant;

#[test]
#[ignore]
fn ssim2_fotos_reales() {
    let mut files: Vec<_> = std::fs::read_dir("bench/data/kodak")
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    if let Ok(extra) = std::env::var("SSIM2_EXTRA") {
        files.push(extra.into());
    }
    for f in files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "png"))
        .take(6)
        .chain(files.iter().filter(|f| f.to_string_lossy().contains("big")))
    {
        let img = image::open(f).unwrap().to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let a = img.into_raw();
        for q in [30.0, 80.0] {
            let p = tesela::Params {
                quality: q,
                ..Default::default()
            };
            let enc = tesela::encode(&a, w, h, &p);
            let b = tesela::decode(&enc.bytes).unwrap().rgb;
            let t = Instant::now();
            let r = tesela::metrics::ssimulacra2_reference(&a, &b, w, h);
            let tr = t.elapsed();
            let t = Instant::now();
            let n = tesela::metrics::ssimulacra2(&a, &b, w, h);
            let tn = t.elapsed();
            println!(
                "{} q{q}: {r:.6} / {n:.6}  ({:.2} s / {:.2} s)",
                f.display(),
                tr.as_secs_f64(),
                tn.as_secs_f64()
            );
            assert_eq!(r.to_bits(), n.to_bits());
        }
    }
}
