//! Fuzzing guiado por cobertura del decodificador (libFuzzer):
//!     cargo +nightly fuzz run decode fuzz/corpus/decode
//! Con datos arbitrarios el decodificador debe devolver un error o una imagen,
//! nunca entrar en pánico, salirse de la memoria ni colgarse.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Las dimensiones grandes son legítimas pero lentas: se limitan aquí para
    // que el fuzzer explore la sintaxis en lugar de reservar memoria.
    if data.len() >= 12 {
        let w = u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64;
        let h = u32::from_le_bytes(data[8..12].try_into().unwrap()) as u64;
        if w * h > 1 << 20 {
            return;
        }
    }
    let _ = tesela::decode(data);
    let _ = tesela::codec::decode_preview(data);
    let _ = tesela::codec::decode_animation(data, 1 << 22);
});
