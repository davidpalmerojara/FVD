#!/usr/bin/env bash
# Descarga fotos de validación de DIV2K (2040 px de ancho) para el banco de
# fotos grandes: 12 por defecto, o las 100 con --todas.
# Uso: bench/fetch_div2k.sh [--todas] [directorio]  (por defecto bench/data/div2k)
set -euo pipefail
ALL=0
if [ "${1:-}" = "--todas" ]; then ALL=1; shift; fi
DIR="${1:-$(dirname "$0")/data/div2k}"
mkdir -p "$DIR"
BASE="https://media.githubusercontent.com/media/sanathprabhu2-arch/DIV2kDataSet/HEAD/DIV2K_valid_HR"
IDS="801 809 817 825 833 841 849 857 865 873 881 889"
[ "$ALL" = 1 ] && IDS="$(seq 801 900)"
for i in $IDS; do
  f="$DIR/0$i.png"
  [ -s "$f" ] || curl -fsSL -o "$f" "$BASE/0$i.png"
done
echo "DIV2K listo en $DIR"
