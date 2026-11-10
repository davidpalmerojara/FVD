#!/usr/bin/env bash
# Descarga las 24 imágenes del Kodak Lossless True Color Image Suite.
# Uso: bench/fetch_kodak.sh [directorio]   (por defecto: bench/data/kodak)
set -euo pipefail
DIR="${1:-$(dirname "$0")/data/kodak}"
mkdir -p "$DIR"
MIRRORS=(
  "https://r0k.us/graphics/kodak/kodak"
  "https://media.githubusercontent.com/media/lamco-admin/lamcogaussianimage/HEAD/test-data/kodak-dataset"
)
for i in $(seq -w 1 24); do
  f="$DIR/kodim$i.png"
  [ -s "$f" ] && continue
  for m in "${MIRRORS[@]}"; do
    if curl -fsSL -o "$f" "$m/kodim$i.png" && file "$f" | grep -q PNG; then break; fi
    rm -f "$f"
  done
  [ -s "$f" ] || { echo "no se pudo descargar kodim$i.png" >&2; exit 1; }
done
echo "Kodak listo en $DIR"
