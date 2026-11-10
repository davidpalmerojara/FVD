//! Tesela: códec de imagen experimental con transformadas de tamaño variable,
//! predicción intra direccional, CCLM y codificación aritmética adaptativa.

pub mod codec;
pub mod coeff;
pub mod color;
pub mod dct;
pub mod deblock;
pub mod init_probs;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod predict;
pub mod rc;
pub mod restore;
pub mod tools;

pub use codec::{Decoded, Encoded, Params, decode, encode, encode_rgba};

#[cfg(target_arch = "wasm32")]
pub mod wasm;
