//! Vectores de conformidad del formato Tesela v1.
//!
//! `tests/vectors/*.tsl` son archivos de referencia y `manifest.txt` guarda,
//! para cada uno, el tamaño y el hash FNV-1a de 64 bits de los píxeles RGBA
//! decodificados. Si un cambio en el decodificador altera la salida de un
//! archivo v1, este test falla: la v1 está congelada.
//!
//! Regenerar los vectores (solo al crear una versión nueva del formato):
//!     cargo test --release --test conformance -- --ignored generate_vectors

use std::path::PathBuf;
use tesela::{Params, decode, encode_rgba};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
}

pub fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[test]
fn conformance() {
    let manifest = std::fs::read_to_string(dir().join("manifest.txt")).expect("manifest.txt");
    let mut n = 0;
    for line in manifest
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (name, w, h, hash) = (
            f[0],
            f[1].parse::<usize>().unwrap(),
            f[2].parse::<usize>().unwrap(),
            f[3],
        );
        let data = std::fs::read(dir().join(name)).unwrap();
        let d = decode(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((d.width, d.height), (w, h), "{name}: dimensiones");
        assert_eq!(
            format!("{:016x}", fnv64(&d.rgba())),
            hash,
            "{name}: la salida decodificada cambió"
        );
        n += 1;
    }
    assert!(n >= 10, "faltan vectores");
}

// --------------------------------------------------------------- Generación

fn sample_crop(w: usize, h: usize) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/compare/sample.png");
    let img = image::open(path).unwrap().to_rgba8();
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (ox, oy) = if w > 300 { (200, 100) } else { (300, 100) };
            out.extend_from_slice(&img.get_pixel((ox + x) as u32, (oy + y) as u32).0);
        }
    }
    out
}

fn screen_rgba(w: usize, h: usize) -> Vec<u8> {
    let mut v = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let glyph =
                y % 12 < 9 && x % 7 < 5 && ((x / 7 * 7 + (x % 7) * 3 + (y % 12) * 5) % 11) < 4;
            let px = if x > w * 2 / 3 && y < h / 3 {
                [37, 99, 235]
            } else if glyph && (y / 12) % 3 != 2 {
                [30, 30, 40]
            } else {
                [245, 245, 245]
            };
            v[i..i + 4].copy_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    v
}

#[test]
#[ignore]
fn generate_vectors() {
    let photo = sample_crop(192, 128);
    let mut alpha = photo.clone();
    for (i, px) in alpha.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i % 192) as f32 - 96.0, (i / 192) as f32 - 64.0);
        px[3] = (255.0 - (x * x + y * y).sqrt() * 3.0).clamp(0.0, 255.0) as u8;
    }
    let base = Params::default();
    let cases: Vec<(&str, Vec<u8>, usize, usize, Params)> = vec![
        (
            "foto_q30.tsl",
            photo.clone(),
            192,
            128,
            Params {
                quality: 30.0,
                ..base.clone()
            },
        ),
        (
            "foto_q75.tsl",
            photo.clone(),
            192,
            128,
            Params {
                quality: 75.0,
                ..base.clone()
            },
        ),
        (
            "foto_grande_q50.tsl",
            sample_crop(448, 320),
            448,
            320,
            Params {
                quality: 50.0,
                ..base.clone()
            },
        ),
        (
            "foto_q97_444.tsl",
            photo.clone(),
            192,
            128,
            Params {
                quality: 97.0,
                ..base.clone()
            },
        ),
        (
            "foto_psnr.tsl",
            photo.clone(),
            192,
            128,
            Params {
                quality: 60.0,
                ..base.clone()
            }
            .tune_psnr(),
        ),
        (
            "foto_teselas.tsl",
            sample_crop(200, 150),
            200,
            150,
            Params {
                quality: 70.0,
                tile: 64,
                ..base.clone()
            },
        ),
        (
            "foto_vista_previa.tsl",
            photo.clone(),
            192,
            128,
            Params {
                quality: 80.0,
                preview: true,
                ..base.clone()
            },
        ),
        (
            "captura.tsl",
            screen_rgba(160, 120),
            160,
            120,
            Params {
                quality: 80.0,
                screen: Some(true),
                ..base.clone()
            },
        ),
        (
            "captura_q40.tsl",
            screen_rgba(160, 120),
            160,
            120,
            Params {
                quality: 40.0,
                screen: Some(true),
                ..base.clone()
            },
        ),
        (
            "alfa.tsl",
            alpha.clone(),
            192,
            128,
            Params {
                quality: 70.0,
                ..base.clone()
            },
        ),
        (
            "sin_perdida.tsl",
            photo.clone(),
            192,
            128,
            Params {
                lossless: true,
                ..base.clone()
            },
        ),
        (
            "sin_perdida_alfa.tsl",
            alpha,
            192,
            128,
            Params {
                lossless: true,
                ..base.clone()
            },
        ),
        (
            "diminuta_7x5.tsl",
            sample_crop(7, 5),
            7,
            5,
            Params {
                quality: 60.0,
                ..base.clone()
            },
        ),
        (
            "diminuta_1x1.tsl",
            sample_crop(1, 1),
            1,
            1,
            Params {
                quality: 60.0,
                ..base
            },
        ),
    ];
    let mut manifest = String::from("# archivo ancho alto fnv64(rgba decodificado)\n");
    for (name, rgba, w, h, p) in cases {
        let e = encode_rgba(&rgba, w, h, &p);
        std::fs::write(dir().join(name), &e.bytes).unwrap();
        let d = decode(&e.bytes).unwrap();
        if p.lossless {
            assert_eq!(d.rgba(), rgba, "{name}: sin pérdida");
        }
        manifest.push_str(&format!("{name} {w} {h} {:016x}\n", fnv64(&d.rgba())));
    }
    std::fs::write(dir().join("manifest.txt"), manifest).unwrap();
}
