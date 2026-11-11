#!/usr/bin/env bash
# Compila los módulos WebAssembly de Tesela:
#   web/tesela.wasm     un hilo (decodificar y respaldo)
#   web/tesela-mt.wasm  con hilos, para codificar en páginas aisladas
# Requiere: rustup target add wasm32-unknown-unknown wasm32-wasip1-threads
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --target wasm32-unknown-unknown --lib --no-default-features --features metrics
cp target/wasm32-unknown-unknown/release/tesela.wasm web/tesela.wasm
cargo build --release --target wasm32-wasip1-threads --lib --no-default-features --features metrics
cp target/wasm32-wasip1-threads/release/tesela.wasm web/tesela-mt.wasm
ls -la web/tesela.wasm web/tesela-mt.wasm
