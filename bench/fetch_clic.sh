#!/usr/bin/env bash
# Descarga el conjunto de validación «professional» de CLIC 2020 (41 fotos de
# alta resolución del Challenge on Learned Image Compression), uno de los
# conjuntos de referencia en investigación de compresión de imagen.
# Uso: bench/fetch_clic.sh [directorio]  (por defecto bench/data/clic)
set -euo pipefail
DIR="${1:-$(dirname "$0")/data/clic}"
mkdir -p "$DIR"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fsSL -o "$TMP/clic.zip" https://data.vision.ee.ethz.ch/cvl/clic/professional_valid_2020.zip
python3 - "$TMP/clic.zip" "$DIR" <<'PY'
import sys, zipfile, os
z = zipfile.ZipFile(sys.argv[1])
n = 0
for info in z.infolist():
    if info.filename.lower().endswith(".png") and not info.filename.startswith("__MACOSX"):
        with z.open(info) as src, open(os.path.join(sys.argv[2], os.path.basename(info.filename)), "wb") as dst:
            dst.write(src.read())
        n += 1
print(f"{n} imágenes en {sys.argv[2]}")
PY
