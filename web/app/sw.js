// Service worker de «Convertir y abrir» (alcance /app/).
//
// 1. Sin conexión: guarda la aplicación (página, scripts, WebAssembly y
//    fuentes) y la sirve desde la caché cuando la red falla. Con red, siempre
//    pide la versión nueva (red primero), así que publicar cambios no exige
//    tocar nada aquí salvo la lista si se añaden archivos.
// 2. Aislamiento entre orígenes: el codificador con hilos necesita
//    SharedArrayBuffer, que el navegador solo permite con las cabeceras
//    COOP/COEP. Cloudflare Pages ya las envía (web/_headers); en servidores que
//    no lo hacen (GitHub Pages, un servidor estático cualquiera) se añaden aquí,
//    como hace compare/coi-sw.js. Las respuestas sacadas de la caché también
//    las llevan, para que sin conexión se siga codificando con varios hilos.
//
// Este service worker controla solo /app/; el de la raíz (tesela-sw.js, si lo
// registró otra página) sigue controlando el resto de la web.

const CACHE = 'tesela-app-v2';
const SHELL = [
  './',
  'app.css',
  'app.js',
  'png.js',
  'zip.js',
  'encode-worker.js',
  'manifest.webmanifest',
  'icons/icon-192.png',
  'icons/icon-512.png',
  '../style.css?v=4',
  '../fonts/manrope.woff2',
  '../fonts/dm-mono-regular.woff2',
  '../fonts/dm-mono-medium.woff2',
  '../tesela.js',
  '../tesela-core.js',
  '../tesela-worker.js',
  '../tesela-threads.js',
  '../tesela-thread-worker.js',
  '../tesela.wasm',
  '../tesela-mt.wasm',
].map((p) => new URL(p, self.location).href);
const INDEX = SHELL[0];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(SHELL.map((u) => new Request(u, { cache: 'reload' }))))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    (async () => {
      for (const k of await caches.keys()) if (k.startsWith('tesela-app-') && k !== CACHE) await caches.delete(k);
      await self.clients.claim();
    })(),
  );
});

/** Copia de la respuesta con COOP/COEP/CORP (y sin la marca de redirección). */
function isolate(res) {
  if (!res || res.status === 0 || res.type === 'opaqueredirect') return res;
  const headers = new Headers(res.headers);
  headers.set('Cross-Origin-Opener-Policy', 'same-origin');
  headers.set('Cross-Origin-Embedder-Policy', 'require-corp');
  headers.set('Cross-Origin-Resource-Policy', 'same-origin');
  return new Response(res.body, { status: res.status, statusText: res.statusText, headers });
}

/** Clave de caché: /app/index.html y /app/?abrir son la misma página. */
function cacheKey(req) {
  const url = new URL(req.url);
  if (req.mode === 'navigate' && url.href.startsWith(new URL('./', self.location).href)) return INDEX;
  return url.href;
}

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (req.cache === 'only-if-cached' && req.mode !== 'same-origin') return;
  const key = cacheKey(req);
  const shell = SHELL.includes(key);
  event.respondWith(
    (async () => {
      try {
        const res = await fetch(req);
        // Guarda la versión nueva de los archivos de la aplicación.
        if (shell && res.ok && res.type === 'basic' && !res.redirected) {
          const copy = res.clone();
          event.waitUntil(caches.open(CACHE).then((c) => c.put(key, copy)));
        }
        return isolate(res);
      } catch (err) {
        const hit = await caches.match(key);
        if (hit) return isolate(hit);
        throw err;
      }
    })(),
  );
});
