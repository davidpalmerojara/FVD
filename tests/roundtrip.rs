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
    assert!(decode(b"TSL\x02\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").is_err());
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

#[test]
fn metadata_roundtrip() {
    use tesela::codec::{Metadata, decode_preview};
    let (w, h) = (96, 64);
    let img = synthetic(w, h, 7);
    let meta = Metadata {
        icc: Some((0..=255u8).cycle().take(600).collect()),
        exif: Some(b"MM\0*\0\0\0\x08\0\0".to_vec()),
    };
    for preview in [false, true] {
        let p = Params {
            quality: 70.0,
            preview,
            meta: meta.clone(),
            ..Default::default()
        };
        let e = encode(&img, w, h, &p);
        let d = decode(&e.bytes).expect("decodifica");
        assert_eq!(d.meta, meta, "metadatos (vista previa: {preview})");
        assert_eq!(d.rgb, e.recon_rgb, "píxeles (vista previa: {preview})");
        // La vista previa se encuentra detrás de los metadatos.
        assert_eq!(decode_preview(&e.bytes).unwrap().is_some(), preview);
        // Los mismos píxeles que sin metadatos: solo cambia el contenedor.
        let bare = encode(
            &img,
            w,
            h,
            &Params {
                quality: 70.0,
                preview,
                ..Default::default()
            },
        );
        assert_eq!(decode(&bare.bytes).unwrap().rgb, d.rgb);
        assert_eq!(e.bytes.len(), bare.bytes.len() + 4 + 8 + 600 + 8 + 10);
    }
    // La v1 no tiene metadatos: se ignoran al codificar.
    let p = Params {
        quality: 70.0,
        version: 1,
        meta,
        ..Default::default()
    };
    let d = decode(&encode(&img, w, h, &p).bytes).unwrap();
    assert!(d.meta.is_empty());
}

#[test]
fn animation_roundtrip() {
    use tesela::codec::{anim_info, decode_animation, encode_animation};
    let (w, h) = (80, 48);
    // Fondo fijo y un cuadrado que se mueve; el fotograma 3 repite el 2.
    let frame = |t: usize| -> Vec<u8> {
        let mut f = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let sq = x >= 10 + 8 * t.min(2) && x < 22 + 8 * t.min(2) && (16..28).contains(&y);
                if sq {
                    f.extend_from_slice(&[230, 40, 30, 255]);
                } else {
                    f.extend_from_slice(&[
                        (x * 3) as u8,
                        (y * 5) as u8,
                        120,
                        if x < 4 { 0 } else { 255 },
                    ]);
                }
            }
        }
        f
    };
    let frames: Vec<Vec<u8>> = (0..4).map(frame).collect();
    let durations = [100, 100, 100, 250];
    for lossless in [true, false] {
        let p = Params {
            lossless,
            quality: 80.0,
            ..Default::default()
        };
        let bytes = encode_animation(&frames, &durations, w, h, 0, &p).unwrap();
        let a = anim_info(&bytes).unwrap().expect("animada");
        assert_eq!((a.width, a.height, a.loops), (w, h, 0));
        // El último fotograma no cambia nada: alarga el anterior.
        assert_eq!(
            a.frames.iter().map(|f| f.duration_ms).collect::<Vec<_>>(),
            [100, 100, 350]
        );
        // Solo se guarda el rectángulo que cambia.
        assert!(a.frames[1].w < w && a.frames[1].h < h);
        let (_, dec) = decode_animation(&bytes, 1 << 26).unwrap();
        assert_eq!(dec.len(), 3);
        if lossless {
            for (k, (canvas, _)) in dec.iter().enumerate() {
                assert_eq!(canvas, &frames[k], "fotograma {k} exacto");
            }
        }
        // decode() da el primer fotograma.
        let first = decode(&bytes).unwrap();
        assert_eq!((first.width, first.height), (w, h));
    }
}

#[test]
fn animation_inter_no_drift() {
    use tesela::codec::{anim_info, decode_animation, encode_animation};
    // Una franja de texto sintético que se desplaza (como al hacer scroll):
    // la predicción entre fotogramas debe aprovecharla sin acumular error.
    let (w, h) = (128, 96);
    let base = synthetic(w, 3 * h, 5);
    let frames: Vec<Vec<u8>> = (0..8)
        .map(|t| {
            let mut f = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    let i = ((y + 6 * t) * w + x) * 3;
                    f.extend_from_slice(&[base[i], base[i + 1], base[i + 2], 255]);
                }
            }
            f
        })
        .collect();
    let durations = vec![50; frames.len()];
    for lossless in [true, false] {
        let p = Params {
            lossless,
            quality: 85.0,
            ..Default::default()
        };
        let bytes = encode_animation(&frames, &durations, w, h, 0, &p).unwrap();
        let (_, dec) = decode_animation(&bytes, 1 << 26).unwrap();
        assert_eq!(dec.len(), frames.len());
        let rgb =
            |f: &[u8]| -> Vec<u8> { f.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect() };
        let first = metrics::psnr_rgb(&rgb(&frames[0]), &rgb(&dec[0].0));
        for (k, (canvas, _)) in dec.iter().enumerate() {
            if lossless {
                assert_eq!(canvas, &frames[k], "sin pérdida, fotograma {k}");
            } else {
                // Sin deriva: ningún fotograma mucho peor que el primero (que no predice).
                let ps = metrics::psnr_rgb(&rgb(&frames[k]), &rgb(canvas));
                assert!(
                    ps > first - 3.0,
                    "fotograma {k}: PSNR {ps:.1} frente a {first:.1} del primero"
                );
            }
        }
        // Un fotograma que depende del anterior no se decodifica solo.
        let a = anim_info(&bytes).unwrap().unwrap();
        assert!(tesela::codec::decode_frame(&bytes, &a.frames[1], None).is_err());
    }
}

#[test]
fn animation_encoder_matches_decoder() {
    use tesela::codec::{decode_animation, encode_animation_recon};
    // Movimiento de 1 píxel por fotograma sobre una imagen de 3×2
    // superbloques: predicción entre fotogramas, predictor de vectores y
    // varias teselas de trabajo. El decodificador debe reproducir byte a
    // byte el lienzo que el codificador cree haber producido.
    let (w, h) = (192, 128);
    let base = synthetic(w + 16, h + 16, 9);
    let frames: Vec<Vec<u8>> = (0..6)
        .map(|t| {
            let mut f = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    let i = ((y + t) * (w + 16) + x + 2 * t) * 3;
                    f.extend_from_slice(&[base[i], base[i + 1], base[i + 2], 255]);
                }
            }
            f
        })
        .collect();
    for q in [40.0, 80.0] {
        for threads in [1, 4] {
            let p = Params {
                quality: q,
                threads,
                ..Default::default()
            };
            let (bytes, recons) =
                encode_animation_recon(&frames, &vec![40; frames.len()], w, h, 0, &p).unwrap();
            let (_, dec) = decode_animation(&bytes, 1 << 26).unwrap();
            assert_eq!(dec.len(), recons.len());
            for (k, ((d, _), r)) in dec.iter().zip(&recons).enumerate() {
                assert!(
                    d == r,
                    "q{q}, {threads} hilos: el fotograma {k} decodificado no coincide con el del codificador"
                );
            }
        }
    }
}

#[test]
fn animation_zoom_matches_decoder() {
    use tesela::codec::{decode_animation, encode_animation_recon};
    // Zoom lento (movimiento global afín): el decodificador debe reproducir
    // exactamente lo que el codificador cree haber producido.
    let (w, h) = (200, 136);
    let (bw, bh) = (w + 40, h + 40);
    let base = synthetic(bw, bh, 5);
    let frames: Vec<Vec<u8>> = (0..5)
        .map(|t| {
            let s = 1.0 + 0.01 * t as f64;
            let mut f = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    let sx = (x as f64 - w as f64 / 2.0) / s + bw as f64 / 2.0;
                    let sy = (y as f64 - h as f64 / 2.0) / s + bh as f64 / 2.0;
                    let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
                    let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
                    for c in 0..3 {
                        let p = |xx: usize, yy: usize| base[(yy * bw + xx) * 3 + c] as f64;
                        let v = p(x0, y0) * (1.0 - fx) * (1.0 - fy)
                            + p(x0 + 1, y0) * fx * (1.0 - fy)
                            + p(x0, y0 + 1) * (1.0 - fx) * fy
                            + p(x0 + 1, y0 + 1) * fx * fy;
                        f.push(v.round() as u8);
                    }
                    f.push(255);
                }
            }
            f
        })
        .collect();
    for threads in [1, 3] {
        let p = Params {
            quality: 70.0,
            threads,
            ..Default::default()
        };
        let (bytes, recons) =
            encode_animation_recon(&frames, &vec![40; frames.len()], w, h, 0, &p).unwrap();
        let (_, dec) = decode_animation(&bytes, 1 << 26).unwrap();
        for (k, ((d, _), r)) in dec.iter().zip(&recons).enumerate() {
            assert!(d == r, "{threads} hilos: el fotograma {k} no coincide");
        }
    }
}
