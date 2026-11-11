#!/usr/bin/env python3
"""Banco de animación: Tesela frente a GIF, WebP animado y AVIF animado.

    python3 bench/gen_animations.py && python3 bench/anim_bench.py

Para cada animación (fotogramas PNG de bench/data/anim/<nombre>/) mide el
tamaño total y el SSIMULACRA2 medio por fotograma (cada fotograma decodificado
frente a su original, respetando las duraciones). Los formatos con pérdida se
barren en calidad y se interpola el tamaño a igual SSIMULACRA2.
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageSequence

ROOT = Path(__file__).resolve().parent.parent
TESELA = os.environ.get("TESELA_BIN", str(ROOT / "target" / "release" / "tesela"))
DATA = ROOT / "bench" / "data" / "anim"
QS = {"tesela": [40, 60, 75, 85, 93], "webp": [20, 40, 60, 80, 95], "avif": [20, 40, 60, 75, 90]}
TARGETS = [70, 80, 90]


def ssim2(a: Path, b: Path) -> float:
    out = subprocess.run([TESELA, "compare", str(a), str(b)], check=True, capture_output=True, text=True).stdout
    return json.loads(out)["ssimulacra2"]


def expand(frames, durs, ms, n):
    """Fotogramas decodificados → uno por fotograma original, según las duraciones."""
    out = []
    for f, d in zip(frames, durs):
        out += [f] * max(1, round(d / ms))
    return (out + [out[-1]] * n)[:n]


def pil_frames(path):
    im = Image.open(path)
    frames, durs = [], []
    for fr in ImageSequence.Iterator(im):
        frames.append(fr.convert("RGB").copy())
        durs.append(fr.info.get("duration", im.info.get("duration", 0)))
    return frames, durs


def measure(codec, src, ms, q, td):
    n = len(src)
    out = Path(td) / codec
    if codec == "gif":
        src[0].save(out, "GIF", save_all=True, append_images=src[1:], duration=ms, loop=0, optimize=True)
    elif codec == "webp":
        src[0].save(out, "WEBP", save_all=True, append_images=src[1:], duration=ms, loop=0, quality=q, method=6)
    elif codec == "webp-ll":
        src[0].save(out, "WEBP", save_all=True, append_images=src[1:], duration=ms, loop=0, lossless=True, method=6)
    elif codec == "avif":
        src[0].save(out, "AVIF", save_all=True, append_images=src[1:], duration=ms, loop=0, quality=q, speed=4)
    elif codec == "tesela":
        apng = Path(td) / "in.png"
        src[0].save(apng, "PNG", save_all=True, append_images=src[1:], duration=ms, loop=0)
        subprocess.run([TESELA, "encode", str(apng), str(out), "-q", str(q)], check=True, capture_output=True)
    size = out.stat().st_size
    if codec == "tesela":
        dd = Path(td) / "dec/"
        subprocess.run([TESELA, "decode", str(out), str(dd) + "/"], check=True, capture_output=True)
        durs = [int(x) for x in (dd / "duraciones_ms").read_text().split()]
        frames = [Image.open(dd / f"{i:03d}.png").convert("RGB") for i in range(len(durs))]
    else:
        frames, durs = pil_frames(out)
    dec = expand(frames, durs, ms, n)
    scores = []
    for i, (a, b) in enumerate(zip(src, dec)):
        pa, pb = Path(td) / "a.png", Path(td) / "b.png"
        a.save(pa)
        b.save(pb)
        scores.append(ssim2(pa, pb))
    return {"codec": codec, "q": q, "bytes": size, "ssimulacra2": float(np.mean(scores))}


def size_at(rows, target):
    pts = sorted((r["ssimulacra2"], r["bytes"]) for r in rows)
    for (s0, b0), (s1, b1) in zip(pts, pts[1:]):
        if s0 <= target <= s1 and s1 > s0:
            return np.exp(np.log(b0) + (np.log(b1) - np.log(b0)) * (target - s0) / (s1 - s0))
    return None


def main():
    names = sys.argv[1:] or sorted(p.name for p in DATA.iterdir() if p.is_dir())
    report = {}
    for name in names:
        d = DATA / name
        src = [Image.open(p).convert("RGB") for p in sorted(d.glob("*.png"))]
        ms = int((d / "duracion_ms").read_text())
        rows = []
        with tempfile.TemporaryDirectory() as td:
            for codec in ["gif", "webp-ll"]:
                rows.append(measure(codec, src, ms, None, td))
                print(name, rows[-1], flush=True)
            for codec, qs in QS.items():
                for q in qs:
                    rows.append(measure(codec, src, ms, q, td))
                    print(name, rows[-1], flush=True)
        at = {}
        for t in TARGETS:
            at[t] = {c: size_at([r for r in rows if r["codec"] == c], t) for c in QS}
        report[name] = {"frames": len(src), "size": src[0].size, "ms": ms, "rows": rows, "size_at": at}
    out = ROOT / "bench" / "results" / "animacion"
    out.mkdir(parents=True, exist_ok=True)
    (out / "raw.json").write_text(json.dumps(report, indent=1))
    lines = ["# Banco de animación", "", "Tamaño total a igual SSIMULACRA2 medio por fotograma (KB; menos es mejor).", ""]
    for name, r in report.items():
        lines += [f"## {name}: {r['frames']} fotogramas de {r['size'][0]}×{r['size'][1]}", "",
                  "| SSIMULACRA2 | Tesela | WebP animado | AVIF animado |", "|---|---|---|---|"]
        for t in TARGETS:
            row = r["size_at"][t]
            cell = lambda v: f"{v / 1024:.1f}" if v else "—"
            lines.append(f"| {t} | {cell(row['tesela'])} | {cell(row['webp'])} | {cell(row['avif'])} |")
        g = next(x for x in r["rows"] if x["codec"] == "gif")
        w = next(x for x in r["rows"] if x["codec"] == "webp-ll")
        lines += ["", f"GIF: {g['bytes'] / 1024:.1f} KB (SSIMULACRA2 {g['ssimulacra2']:.1f}). "
                  f"WebP sin pérdida: {w['bytes'] / 1024:.1f} KB.", ""]
    (out / "REPORT.md").write_text("\n".join(lines))
    print("\n".join(lines))


if __name__ == "__main__":
    main()
