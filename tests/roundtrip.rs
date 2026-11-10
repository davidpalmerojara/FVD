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
        let p = Params { quality: 60.0, tile, ..Params::default() };
        let e = encode(&img, w, h, &p);
        let d = decode(&e.bytes).unwrap();
        assert_eq!(d.rgb, e.recon_rgb, "tile={tile}");
        assert!(metrics::psnr_rgb(&img, &d.rgb) > 25.0);
    }
}
