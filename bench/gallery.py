#!/usr/bin/env python3
"""Galería de la web: cada formato con el MISMO tamaño de archivo.

Para cada imagen se fija un presupuesto de bytes y se busca (bisección sobre
el parámetro de calidad) el archivo más grande de cada formato que no lo
supera. Luego se decodifica, se recorta la misma zona y se guardan los
recortes en PNG (sin pérdida) junto con los bytes reales y el SSIMULACRA2 de
la imagen completa.

    python3 bench/gallery.py imagen.png:presupuesto_kb:x,y,lado [...] --out web/gallery

Usa los mismos códecs y ajustes que bench.py (WebP método 6, AVIF aom speed 4,
JPEG XL effort 7, JPEG de Pillow con optimize).
"""

import argparse
import io
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench import TESELA, compare  # noqa: E402

# Rango del parámetro de cada códec (de peor a mejor calidad) y si es entero.
RANGES = {
    "jpeg": (1, 100, True),
    "webp": (0, 100, True),
    "avif": (0, 100, True),
    "jxl": (25.0, 0.05, False),  # distancia: menor = mejor
    "tesela": (1, 100, False),
}
LABELS = {"jpeg": "JPEG", "webp": "WebP", "avif": "AVIF", "jxl": "JPEG XL", "tesela": "Tesela"}


def encode(codec, img_path, param, out):
    if codec == "tesela":
        subprocess.run([TESELA, "encode", img_path, out, "-q", f"{param:.2f}"], check=True, capture_output=True)
        return
    im = Image.open(img_path).convert("RGB")
    if codec == "jpeg":
        im.save(out, "JPEG", quality=int(param), optimize=True)
    elif codec == "webp":
        im.save(out, "WEBP", quality=int(param), method=6)
    elif codec == "avif":
        im.save(out, "AVIF", quality=int(param), speed=4, subsampling="4:2:0")
    elif codec == "jxl":
        import imagecodecs

        Path(out).write_bytes(imagecodecs.jpegxl_encode(np.asarray(im), distance=param, effort=7, numthreads=1))


def decode(codec, enc, dec):
    if codec == "tesela":
        subprocess.run([TESELA, "decode", enc, dec], check=True, capture_output=True)
    elif codec == "jxl":
        import imagecodecs

        Image.fromarray(imagecodecs.jpegxl_decode(Path(enc).read_bytes())).save(dec)
    else:
        Image.open(enc).convert("RGB").save(dec)


def fit(codec, img_path, budget, td):
    """Mejor parámetro cuyo archivo no pasa de `budget` bytes."""
    lo, hi, integer = RANGES[codec]
    out = os.path.join(td, f"{codec}.bin")
    best = None
    a, b = lo, hi
    for _ in range(14):
        mid = round((a + b) / 2) if integer else (a + b) / 2
        encode(codec, img_path, mid, out)
        size = os.path.getsize(out)
        if size <= budget:
            best = (mid, size)
            a = mid
        else:
            b = mid
        if integer and abs(b - a) <= 1:
            break
    if best is None:
        best = (lo, None)
    encode(codec, img_path, best[0], out)
    return best[0], os.path.getsize(out), out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("items", nargs="+", help="imagen.png:presupuesto_kb:x,y,lado[:nombre]")
    ap.add_argument("--out", default="web/gallery")
    ap.add_argument("--codecs", default="jpeg,webp,avif,jxl,tesela")
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    meta = []
    for item in args.items:
        parts = item.split(":")
        path, kb, crop = parts[0], float(parts[1]), [int(v) for v in parts[2].split(",")]
        name = parts[3] if len(parts) > 3 else Path(path).stem
        x, y, side = crop
        budget = int(kb * 1024)
        w, h = Image.open(path).size
        entry = {"name": name, "width": w, "height": h, "budget": budget, "crop": [x, y, side], "codecs": []}
        Image.open(path).convert("RGB").crop((x, y, x + side, y + side)).save(out / f"{name}_original.png")
        with tempfile.TemporaryDirectory() as td:
            for codec in args.codecs.split(","):
                param, size, enc = fit(codec, path, budget, td)
                dec = os.path.join(td, f"{codec}.png")
                decode(codec, enc, dec)
                m = compare(path, dec)
                Image.open(dec).convert("RGB").crop((x, y, x + side, y + side)).save(out / f"{name}_{codec}.png")
                entry["codecs"].append({
                    "codec": codec, "label": LABELS[codec], "param": param, "bytes": size,
                    "ssimulacra2": round(m["ssimulacra2"], 1), "psnr": round(m["psnr"], 2),
                })
                print(f"{name} {codec:7s} param {param} {size} B  SSIMULACRA2 {m['ssimulacra2']:.1f}", flush=True)
        meta.append(entry)
    (out / "gallery.json").write_text(json.dumps(meta, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
