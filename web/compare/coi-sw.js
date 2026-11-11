// Añade las cabeceras de aislamiento entre orígenes (COOP/COEP) a las
// respuestas del comparador, para servidores que no las envían (como GitHub
// Pages). Sin ellas el navegador no deja compartir memoria entre workers y
// Tesela tendría que codificar con un solo hilo.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.cache === 'only-if-cached' && req.mode !== 'same-origin') return;
  if (new URL(req.url).origin !== self.location.origin) return;
  event.respondWith(
    fetch(req).then((res) => {
      if (res.status === 0) return res;
      const headers = new Headers(res.headers);
      headers.set('Cross-Origin-Opener-Policy', 'same-origin');
      headers.set('Cross-Origin-Embedder-Policy', 'require-corp');
      headers.set('Cross-Origin-Resource-Policy', 'same-origin');
      return new Response(res.body, { status: res.status, statusText: res.statusText, headers });
    }),
  );
});
