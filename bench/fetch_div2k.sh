#!/usr/bin/env bash
# Descarga 12 fotos de validación de DIV2K (2040 px de ancho) para el banco de
# fotos grandes. Uso: bench/fetch_div2k.sh [directorio]  (por defecto bench/data/div2k)
set -euo pipefail
DIR="${1:-$(dirname "$0")/data/div2k}"
mkdir -p "$DIR"
BASE="https://media.githubusercontent.com/media/sanathprabhu2-arch/DIV2kDataSet/HEAD/DIV2K_valid_HR"
for i in 801 809 817 825 833 841 849 857 865 873 881 889; do
  f="$DIR/0$i.png"
  [ -s "$f" ] || curl -fsSL -o "$f" "$BASE/0$i.png"
done
echo "DIV2K listo en $DIR"
