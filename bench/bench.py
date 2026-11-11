#!/usr/bin/env python3
"""Banco de pruebas: Tesela frente a WebP, AVIF y JPEG XL.

Para cada imagen y códec recorre varios niveles de calidad, codifica,
decodifica a PNG y mide (con el mismo binario `tesela compare` para todos):
PSNR RGB, PSNR-Y, SSIM-Y y SSIMULACRA2.

Después calcula el BD-rate (Bjøntegaard) de cada códec frente a WebP: el
porcentaje medio de bits que se ahorra (negativo = mejor) a igual calidad.

Uso:
    cargo build --release
    bench/fetch_kodak.sh
    python3 bench/bench.py --images bench/data/kodak
"""

import argparse
import io
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
TESELA = os.environ.get("TESELA_BIN", str(ROOT / "target" / "release" / "tesela"))

SWEEPS = {
    "tesela": [20, 35, 50, 62, 72, 80, 87, 93, 97],
    "tesela-psnr": [20, 35, 50, 62, 72, 80, 87, 93, 97],
    "webp": [5, 15, 30, 50, 70, 80, 88, 94, 98],
    "avif": [15, 30, 45, 55, 65, 75, 82, 90, 95],
    "jxl": [8.0, 5.0, 3.5, 2.5, 1.8, 1.2, 0.8, 0.5, 0.3],
}

METRICS = ["ssimulacra2", "psnr_y", "ssim_y", "psnr"]
# Columnas de BD-rate: (clave, métrica, rango de calidad sobre el que se integra).
BD_SPECS = [
    ("ssimulacra2", "ssimulacra2", (30, 90)),
    ("ssimulacra2_hq", "ssimulacra2", (70, 90)),
    ("psnr_y", "psnr_y", (28, 48)),
    ("ssim_y", "ssim_y", (0.85, 0.995)),
    ("psnr", "psnr", (26, 46)),
]


def compare(orig: str, dist: str) -> dict:
    out = subprocess.run([TESELA, "compare", orig, dist], check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def run_one(job):
    codec, img, param, tesela_args = job
    with tempfile.TemporaryDirectory() as td:
        enc = os.path.join(td, "enc")
        dec = os.path.join(td, "dec.png")
        t0 = time.perf_counter()
        if codec in ("tesela", "tesela-psnr"):
            extra = ["--tune", "psnr"] if codec == "tesela-psnr" else []
            # Sin metadatos, como los demás códecs (que pasan por PIL .convert("RGB")).
            subprocess.run([TESELA, "encode", img, enc, "-q", str(param), "--sin-metadatos", *extra, *tesela_args],
                           check=True, capture_output=True)
            t1 = time.perf_counter()
            subprocess.run([TESELA, "decode", enc, dec], check=True, capture_output=True)
        elif codec == "webp":
            Image.open(img).convert("RGB").save(enc, "WEBP", quality=param, method=6)
            t1 = time.perf_counter()
            Image.open(enc).convert("RGB").save(dec)
        elif codec == "avif":
            Image.open(img).convert("RGB").save(enc, "AVIF", quality=param, speed=4, subsampling="4:2:0")
            t1 = time.perf_counter()
            Image.open(enc).convert("RGB").save(dec)
        elif codec == "jxl":
            import imagecodecs
            px = np.asarray(Image.open(img).convert("RGB"))
            data = imagecodecs.jpegxl_encode(px, distance=param, effort=7, numthreads=1)
            Path(enc).write_bytes(data)
            t1 = time.perf_counter()
            Image.fromarray(imagecodecs.jpegxl_decode(Path(enc).read_bytes())).save(dec)
        else:
            raise ValueError(codec)
        t2 = time.perf_counter()
        w, h = Image.open(img).size
        size = os.path.getsize(enc)
        row = {"codec": codec, "image": Path(img).name, "param": param, "bytes": size,
               "bpp": size * 8 / (w * h), "enc_s": t1 - t0, "dec_s": t2 - t1}
        row.update(compare(img, dec))
        return row


# ------------------------------------------------------------------ BD-rate

def _clean_curve(bpp, q):
    """Ordena por calidad y elimina puntos no monótonos."""
    pts = sorted(zip(q, np.log(bpp)))
    xs, ys = [], []
    for x, y in pts:
        if xs and (x <= xs[-1] or y <= ys[-1]):
            continue
        xs.append(x)
        ys.append(y)
    return np.array(xs), np.array(ys)


def bd_rate(anchor, test, lo, hi):
    """BD-rate (%) de `test` frente a `anchor`; cada uno es (bpp[], calidad[])."""
    from scipy.interpolate import PchipInterpolator

    xa, ya = _clean_curve(*anchor)
    xt, yt = _clean_curve(*test)
    if len(xa) < 2 or len(xt) < 2:
        return None
    a, b = max(xa[0], xt[0], lo), min(xa[-1], xt[-1], hi)
    if b - a < 1e-6 * max(1.0, abs(b)):
        return None
    grid = np.linspace(a, b, 200)
    da = PchipInterpolator(xa, ya)(grid)
    dt = PchipInterpolator(xt, yt)(grid)
    return (np.exp(np.trapezoid(dt - da, grid) / (b - a)) - 1) * 100


def summarize(rows, anchor="webp"):
    codecs = sorted({r["codec"] for r in rows}, key=lambda c: list(SWEEPS).index(c))
    images = sorted({r["image"] for r in rows})
    by = {}
    for r in rows:
        by.setdefault((r["codec"], r["image"]), []).append(r)
    summary = {}
    for c in codecs:
        s = {}
        for key, m, rng in BD_SPECS:
            vals = []
            for im in images:
                if (anchor, im) not in by or (c, im) not in by:
                    continue
                A = by[(anchor, im)]
                T = by[(c, im)]
                v = bd_rate(([r["bpp"] for r in A], [r[m] for r in A]),
                            ([r["bpp"] for r in T], [r[m] for r in T]), *rng)
                if v is not None:
                    vals.append(v)
            s[key] = float(np.mean(vals)) if vals else None
        cr = [r for r in rows if r["codec"] == c]
        s["enc_s"] = float(np.mean([r["enc_s"] for r in cr]))
        s["dec_s"] = float(np.mean([r["dec_s"] for r in cr]))
        summary[c] = s
    return summary


def mean_curves(rows):
    curves = {}
    for c in SWEEPS:
        cr = [r for r in rows if r["codec"] == c]
        if not cr:
            continue
        pts = []
        for p in SWEEPS[c]:
            pr = [r for r in cr if r["param"] == p]
            if pr:
                pts.append({k: float(np.mean([r[k] for r in pr])) for k in ["bpp", *METRICS]})
        curves[c] = sorted(pts, key=lambda d: d["bpp"])
    return curves


def plot(rows, out: Path):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    curves = mean_curves(rows)
    colors = {"tesela": "#d62728", "tesela-psnr": "#ff9896", "webp": "#1f77b4", "avif": "#2ca02c", "jxl": "#9467bd"}
    fig, axes = plt.subplots(1, 3, figsize=(16, 5))
    for ax, m, label in zip(axes, ["ssimulacra2", "psnr_y", "ssim_y"], ["SSIMULACRA2", "PSNR-Y (dB)", "SSIM-Y"]):
        for c, pts in curves.items():
            ax.plot([p["bpp"] for p in pts], [p[m] for p in pts], "o-", ms=3, label=c.upper(), color=colors.get(c))
        ax.set_xlabel("bits por píxel")
        ax.set_ylabel(label)
        ax.set_xlim(0, 2.5)
        ax.grid(alpha=0.3)
        ax.legend()
    fig.suptitle("Calidad media frente a tasa (Kodak)")
    fig.tight_layout()
    fig.savefig(out, dpi=110)


def write_report(rows, summary, out: Path, note: str):
    lines = ["# Resultados del banco de pruebas", ""]
    if note:
        lines += [note, ""]
    imgs = len({r["image"] for r in rows})
    lines += [f"Imágenes: {imgs}. BD-rate frente a **WebP** (negativo = menos bytes a igual calidad).", ""]
    lines += ["| Códec | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Cod. (s/img) | Dec. (s/img) |",
              "|---|---|---|---|---|---|---|---|"]
    fmt = lambda v: "—" if v is None else f"{v:+.1f} %"
    for c, s in summary.items():
        lines.append(f"| {c.upper()} | {fmt(s['ssimulacra2'])} | {fmt(s['ssimulacra2_hq'])} | {fmt(s['psnr_y'])} | {fmt(s['ssim_y'])} | "
                     f"{fmt(s['psnr'])} | {s['enc_s']:.2f} | {s['dec_s']:.3f} |")
    rng = ", ".join(f"{k}: {r[0]}–{r[1]}" for k, _, r in BD_SPECS)
    lines += ["", f"Rangos de calidad sobre los que se integra el BD-rate: {rng}.", "", "![curvas](curves.png)", ""]
    out.write_text("\n".join(lines))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--images", default=str(ROOT / "bench" / "data" / "kodak"))
    ap.add_argument("--codecs", default="tesela,tesela-psnr,webp,avif,jxl")
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    ap.add_argument("--out", default=str(ROOT / "bench" / "results"))
    ap.add_argument("--limit", type=int, default=0, help="usar solo las N primeras imágenes")
    ap.add_argument("--tesela-args", default="", help="opciones extra para `tesela encode`")
    ap.add_argument("--reuse", action="store_true", help="reutilizar resultados guardados de otros códecs")
    ap.add_argument("--note", default="")
    args = ap.parse_args()

    imgs = sorted(str(p) for p in Path(args.images).glob("*.png"))
    if args.limit:
        imgs = imgs[: args.limit]
    if not imgs:
        sys.exit(f"no hay PNG en {args.images} (ejecuta bench/fetch_kodak.sh)")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    raw = out / "raw.json"
    codecs = args.codecs.split(",")
    tesela_args = shlex.split(args.tesela_args)

    old = json.loads(raw.read_text()) if (args.reuse and raw.exists()) else []
    names = {Path(i).name for i in imgs}
    keep = [r for r in old if r["codec"] not in codecs and r["image"] in names]
    jobs = [(c, i, p, tesela_args) for c in codecs for i in imgs for p in SWEEPS[c]]
    t0 = time.time()
    rows = []
    with ProcessPoolExecutor(max_workers=args.jobs) as ex:
        for k, r in enumerate(ex.map(run_one, jobs), 1):
            rows.append(r)
            if k % 20 == 0 or k == len(jobs):
                print(f"\r{k}/{len(jobs)} trabajos ({time.time() - t0:.0f} s)", end="", file=sys.stderr, flush=True)
    print(file=sys.stderr)
    rows = keep + rows
    raw.write_text(json.dumps(rows, indent=1))
    summary = summarize(rows)
    (out / "summary.json").write_text(json.dumps(summary, indent=2))
    plot(rows, out / "curves.png")
    write_report(rows, summary, out / "REPORT.md", args.note)
    print((out / "REPORT.md").read_text())


if __name__ == "__main__":
    main()
