//! Tesela: códec de imagen experimental con transformadas de tamaño variable,
//! predicción intra direccional, CCLM y codificación aritmética adaptativa.

pub mod codec;
pub mod coeff;
pub mod color;
pub mod dct;
pub mod init_probs;
pub mod deblock;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod predict;
pub mod rc;
pub mod restore;

pub use codec::{Decoded, Encoded, Params, decode, encode};

#[cfg(target_arch = "wasm32")]
pub mod wasm;
