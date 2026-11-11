//! Fuzzer del decodificador (Rust estable, sin dependencias).
//!
//! Muta archivos válidos (vectores de conformidad y demos) y comprueba que el
//! decodificador nunca entra en pánico ni tarda demasiado: con datos rotos debe
//! devolver un error o una imagen cualquiera, nunca caerse.
//!
//!     cargo run --profile fuzz --example fuzz_decode -- [iteraciones] [semilla]
//!
//! Los archivos que fallan se guardan en `fuzz-fallos/`.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Píxeles que declara la cabecera (para no gastar el tiempo en imágenes enormes).
fn declared_pixels(d: &[u8]) -> u64 {
    if d.len() < 12 {
        return 0;
    }
    let w = u32::from_le_bytes(d[4..8].try_into().unwrap()) as u64;
    let h = u32::from_le_bytes(d[8..12].try_into().unwrap()) as u64;
    w * h
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut d = seed.to_vec();
    let n = 1 + rng.below(6);
    for _ in 0..n {
        if d.is_empty() {
            d.push(0);
        }
        let len = d.len();
        match rng.below(9) {
            0 => {
                let i = rng.below(len);
                d[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.below(len);
                d[i] = rng.next() as u8;
            }
            2 => {
                // Campo de la cabecera (los 23 primeros bytes).
                let i = rng.below(len.min(23));
                d[i] = [0, 1, 2, 0x7f, 0x80, 0xff][rng.below(6)];
            }
            3 => d.truncate(rng.below(len)),
            4 => {
                let i = rng.below(len);
                let k = rng.below(len - i).min(64);
                d.drain(i..i + k);
            }
            5 => {
                let i = rng.below(len);
                let k = rng.below(len - i).min(64);
                let chunk: Vec<u8> = d[i..i + k].to_vec();
                let j = rng.below(len);
                d.splice(j..j, chunk);
            }
            6 => {
                // Longitud de tesela o de vista previa.
                if len > 27 {
                    let i = 23 + rng.below((len - 23).min(16));
                    d[i] = rng.next() as u8;
                }
            }
            7 => {
                let i = rng.below(len);
                for b in d.iter_mut().skip(i).take(1 + rng.below(32)) {
                    *b = rng.next() as u8;
                }
            }
            _ => {
                let i = rng.below(len);
                d[i] = d[i].wrapping_add(1);
            }
        }
    }
    d
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let iters: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(20_000);
    let seed: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0x5eed);
    let mut seeds = Vec::new();
    for dir in ["tests/vectors", "web/demo"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().extension().is_some_and(|x| x == "tsl") {
                    seeds.push((e.path().display().to_string(), std::fs::read(e.path()).unwrap()));
                }
            }
        }
    }
    seeds.sort();
    assert!(!seeds.is_empty(), "no hay archivos semilla");
    std::panic::set_hook(Box::new(|_| {}));
    std::fs::create_dir_all("fuzz-fallos").ok();
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let (mut panics, mut slow, mut ok, mut err, mut skipped) = (0, 0, 0, 0, 0);
    let mut msgs = std::collections::BTreeMap::new();
    let t0 = Instant::now();
    for it in 0..iters {
        let (name, s) = &seeds[rng.below(seeds.len())];
        let data = mutate(&mut rng, s);
        if declared_pixels(&data) > 4 << 20 {
            skipped += 1;
            continue;
        }
        let t = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| {
            let a = tesela::decode(&data).is_ok();
            let _ = tesela::codec::decode_preview(&data);
            a
        }));
        let dt = t.elapsed();
        match r {
            Ok(true) => ok += 1,
            Ok(false) => err += 1,
            Err(e) => {
                panics += 1;
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                let key: String = msg.chars().take(90).collect();
                let entry = msgs.entry(key).or_insert(0);
                if *entry == 0 {
                    let p = format!("fuzz-fallos/{it}.tsl");
                    std::fs::write(&p, &data).ok();
                    println!("PÁNICO ({name} mutado) -> {p}: {msg}");
                }
                *entry += 1;
            }
        }
        if dt > Duration::from_secs(2) {
            slow += 1;
            let p = format!("fuzz-fallos/lento_{it}.tsl");
            std::fs::write(&p, &data).ok();
            println!("LENTO {:.1} s -> {p}", dt.as_secs_f64());
        }
    }
    println!(
        "{iters} pruebas en {:.0} s: {ok} decodificadas, {err} rechazadas, {panics} pánicos ({} distintos), {slow} lentas, {skipped} omitidas (demasiado grandes)",
        t0.elapsed().as_secs_f64(),
        msgs.len()
    );
    if panics > 0 || slow > 0 {
        std::process::exit(1);
    }
}
