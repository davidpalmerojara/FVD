#!/usr/bin/env bash
# Compila el módulo WebAssembly de Tesela y lo copia a web/tesela.wasm.
# Requiere: rustup target add wasm32-unknown-unknown
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --target wasm32-unknown-unknown --lib --no-default-features --features metrics
cp target/wasm32-unknown-unknown/release/tesela.wasm web/tesela.wasm
ls -la web/tesela.wasm
