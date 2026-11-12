#!/usr/bin/env python3
"""Resumen del registro del comparador y reproducción con los códecs nativos.

El comparador web (servido con `python web/serve.py`) guarda cada foto que
termina en `registro/`: el original, la imagen probada (ya reducida) y los
resultados de cada formato en el navegador. Este script:

    python bench/registro.py               # resumen de lo probado en el navegador
    python bench/registro.py --banco       # además, repite las imágenes con los
                                           # códecs nativos (bench/bench.py) y
                                           # compara foto a foto a igual calidad
    python bench/registro.py --banco --objetivo 85 --jobs 4

Con --banco, los resultados quedan en registro/_banco/ (REPORT.md con el
BD-rate y raw.json con todos los puntos).
"""
import argparse
import json
import math
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REG = Path(os.environ.get("TESELA_REGISTRO", ROOT / "registro"))
CODECS = ["tesela", "jxl", "avif", "webp", "jpeg"]
NOMBRES = {"tesela": "Tesela", "jxl": "JPEG XL", "avif": "AVIF", "webp": "WebP", "jpeg": "JPEG"}


def cargar():
    f = REG / "registro.jsonl"
    if not f.exists():
        sys.exit("No hay registro todavía: prueba fotos en el comparador con `python web/serve.py`.")
    return [json.loads(l) for l in f.read_text(encoding="utf-8").splitlines() if l.strip()]


def tam(e, c):
    r = e["resultados"].get(c)
    if not r or r.get("status") != "ok":
        return None
    return r.get("sizeAtTarget") if e["modo"] == "target" else r.get("size")


def resumen(filas):
    print(f"{len({e['carpeta'] for e in filas})} fotos, {len(filas)} pruebas en el navegador\n")
    print(f"{'foto':34} {'probada':>10} {'obj':>4} " + " ".join(f"{NOMBRES[c]:>9}" for c in CODECS) + "   Tesela/mejor")
    logs = {c: [] for c in CODECS[1:]}
    for e in sorted(filas, key=lambda e: e["fecha"]):
        t = tam(e, "tesela")
        otros = {c: tam(e, c) for c in CODECS[1:]}
        for c, s in otros.items():
            if t and s:
                logs[c].append(math.log(t / s))
        vals = [(s, c) for c, s in otros.items() if s]
        mejor = min(vals) if vals else None
        dif = f"{100 * (t / mejor[0] - 1):+6.1f} % ({NOMBRES[mejor[1]]})" if t and mejor else "—"
        obj = e["objetivo"] if e["modo"] == "target" else "fija"
        celdas = " ".join(f"{(tam(e, c) or 0) / 1024:8.1f}K" if tam(e, c) else f"{'—':>9}" for c in CODECS)
        print(f"{e['nombre'][:34]:34} {e['probada']['w']:>4}×{e['probada']['h']:<5} {obj:>4} {celdas}   {dif}")
    print()
    for c, v in logs.items():
        if v:
            m = (math.exp(sum(v) / len(v)) - 1) * 100
            gana = sum(1 for x in v if x < 0)
            print(f"Tesela frente a {NOMBRES[c]:8}: {m:+6.1f} % de media; gana en {gana} de {len(v)}")


def size_at(rows, target):
    pts = sorted((r["ssimulacra2"], r["bytes"]) for r in rows)
    for (s0, b0), (s1, b1) in zip(pts, pts[1:]):
        if s0 <= target <= s1 and s1 > s0:
            return math.exp(math.log(b0) + (math.log(b1) - math.log(b0)) * (target - s0) / (s1 - s0))
    return None


def banco(filas, objetivo, jobs):
    dst = REG / "_banco"
    img = dst / "imagenes"
    if img.exists():
        shutil.rmtree(img)
    img.mkdir(parents=True)
    ultimas = {}
    for e in sorted(filas, key=lambda e: e["fecha"]):
        ultimas[e["carpeta"]] = e
    for carpeta, e in ultimas.items():
        shutil.copy(REG / carpeta / e["probada_png"], img / f"{carpeta}.png")
    print(f"\nBanco nativo con {len(ultimas)} imágenes (Tesela, WebP, AVIF, JPEG XL)…")
    subprocess.run(
        [sys.executable, str(ROOT / "bench" / "bench.py"), "--images", str(img), "--codecs", "tesela,webp,avif,jxl",
         "--jobs", str(jobs), "--out", str(dst), "--note", "Imágenes del registro del comparador."],
        check=True,
    )
    raw = json.loads((dst / "raw.json").read_text())
    print(f"\nFoto a foto a SSIMULACRA2 = {objetivo} (códecs nativos; peor para Tesela primero):")
    out = []
    for name in sorted({r["image"] for r in raw}):
        s = {c: size_at([r for r in raw if r["image"] == name and r["codec"] == c], objetivo) for c in ["tesela", "jxl", "avif", "webp"]}
        if not s["tesela"]:
            continue
        rivales = {c: v for c, v in s.items() if c != "tesela" and v}
        if not rivales:
            continue
        c, v = min(rivales.items(), key=lambda kv: kv[1])
        out.append((s["tesela"] / v - 1, name, s, c))
    for dif, name, s, c in sorted(out, reverse=True):
        cel = " ".join(f"{NOMBRES[k]} {v / 1024:7.1f}K" if v else f"{NOMBRES[k]}     —  " for k, v in s.items())
        print(f"  {dif * 100:+6.1f} % frente a {NOMBRES[c]:8} {name[:40]:40} {cel}")
    print(f"\nInforme completo: {dst / 'REPORT.md'}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--banco", action="store_true", help="repetir las imágenes con los códecs nativos")
    ap.add_argument("--objetivo", type=float, default=80, help="SSIMULACRA2 para la comparación foto a foto")
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    args = ap.parse_args()
    filas = cargar()
    resumen(filas)
    if args.banco:
        banco(filas, args.objetivo, args.jobs)


if __name__ == "__main__":
    main()
