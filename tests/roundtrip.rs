use tesela::{Params, decode, encode, metrics};

fn synthetic(w: usize, h: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2654435761).max(1);
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s % 21) as i32 - 10
    };
    let mut v = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let edge = if (x / 9 + y / 13) % 2 == 0 { 60 } else { 0 };
            v.push((x * 255 / w.max(1) + rnd() as usize % 3).min(255) as u8);
            v.push(((y * 200 / h.max(1)) as i32 + edge + rnd()).clamp(0, 255) as u8);
            v.push((((x + y) * 3) % 256) as u8);
        }
    }
    v
}

#[test]
fn decoder_matches_encoder_on_odd_sizes() {
    for &(w, h) in &[(1, 1), (7, 5), (65, 33), (130, 70), (64, 64)] {
        for sub in [false, true] {
            for q in [10.0, 60.0, 95.0] {
                let img = synthetic(w, h, (w * 31 + h) as u32);
                let p = Params {
                    quality: q,
                    subsample: Some(sub),
                    ..Params::default()
                };
                let e = encode(&img, w, h, &p);
                let d = decode(&e.bytes).expect("decode");
                assert_eq!((d.width, d.height), (w, h));
                assert_eq!(d.rgb, e.recon_rgb, "{w}x{h} sub={sub} q={q}");
            }
        }
    }
}

#[test]
fn high_quality_is_close_to_source() {
    let (w, h) = (96, 80);
    let img = synthetic(w, h, 7);
    let p = Params {
        quality: 98.0,
        subsample: Some(false),
        ..Params::default()
    }
    .tune_psnr();
    let e = encode(&img, w, h, &p);
    let d = decode(&e.bytes).unwrap();
    let psnr = metrics::psnr_rgb(&img, &d.rgb);
    assert!(psnr > 45.0, "PSNR demasiado bajo: {psnr}");
}

#[test]
fn quality_controls_size() {
    let (w, h) = (128, 96);
    let img = synthetic(w, h, 3);
    let sizes: Vec<usize> = [20.0, 50.0, 80.0]
        .iter()
        .map(|&q| {
            encode(
                &img,
                w,
                h,
                &Params {
                    quality: q,
                    ..Params::default()
                },
            )
            .bytes
            .len()
        })
        .collect();
    assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2], "{sizes:?}");
}

#[test]
fn rejects_garbage() {
    assert!(decode(b"hola").is_err());
    assert!(decode(b"TSL1\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").is_err());
    // Datos truncados/corruptos no deben provocar pánico.
    let img = synthetic(40, 40, 1);
    let mut e = encode(&img, 40, 40, &Params::default()).bytes;
    e.truncate(e.len() / 2);
    for b in e.iter_mut().skip(25) {
        *b ^= 0x5A;
    }
    let _ = decode(&e);
}

#[test]
fn tiles_roundtrip() {
    let (w, h) = (200, 150);
    let img = synthetic(w, h, 9);
    for tile in [64, 128, 1024] {
        let p = Params {
            quality: 60.0,
            tile,
            ..Params::default()
        };
        let e = encode(&img, w, h, &p);
        let d = decode(&e.bytes).unwrap();
        assert_eq!(d.rgb, e.recon_rgb, "tile={tile}");
        assert!(metrics::psnr_rgb(&img, &d.rgb) > 25.0);
    }
}

/// Imagen sintética tipo "captura": fondo plano, texto repetido y bloques de color.
fn screenlike(w: usize, h: usize) -> Vec<u8> {
    let glyph = |c: usize, x: usize, y: usize| ((c * 7 + x * 3 + y * 5) % 11) < 4;
    let mut v = vec![245u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let (line, col) = (y / 12, x / 7);
            if y % 12 < 9 && x % 7 < 5 && line % 3 != 2 && glyph(col % 5, x % 7, y % 12) {
                v[i..i + 3].copy_from_slice(&[30, 30, 40]);
            }
            if x > w * 2 / 3 && y < h / 3 {
                v[i..i + 3].copy_from_slice(&[37, 99, 235]);
            }
        }
    }
    v
}

#[test]
fn screen_tools_roundtrip() {
    let (w, h) = (150, 110);
    let img = screenlike(w, h);
    for (tile, q) in [(1024, 80.0), (64, 40.0), (128, 95.0)] {
        let p = Params {
            quality: q,
            tile,
            screen: Some(true),
            ..Params::default()
        };
        let e = encode(&img, w, h, &p);
        let d = decode(&e.bytes).unwrap();
        assert_eq!(d.rgb, e.recon_rgb, "tile={tile} q={q}");
        let off = encode(
            &img,
            w,
            h,
            &Params {
                screen: Some(false),
                ..p.clone()
            },
        );
        if q >= 80.0 {
            assert!(
                e.bytes.len() < off.bytes.len(),
                "las herramientas deberían ayudar: {} vs {}",
                e.bytes.len(),
                off.bytes.len()
            );
        }
    }
}

#[test]
fn corrupt_screen_stream_does_not_panic() {
    let img = screenlike(96, 64);
    let p = Params {
        quality: 70.0,
        screen: Some(true),
        ..Params::default()
    };
    let good = encode(&img, 96, 64, &p).bytes;
    for seed in 0..40u32 {
        let mut b = good.clone();
        let mut s = seed.wrapping_mul(2654435761) | 1;
        for _ in 0..8 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let i = 40 + (s as usize % (b.len() - 40));
            b[i] ^= (s >> 8) as u8 | 1;
        }
        let _ = decode(&b);
    }
}

#[test]
fn lossless_is_exact() {
    for (w, h, img) in [
        (97, 61, synthetic(97, 61, 5)),
        (150, 110, screenlike(150, 110)),
    ] {
        for tile in [64, 1024] {
            let p = Params {
                lossless: true,
                tile,
                ..Params::default()
            };
            let e = encode(&img, w, h, &p);
            let d = decode(&e.bytes).unwrap();
            assert_eq!(d.rgb, img, "{w}x{h} tile={tile}");
            assert!(d.alpha.is_none());
        }
    }
}

#[test]
fn alpha_roundtrip() {
    let (w, h) = (120, 90);
    let rgb = synthetic(w, h, 11);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        let (x, y) = (i % w, i / w);
        let a = if (x as i32 - 60).pow(2) + (y as i32 - 45).pow(2) < 900 {
            255
        } else if x < 20 {
            0
        } else {
            (x * 2) as u8
        };
        rgba.extend_from_slice(&[px[0], px[1], px[2], a]);
    }
    for (q, lossless, tile) in [(70.0, false, 1024), (40.0, false, 64), (0.0, true, 1024)] {
        let p = Params {
            quality: q,
            lossless,
            tile,
            ..Params::default()
        };
        let e = tesela::encode_rgba(&rgba, w, h, &p);
        let d = decode(&e.bytes).unwrap();
        let alpha: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(
            d.alpha.as_deref(),
            Some(&alpha[..]),
            "el alfa debe ser exacto"
        );
        assert_eq!(d.rgb, e.recon_rgb);
        if lossless {
            assert_eq!(d.rgba(), rgba);
        }
    }
    // Imagen opaca: no se guarda alfa.
    let opaque: Vec<u8> = rgb
        .chunks_exact(3)
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect();
    assert!(
        decode(&tesela::encode_rgba(&opaque, w, h, &Params::default()).bytes)
            .unwrap()
            .alpha
            .is_none()
    );
}
