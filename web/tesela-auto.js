// Soporte automático de imágenes Tesela en cualquier página:
//
//   <script src="/tesela-auto.js"></script>        (mejor en <head>)
//   <img src="foto.tsl" alt="...">                  (HTML normal)
//
// Registra el Service Worker (tesela-sw.js, junto a este archivo o donde
// indique data-sw), que entrega los .tsl al navegador ya decodificados. En la
// primera visita, mientras el Service Worker aún no controla la página, las
// imágenes .tsl se recargan en cuanto toma el control. Sin Service Worker
// (navegador antiguo o página abierta como archivo), se decodifican aquí.
(function () {
  'use strict';
  const script = document.currentScript;
  const base = script ? script.src : location.href;
  const swUrl = new URL((script && script.dataset.sw) || 'tesela-sw.js', base).href;
  const isTsl = (src) => /\.tsl(\?|#|$)/i.test(src || '');

  // Oculta las .tsl hasta que carguen (evita ver el icono de imagen rota).
  const style = document.createElement('style');
  style.textContent = 'img[data-tsl-pending]{visibility:hidden}';
  document.head.appendChild(style);

  const pending = new Set();
  function track(img) {
    if (!isTsl(img.currentSrc || img.src) || img.dataset.tslDone) return;
    if (img.complete && img.naturalWidth > 0) {
      img.dataset.tslDone = '1';
      return;
    }
    img.setAttribute('data-tsl-pending', '');
    pending.add(img);
  }
  document.addEventListener(
    'load',
    (e) => {
      const img = e.target;
      if (img instanceof HTMLImageElement && img.naturalWidth > 0 && isTsl(img.currentSrc || img.src)) {
        img.removeAttribute('data-tsl-pending');
        img.dataset.tslDone = '1';
        pending.delete(img);
      }
    },
    true,
  );
  const scan = (root) => {
    if (root instanceof HTMLImageElement) track(root);
    root.querySelectorAll && root.querySelectorAll('img').forEach(track);
  };
  new MutationObserver((muts) => {
    for (const m of muts) {
      if (m.type === 'attributes') track(m.target);
      else m.addedNodes.forEach(scan);
    }
  }).observe(document.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ['src', 'srcset'] });
  if (document.readyState !== 'loading') scan(document);
  else document.addEventListener('DOMContentLoaded', () => scan(document));

  // Vuelve a pedir las imágenes que fallaron antes de que el Service Worker
  // controlara la página (añade un parámetro para saltarse la caché de imágenes).
  function retry() {
    document.querySelectorAll('img').forEach((img) => {
      track(img);
      if (!pending.has(img)) return;
      const u = new URL(img.getAttribute('src'), location.href);
      u.searchParams.set('tsl', '1');
      img.src = u.href;
    });
  }

  // Alternativa sin Service Worker: decodifica en la página.
  async function polyfill() {
    const { toObjectURL } = await import(new URL('tesela.js', base).href);
    const fix = async (img) => {
      if (img.dataset.tslPolyfill || !isTsl(img.getAttribute('src'))) return;
      img.dataset.tslPolyfill = '1';
      try {
        const res = await fetch(img.getAttribute('src'));
        img.src = await toObjectURL(await res.arrayBuffer());
      } catch (e) {
        console.warn('Tesela:', e);
      }
    };
    document.querySelectorAll('img').forEach(fix);
    new MutationObserver((muts) => muts.forEach((m) => m.addedNodes.forEach((n) => {
      if (n instanceof HTMLImageElement) fix(n);
      else if (n.querySelectorAll) n.querySelectorAll('img').forEach(fix);
    }))).observe(document.documentElement, { childList: true, subtree: true });
  }

  if ('serviceWorker' in navigator && location.protocol !== 'file:') {
    navigator.serviceWorker.register(swUrl).then(
      () => {
        if (!navigator.serviceWorker.controller) {
          navigator.serviceWorker.addEventListener('controllerchange', retry, { once: true });
        }
      },
      (e) => {
        console.warn('Tesela: no se pudo registrar el Service Worker, se decodifica en la página.', e);
        polyfill();
      },
    );
  } else {
    polyfill();
  }
})();
