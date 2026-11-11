//! SSIMULACRA2 calculado por filas, con poca memoria.
//!
//! Es una adaptación del crate `ssimulacra2` 0.5.1 (BSD-2-Clause, © los
//! contribuidores de rav1e): mismas operaciones y en el mismo orden, así que
//! da exactamente el mismo resultado (lo comprueban los tests). La diferencia
//! es la memoria: el original guarda unas 13 imágenes completas en coma
//! flotante a la vez (~165 bytes por píxel, casi 2 GB con una foto de 12 MP);
//! aquí la escala 1:1 se procesa fila a fila con anillos de 12 filas y solo se
//! guardan las versiones reducidas para las escalas siguientes (~8 B/px).
//!
//! Por escala: RGB lineal → XYB (desplazado a positivo) → medias y varianzas
//! con un gaussiano recursivo (horizontal por fila; vertical como un filtro
//! IIR por columna que avanza fila a fila) → mapas de SSIM y de bordes.

use ssimulacra2::{ColorPrimaries, LinearRgb, Rgb, TransferCharacteristic, Xyb};

const NUM_SCALES: usize = 6;

// Constantes del gaussiano recursivo (σ = 1,5), tal como las genera el crate.
const RADIUS: usize = 5;
const VERT_MUL_IN_1: f32 = 0.055295236_f32;
const VERT_MUL_IN_3: f32 = -0.058836687_f32;
const VERT_MUL_IN_5: f32 = 0.012955819_f32;
const VERT_MUL_PREV_1: f32 = -1.9021131_f32;
const VERT_MUL_PREV_3: f32 = -1.1755705_f32;
const VERT_MUL_PREV_5: f32 = -0.00000000000000012246469_f32;
const MUL_IN_1: f32 = 0.055295236_f32;
const MUL_IN_3: f32 = -0.058836687_f32;
const MUL_IN_5: f32 = 0.012955819_f32;
const MUL_PREV_1: f32 = 1.9021131_f32;
const MUL_PREV_3: f32 = 1.1755705_f32;
const MUL_PREV_5: f32 = 0.00000000000000012246469_f32;
const MUL_PREV2_1: f32 = -1_f32;
const MUL_PREV2_3: f32 = -1_f32;
const MUL_PREV2_5: f32 = -1_f32;

/// Filas guardadas en los anillos: la vertical del gaussiano lee de n−R−1 a
/// n+R−1 para producir la fila n.
const RING: usize = 2 * RADIUS + 2;

type Px = [f32; 3];

/// Planos que se desenfocan: σ₁², σ₂², σ₁₂, μ₁ y μ₂, cada uno con 3 canales.
const Q: usize = 15;

/// Desenfoque horizontal de una fila (idéntico a `RecursiveGaussian::horizontal_row`).
fn horizontal_row(input: &[f32], output: &mut [f32]) {
    let width = input.len();
    let big_n = RADIUS as isize;
    let (mut prev_1, mut prev_3, mut prev_5) = (0f32, 0f32, 0f32);
    let (mut prev2_1, mut prev2_3, mut prev2_5) = (0f32, 0f32, 0f32);
    let mut n = (-big_n) + 1;
    while n < width as isize {
        let left = n - big_n - 1;
        let right = n + big_n - 1;
        let left_val = if left >= 0 {
            input[left as usize]
        } else {
            0f32
        };
        let right_val = if right < width as isize {
            input[right as usize]
        } else {
            0f32
        };
        let sum = left_val + right_val;

        let mut out_1 = sum * MUL_IN_1;
        let mut out_3 = sum * MUL_IN_3;
        let mut out_5 = sum * MUL_IN_5;

        out_1 = MUL_PREV2_1.mul_add(prev2_1, out_1);
        out_3 = MUL_PREV2_3.mul_add(prev2_3, out_3);
        out_5 = MUL_PREV2_5.mul_add(prev2_5, out_5);
        prev2_1 = prev_1;
        prev2_3 = prev_3;
        prev2_5 = prev_5;

        out_1 = MUL_PREV_1.mul_add(prev_1, out_1);
        out_3 = MUL_PREV_3.mul_add(prev_3, out_3);
        out_5 = MUL_PREV_5.mul_add(prev_5, out_5);
        prev_1 = out_1;
        prev_3 = out_3;
        prev_5 = out_5;

        if n >= 0 {
            output[n as usize] = out_1 + out_3 + out_5;
        }
        n += 1;
    }
}

/// Fila de RGB lineal → planos XYB positivos (como `make_positive_xyb` + `xyb_to_planar`).
fn xyb_planar(lin: &[Px], out: &mut [Vec<f32>; 3]) {
    let w = lin.len();
    let xyb = Xyb::from(LinearRgb::new(lin.to_vec(), w, 1).expect("fila"));
    for (x, pix) in xyb.data().iter().enumerate() {
        let mut p = *pix;
        p[2] = (p[2] - p[1]) + 0.55;
        p[0] = (p[0]).mul_add(14.0, 0.42);
        p[1] += 0.01;
        out[0][x] = p[0];
        out[1][x] = p[1];
        out[2][x] = p[2];
    }
}

/// Resultados de una escala (como `MsssimScale`).
struct Scale {
    ssim: [f64; 6],
    edge: [f64; 12],
}

/// Procesa una escala de `w × h` recibiendo sus filas de RGB lineal en orden
/// (`row(y)` da las dos imágenes). Si `next` es `Some`, construye a la vez la
/// escala siguiente reducida a la mitad (como `downscale_by_2`).
fn process_scale(
    w: usize,
    h: usize,
    row: &mut dyn FnMut(usize) -> (Vec<Px>, Vec<Px>),
    mut next: Option<&mut (Vec<Px>, Vec<Px>)>,
) -> Scale {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    if let Some(n) = next.as_deref_mut() {
        n.0 = vec![[0.0; 3]; nw * nh];
        n.1 = vec![[0.0; 3]; nw * nh];
    }
    let mut pending: Option<(Vec<Px>, Vec<Px>)> = None;

    // Anillos: planos XYB de las dos imágenes y los 15 planos desenfocados en horizontal.
    let mut img_ring = vec![vec![0f32; w]; RING * 6];
    let mut hb_ring = vec![vec![0f32; w]; RING * Q];
    let mut src = vec![vec![0f32; w]; Q];
    let mut planes1 = [vec![0f32; w], vec![0f32; w], vec![0f32; w]];
    let mut planes2 = [vec![0f32; w], vec![0f32; w], vec![0f32; w]];
    // Estado del IIR vertical por plano y columna: términos 1, 3 y 5.
    let mut prev = vec![[0f32; 3]; Q * w];
    let mut prev2 = vec![[0f32; 3]; Q * w];
    let mut res = vec![vec![0f32; w]; Q];
    let mut ssim_sum = [[0f64; 2]; 3];
    let mut edge_sum = [[0f64; 4]; 3];
    let zero = vec![0f32; w];
    let big_n = RADIUS as isize;

    let mut step = |n: isize, hb_ring: &Vec<Vec<f32>>, img_ring: &Vec<Vec<f32>>| {
        let top = n - big_n - 1;
        let bottom = n + big_n - 1;
        for q in 0..Q {
            let top_row = if top >= 0 {
                &hb_ring[(top as usize % RING) * Q + q]
            } else {
                &zero
            };
            let bottom_row = if bottom < h as isize {
                &hb_ring[(bottom as usize % RING) * Q + q]
            } else {
                &zero
            };
            for x in 0..w {
                let sum = top_row[x] + bottom_row[x];
                let (p, p2) = (prev[q * w + x], prev2[q * w + x]);
                let out1 = p[0].mul_add(VERT_MUL_PREV_1, p2[0]);
                let out3 = p[1].mul_add(VERT_MUL_PREV_3, p2[1]);
                let out5 = p[2].mul_add(VERT_MUL_PREV_5, p2[2]);
                let out1 = sum.mul_add(VERT_MUL_IN_1, -out1);
                let out3 = sum.mul_add(VERT_MUL_IN_3, -out3);
                let out5 = sum.mul_add(VERT_MUL_IN_5, -out5);
                prev2[q * w + x] = p;
                prev[q * w + x] = [out1, out3, out5];
                if n >= 0 {
                    res[q][x] = out1 + out3 + out5;
                }
            }
        }
        if n < 0 {
            return;
        }
        // Fila n de los mapas de SSIM (ssim_map) y de bordes (edge_diff_map).
        const C2: f32 = 0.0009f32;
        let slot = n as usize % RING;
        for c in 0..3 {
            let (s11, s22, s12) = (&res[c], &res[3 + c], &res[6 + c]);
            let (m1, m2) = (&res[9 + c], &res[12 + c]);
            for x in 0..w {
                let mu1 = m1[x];
                let mu2 = m2[x];
                let mu11 = mu1 * mu1;
                let mu22 = mu2 * mu2;
                let mu12 = mu1 * mu2;
                let mu_diff = mu1 - mu2;
                let num_m = mu_diff.mul_add(-mu_diff, 1.0f32);
                let num_s = 2f32.mul_add(s12[x] - mu12, C2);
                let denom_s = (s11[x] - mu11) + (s22[x] - mu22) + C2;
                let mut d = 1.0f64 - f64::from((num_m * num_s) / denom_s);
                d = d.max(0.0);
                ssim_sum[c][0] += d;
                ssim_sum[c][1] += d.powi(4);
            }
            let (row1, row2) = (&img_ring[slot * 6 + c], &img_ring[slot * 6 + 3 + c]);
            for x in 0..w {
                let d1: f64 = (1.0 + f64::from((row2[x] - m2[x]).abs()))
                    / (1.0 + f64::from((row1[x] - m1[x]).abs()))
                    - 1.0;
                let artifact = d1.max(0.0);
                edge_sum[c][0] += artifact;
                edge_sum[c][1] += artifact.powi(4);
                let detail_lost = (-d1).max(0.0);
                edge_sum[c][2] += detail_lost;
                edge_sum[c][3] += detail_lost.powi(4);
            }
        }
    };

    for y in 0..h {
        let (l1, l2) = row(y);
        // Escala siguiente: media de 2×2 con las coordenadas recortadas al borde.
        if let Some(nx) = next.as_deref_mut() {
            let last = y == h - 1;
            if y % 2 == 0 && !last {
                pending = Some((l1.clone(), l2.clone()));
            } else {
                let (t1, t2) = pending.take().unwrap_or_else(|| (l1.clone(), l2.clone()));
                let oy = y / 2;
                for (dst, top, bot) in [(&mut nx.0, &t1, &l1), (&mut nx.1, &t2, &l2)] {
                    for ox in 0..nw {
                        let x0 = (ox * 2).min(w - 1);
                        let x1 = (ox * 2 + 1).min(w - 1);
                        for c in 0..3 {
                            let mut sum = 0f32;
                            sum += top[x0][c];
                            sum += top[x1][c];
                            sum += bot[x0][c];
                            sum += bot[x1][c];
                            dst[oy * nw + ox][c] = sum * (1f32 / 4f32);
                        }
                    }
                }
            }
        }
        xyb_planar(&l1, &mut planes1);
        xyb_planar(&l2, &mut planes2);
        let slot = y % RING;
        for c in 0..3 {
            for x in 0..w {
                src[c][x] = planes1[c][x] * planes1[c][x];
                src[3 + c][x] = planes2[c][x] * planes2[c][x];
                src[6 + c][x] = planes1[c][x] * planes2[c][x];
            }
            src[9 + c].copy_from_slice(&planes1[c]);
            src[12 + c].copy_from_slice(&planes2[c]);
            img_ring[slot * 6 + c].copy_from_slice(&planes1[c]);
            img_ring[slot * 6 + 3 + c].copy_from_slice(&planes2[c]);
        }
        for q in 0..Q {
            horizontal_row(&src[q], &mut hb_ring[slot * Q + q]);
        }
        step(y as isize - big_n + 1, &hb_ring, &img_ring);
    }
    for n in (h as isize - big_n + 1).max(-big_n + 1)..h as isize {
        step(n, &hb_ring, &img_ring);
    }

    let one_per_pixels = 1.0f64 / (w * h) as f64;
    let mut s = Scale {
        ssim: [0.0; 6],
        edge: [0.0; 12],
    };
    for c in 0..3 {
        s.ssim[c * 2] = one_per_pixels * ssim_sum[c][0];
        s.ssim[c * 2 + 1] = (one_per_pixels * ssim_sum[c][1]).sqrt().sqrt();
        s.edge[c * 4] = one_per_pixels * edge_sum[c][0];
        s.edge[c * 4 + 1] = (one_per_pixels * edge_sum[c][1]).sqrt().sqrt();
        s.edge[c * 4 + 2] = one_per_pixels * edge_sum[c][2];
        s.edge[c * 4 + 3] = (one_per_pixels * edge_sum[c][3]).sqrt().sqrt();
    }
    s
}

/// SSIMULACRA2 de dos imágenes sRGB de 8 bits (RGB entrelazado, `w × h`).
pub fn ssimulacra2(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    if w < 8 || h < 8 {
        return f64::NAN;
    }
    let lin = |d: &[u8], y: usize| -> Vec<Px> {
        let px: Vec<Px> = d[y * w * 3..(y + 1) * w * 3]
            .chunks_exact(3)
            .map(|p| {
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]
            })
            .collect();
        let rgb = Rgb::new(
            px,
            w,
            1,
            TransferCharacteristic::SRGB,
            ColorPrimaries::BT709,
        )
        .expect("fila");
        LinearRgb::try_from(rgb).expect("rgb lineal").into_data()
    };
    let mut scales = Vec::new();
    let (mut width, mut height) = (w, h);
    let mut cur: (Vec<Px>, Vec<Px>) = (Vec::new(), Vec::new());
    for scale in 0..NUM_SCALES {
        // Como en el original: se mira el tamaño de la escala anterior.
        if width < 8 || height < 8 {
            break;
        }
        let (sw, sh) = if scale == 0 {
            (w, h)
        } else {
            (width.div_ceil(2), height.div_ceil(2))
        };
        let mut nxt = (Vec::new(), Vec::new());
        let want_next = scale + 1 < NUM_SCALES && sw >= 8 && sh >= 8;
        let s = if scale == 0 {
            process_scale(
                sw,
                sh,
                &mut |y| (lin(a, y), lin(b, y)),
                want_next.then_some(&mut nxt),
            )
        } else {
            let c = &cur;
            process_scale(
                sw,
                sh,
                &mut |y| {
                    (
                        c.0[y * sw..(y + 1) * sw].to_vec(),
                        c.1[y * sw..(y + 1) * sw].to_vec(),
                    )
                },
                want_next.then_some(&mut nxt),
            )
        };
        scales.push(s);
        cur = nxt;
        width = sw;
        height = sh;
    }
    score(&scales)
}

/// Puntuación final (idéntica a `Msssim::score`).
fn score(scales: &[Scale]) -> f64 {
    const WEIGHT: [f64; 108] = [
        0.0,
        0.000_737_660_670_740_658_6,
        0.0,
        0.0,
        0.000_779_348_168_286_730_9,
        0.0,
        0.0,
        0.000_437_115_573_010_737_9,
        0.0,
        1.104_172_642_665_734_6,
        0.000_662_848_341_292_71,
        0.000_152_316_327_837_187_52,
        0.0,
        0.001_640_643_745_659_975_4,
        0.0,
        1.842_245_552_053_929_8,
        11.441_172_603_757_666,
        0.0,
        0.000_798_910_943_601_516_3,
        0.000_176_816_438_078_653,
        0.0,
        1.878_759_497_954_638_7,
        10.949_069_906_051_42,
        0.0,
        0.000_728_934_699_150_807_2,
        0.967_793_708_062_683_3,
        0.0,
        0.000_140_034_242_854_358_84,
        0.998_176_697_785_496_7,
        0.000_319_497_559_344_350_53,
        0.000_455_099_211_379_206_3,
        0.0,
        0.0,
        0.001_364_876_616_324_339_8,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        7.466_890_328_078_848,
        0.0,
        17.445_833_984_131_262,
        0.000_623_560_163_404_146_6,
        0.0,
        0.0,
        6.683_678_146_179_332,
        0.000_377_244_079_796_112_96,
        1.027_889_937_768_264,
        225.205_153_008_492_74,
        0.0,
        0.0,
        19.213_238_186_143_016,
        0.001_140_152_458_661_836_1,
        0.001_237_755_635_509_985,
        176.393_175_984_506_94,
        0.0,
        0.0,
        24.433_009_998_704_76,
        0.285_208_026_121_177_57,
        0.000_448_543_692_383_340_8,
        0.0,
        0.0,
        0.0,
        34.779_063_444_837_72,
        44.835_625_328_877_896,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.000_868_055_657_329_169_8,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.000_531_319_187_435_874_7,
        0.0,
        0.000_165_338_141_613_791_12,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.000_417_917_180_325_133_6,
        0.001_729_082_823_472_283_3,
        0.0,
        0.002_082_700_584_663_643_7,
        0.0,
        0.0,
        8.826_982_764_996_862,
        23.192_433_439_989_26,
        0.0,
        95.108_049_881_108_6,
        0.986_397_803_440_068_2,
        0.983_438_279_246_535_3,
        0.001_228_640_504_827_849_3,
        171.266_725_589_730_7,
        0.980_785_887_243_537_9,
        0.0,
        0.0,
        0.0,
        0.000_513_006_458_899_067_9,
        0.0,
        0.000_108_540_578_584_115_37,
    ];
    let mut ssim = 0.0f64;
    let mut i = 0usize;
    for c in 0..3 {
        for scale in scales {
            for n in 0..2 {
                ssim = WEIGHT[i].mul_add(scale.ssim[c * 2 + n].abs(), ssim);
                i += 1;
                ssim = WEIGHT[i].mul_add(scale.edge[c * 4 + n].abs(), ssim);
                i += 1;
                ssim = WEIGHT[i].mul_add(scale.edge[c * 4 + n + 2].abs(), ssim);
                i += 1;
            }
        }
    }
    ssim *= 0.956_238_261_683_484_4_f64;
    ssim = (6.248_496_625_763_138e-5 * ssim * ssim).mul_add(
        ssim,
        2.326_765_642_916_932f64.mul_add(ssim, -0.020_884_521_182_843_837 * ssim * ssim),
    );
    if ssim > 0.0f64 {
        ssim = ssim
            .powf(0.627_633_646_783_138_7)
            .mul_add(-10.0f64, 100.0f64);
    } else {
        ssim = 100.0f64;
    }
    ssim
}
