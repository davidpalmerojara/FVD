#!/usr/bin/env python3
"""Genera las animaciones de prueba del banco de animación (fotogramas PNG).

    python3 bench/gen_animations.py [directorio]   (por defecto bench/data/anim)

- terminal: texto que se va escribiendo en un terminal, con cursor parpadeante.
- interfaz: una tarjeta con barra de progreso y un indicador que se mueve.
- foto: un paneo lento sobre una foto (todo el fotograma cambia).
- zoom: un zoom lento sobre una foto (movimiento de subpíxel, como una cámara).
- objetos: tres recortes que se mueven (y uno gira) sobre un fondo quieto,
  como un vídeo con movimiento local.
"""
import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "bench" / "data" / "anim"
MONO = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
SANS = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"


def font(path, size):
    try:
        return ImageFont.truetype(path, size)
    except OSError:
        return ImageFont.load_default()


def terminal(d: Path):
    lines = [
        "$ cargo build --release",
        "   Compiling tesela v0.1.0",
        "    Finished `release` profile in 24.1s",
        "$ ./target/release/tesela encode foto.jpg foto.tsl -q 85",
        "112394 bytes, 1.0021 bpp, 0.94 s",
        "$ ls -l foto.*",
        "-rw-r--r-- 1 ana ana 412833 foto.jpg",
        "-rw-r--r-- 1 ana ana 112394 foto.tsl",
    ]
    f = font(MONO, 15)
    text = "\n".join(lines)
    frames = []
    shown = 0
    t = 0
    while shown <= len(text) or t % 2:
        im = Image.new("RGB", (640, 400), (24, 26, 33))
        dr = ImageDraw.Draw(im)
        dr.rectangle([0, 0, 640, 26], fill=(40, 43, 54))
        for i, c in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
            dr.ellipse([12 + 20 * i, 8, 24 + 20 * i, 20], fill=c)
        dr.text((280, 6), "bash", font=font(SANS, 13), fill=(170, 170, 180))
        cur = text[:shown]
        y = 40
        for ln in cur.split("\n"):
            col = (120, 220, 140) if ln.startswith("$") else (210, 214, 224)
            dr.text((16, y), ln, font=f, fill=col)
            y += 22
        last = cur.split("\n")[-1]
        cx = 16 + int(dr.textlength(last, font=f))
        if t % 2 == 0:
            dr.rectangle([cx, y - 22 + 2, cx + 8, y - 22 + 18], fill=(220, 220, 220))
        frames.append(im)
        shown += 3
        t += 1
    return frames, 80


def interfaz(d: Path):
    frames = []
    f = font(SANS, 18)
    s = font(SANS, 13)
    for t in range(40):
        im = Image.new("RGB", (480, 320), (243, 244, 246))
        dr = ImageDraw.Draw(im)
        dr.rounded_rectangle([40, 40, 440, 280], 16, fill=(255, 255, 255), outline=(225, 227, 232))
        dr.text((70, 70), "Subiendo fotos", font=f, fill=(30, 32, 40))
        dr.text((70, 100), f"{min(100, t * 3)} % completado", font=s, fill=(110, 114, 125))
        dr.rounded_rectangle([70, 140, 410, 156], 8, fill=(229, 231, 235))
        dr.rounded_rectangle([70, 140, 70 + int(340 * min(1, t / 33)), 156], 8, fill=(194, 65, 12))
        a = t * 0.35
        import math
        cx, cy = 240 + int(60 * math.cos(a)), 215 + int(20 * math.sin(a))
        dr.ellipse([cx - 14, cy - 14, cx + 14, cy + 14], fill=(37, 99, 235))
        frames.append(im)
    return frames, 60


def foto(d: Path):
    src = Image.open(ROOT / "web" / "compare" / "sample.png").convert("RGB")
    frames = []
    for t in range(30):
        x = 40 + t * 4
        frames.append(src.crop((x, 60, x + 400, 360)))
    return frames, 70


def zoom(d: Path):
    """Zoom lento sobre una foto (movimiento de subpíxel, como una cámara real)."""
    src = Image.open(ROOT / "web" / "compare" / "sample.png").convert("RGB")
    frames = []
    for t in range(30):
        s = 1.0 + 0.006 * t
        w, h = 400 / s, 300 / s
        cx, cy = 400, 230
        box = (cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)
        frames.append(src.resize((400, 300), Image.LANCZOS, box=box))
    return frames, 70


def objetos(d: Path):
    """Movimiento local: recortes que se desplazan en direcciones distintas y uno que gira."""
    src = Image.open(ROOT / "web" / "compare" / "sample.png").convert("RGB")
    bg = src.crop((300, 150, 700, 450))
    p1, p2, p3 = src.crop((0, 0, 90, 90)), src.crop((600, 350, 700, 430)), src.crop((100, 380, 180, 460))
    frames = []
    for t in range(40):
        f = bg.copy()
        f.paste(p1, (int(10 + 6 * t), int(20 + 2 * t)))
        f.paste(p2, (int(290 - 5 * t), int(200 - 1.5 * t)))
        f.paste(p3.rotate(4 * t, resample=Image.BICUBIC), (int(160 + 40 * math.sin(t / 6)), int(120 + 30 * math.cos(t / 6))))
        frames.append(f)
    return frames, 60


def main():
    for name, gen in [("terminal", terminal), ("interfaz", interfaz), ("foto", foto), ("zoom", zoom), ("objetos", objetos)]:
        d = OUT / name
        d.mkdir(parents=True, exist_ok=True)
        frames, ms = gen(d)
        for i, im in enumerate(frames):
            im.save(d / f"{i:03d}.png")
        (d / "duracion_ms").write_text(str(ms))
        print(f"{name}: {len(frames)} fotogramas de {frames[0].size[0]}×{frames[0].size[1]}, {ms} ms")


if __name__ == "__main__":
    main()
