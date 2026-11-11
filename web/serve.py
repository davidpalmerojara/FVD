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
"""

import functools
import http.server
import mimetypes
import os
import sys

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


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    root = os.path.dirname(os.path.abspath(__file__))
    handler = functools.partial(Handler, directory=root)
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as srv:
        print(f"Tesela en http://localhost:{port}/  (comparador: http://localhost:{port}/compare/)")
        print("Pulsa Ctrl+C para parar.")
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
