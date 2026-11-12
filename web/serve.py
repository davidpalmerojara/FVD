#!/usr/bin/env python3
"""Servidor local para la web de Tesela (portada, comparador y demo).

Funciona igual en Windows, macOS, Linux y WSL:

    python web/serve.py            # http://localhost:8000/
    python web/serve.py 8080       # otro puerto

Fuerza los tipos MIME de .js y .wasm: en algunos Windows el registro hace que
el servidor de Python los sirva como text/plain y el navegador se niega a
cargar los módulos.

Además envía las cabeceras de aislamiento entre orígenes (COOP y COEP), que
el navegador exige para compartir memoria entre workers: con ellas, Tesela
codifica con varios hilos en el comparador.

Registro: cada foto que termina el comparador se guarda en `registro/` (en la
raíz del repositorio): el original, la imagen probada (tras reducirla) y los
resultados de cada formato. Se consulta en http://localhost:8000/registro/ y
se reproduce con los códecs nativos con `python bench/registro.py`. La carpeta
no se sube a Git (está en .gitignore): son tus fotos.
"""

import datetime
import email.parser
import email.policy
import functools
import hashlib
import http.server
import json
import mimetypes
import os
import re
import sys

REGISTRO = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "registro")
MAX_POST = 200 * 2**20

mimetypes.add_type("text/javascript", ".js")
mimetypes.add_type("text/javascript", ".mjs")
mimetypes.add_type("application/wasm", ".wasm")


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".js": "text/javascript",
        ".mjs": "text/javascript",
        ".wasm": "application/wasm",
    }

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        super().end_headers()

    def _json(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path != "/api/registro":
            return self.send_error(404)
        n = int(self.headers.get("Content-Length", 0))
        if n <= 0 or n > MAX_POST:
            return self.send_error(413)
        body = self.rfile.read(n)
        head = f"Content-Type: {self.headers.get('Content-Type', '')}\r\n\r\n".encode()
        msg = email.parser.BytesParser(policy=email.policy.default).parsebytes(head + body)
        parts = {p.get_param("name", header="content-disposition"): p for p in msg.iter_parts()}
        try:
            meta = json.loads(parts["meta"].get_content())
            probada = parts["probada"].get_payload(decode=True)
        except (KeyError, ValueError):
            return self.send_error(400)
        original = parts["original"].get_payload(decode=True) if "original" in parts else None
        # Una carpeta por foto (por el contenido del original): las pruebas
        # repetidas de la misma foto se añaden como ejecuciones.
        h = hashlib.sha256(original or probada).hexdigest()[:12]
        nombre = re.sub(r"[^A-Za-z0-9._-]+", "_", meta.get("nombre", "foto"))[:60] or "foto"
        carpeta = os.path.join(REGISTRO, f"{h}_{os.path.splitext(nombre)[0]}")
        os.makedirs(os.path.join(carpeta, "ejecuciones"), exist_ok=True)
        if original is not None:
            ext = os.path.splitext(nombre)[1].lower() or ".bin"
            ruta = os.path.join(carpeta, "original" + ext)
            if not os.path.exists(ruta):
                with open(ruta, "wb") as f:
                    f.write(original)
        pw, ph = meta.get("probada", {}).get("w"), meta.get("probada", {}).get("h")
        ruta = os.path.join(carpeta, f"probada_{pw}x{ph}.png")
        if not os.path.exists(ruta):
            with open(ruta, "wb") as f:
                f.write(probada)
        meta["carpeta"] = os.path.basename(carpeta)
        meta["probada_png"] = os.path.basename(ruta)
        sello = datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
        with open(os.path.join(carpeta, "ejecuciones", sello + ".json"), "w", encoding="utf-8") as f:
            json.dump(meta, f, ensure_ascii=False, indent=1)
        with open(os.path.join(REGISTRO, "registro.jsonl"), "a", encoding="utf-8") as f:
            f.write(json.dumps(meta, ensure_ascii=False) + "\n")
        self._json(200, {"ok": True, "carpeta": meta["carpeta"]})

    def do_GET(self):
        if self.path.split("?")[0] == "/api/registro":
            filas = []
            try:
                with open(os.path.join(REGISTRO, "registro.jsonl"), encoding="utf-8") as f:
                    filas = [json.loads(l) for l in f if l.strip()]
            except FileNotFoundError:
                pass
            return self._json(200, filas)
        if self.path.startswith("/registro-archivos/"):
            # Solo archivos dentro de registro/ (nada de «..»).
            rel = os.path.normpath(self.path[len("/registro-archivos/"):].split("?")[0]).lstrip("/\\")
            ruta = os.path.join(REGISTRO, rel)
            if rel.startswith("..") or not os.path.isfile(ruta):
                return self.send_error(404)
            with open(ruta, "rb") as f:
                data = f.read()
            self.send_response(200)
            self.send_header("Content-Type", mimetypes.guess_type(ruta)[0] or "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        super().do_GET()


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    root = os.path.dirname(os.path.abspath(__file__))
    handler = functools.partial(Handler, directory=root)
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as srv:
        print(f"Tesela en http://localhost:{port}/  (comparador: http://localhost:{port}/compare/)")
        print(f"Registro de lo que pruebes en el comparador: http://localhost:{port}/registro/  ({REGISTRO})")
        print("Pulsa Ctrl+C para parar.")
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
