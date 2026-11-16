//! Carga de imágenes de entrada para el conversor.
//!
//! Copia de `load_rgba`, `load_animation` y `reset_exif_orientation` de
//! `src/main.rs` (la línea de órdenes), para que la aplicación codifique
//! exactamente lo mismo que `tesela encode`: misma orientación EXIF, mismos
//! metadatos y mismos fotogramas en las animaciones. Si cambian allí, hay que
//! cambiarlas aquí.

/// Carga RGBA (alfa 255 si la imagen no tiene transparencia) con sus
/// metadatos. La orientación EXIF se aplica a los píxeles (la foto nunca sale
/// girada) y en el EXIF guardado queda como «normal».
pub fn load_rgba(path: &str) -> Result<(Vec<u8>, usize, usize, tesela::codec::Metadata), String> {
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
pub fn load_animation(
    path: &str,
) -> Result<Option<(Vec<Vec<u8>>, Vec<u32>, usize, usize)>, String> {
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
