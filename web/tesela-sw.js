// Service Worker de Tesela: hace que cualquier petición de imagen `.tsl` de la
// web llegue al navegador como un PNG ya decodificado. Así funcionan <img>,
// srcset, <picture>, fondos CSS y "Guardar imagen como" sin cambiar el HTML.
//
// Colócalo (junto con tesela.wasm) en la raíz de tu web para que controle
// todas las páginas, e incluye tesela-auto.js en ellas.

const WASM_URL = new URL('tesela.wasm', self.location).href;
let wasmPromise = null;

function loadWasm() {
  if (!wasmPromise) {
    wasmPromise = (async () => {
      const bytes = await (await fetch(WASM_URL)).arrayBuffer();
      const module = await WebAssembly.compile(bytes);
      const imports = {};
      for (const imp of WebAssembly.Module.imports(module)) {
        (imports[imp.module] ??= {})[imp.name] = () => {
          throw new Error('import inesperado: ' + imp.name);
        };
      }
      return (await WebAssembly.instantiate(module, imports)).exports;
    })();
  }
  return wasmPromise;
}

/** Decodifica un .tsl con el módulo WebAssembly. Devuelve { width, height, rgba }. */
async function decodeTesela(bytes) {
  const ex = await loadWasm();
  const ptr = ex.tsl_alloc(bytes.length);
  new Uint8Array(ex.memory.buffer).set(bytes, ptr);
  const out = ex.tsl_decode(ptr, bytes.length);
  ex.tsl_free(ptr, bytes.length);
  const take = (p) => {
    const len = new DataView(ex.memory.buffer).getUint32(p, true);
    const data = new Uint8Array(ex.memory.buffer).slice(p + 4, p + len);
    ex.tsl_free(p, len);
    return data;
  };
  if (out === 0) throw new Error(new TextDecoder().decode(take(ex.tsl_last_error())));
  const d = take(out);
  const dv = new DataView(d.buffer);
  return { width: dv.getUint32(0, true), height: dv.getUint32(4, true), rgba: d.subarray(8) };
}

// --------------------------------------------------------------- PNG mínimo
// PNG RGBA sin comprimir (bloques "stored" de deflate): no hace falta
// comprimir porque la respuesta nunca sale del navegador.

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf, start, end, crc = 0xffffffff) {
  for (let i = start; i < end; i++) crc = CRC_TABLE[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  return crc;
}

function encodePng(width, height, rgba) {
  const rowLen = width * 4 + 1;
  const raw = height * rowLen;
  const nblocks = Math.max(1, Math.ceil(raw / 65535));
  const idatLen = 2 + raw + nblocks * 5 + 4;
  const out = new Uint8Array(8 + 25 + 12 + idatLen + 12);
  const dv = new DataView(out.buffer);
  out.set([137, 80, 78, 71, 13, 10, 26, 10], 0);
  let p = 8;
  const chunk = (type, len, write) => {
    dv.setUint32(p, len);
    const start = p + 4;
    for (let i = 0; i < 4; i++) out[start + i] = type.charCodeAt(i);
    p = start + 4;
    write();
    dv.setUint32(p, (crc32(out, start, p) ^ 0xffffffff) >>> 0);
    p += 4;
  };
  chunk('IHDR', 13, () => {
    dv.setUint32(p, width);
    dv.setUint32(p + 4, height);
    out.set([8, 6, 0, 0, 0], p + 8); // 8 bits, RGBA, deflate, sin filtro, sin entrelazado
    p += 13;
  });
  chunk('IDAT', idatLen, () => {
    out[p++] = 0x78;
    out[p++] = 0x01;
    let a = 1;
    let b = 0;
    let left = raw;
    let row = 0;
    let col = 0; // posición dentro de la fila (0 = byte de filtro)
    while (left > 0) {
      const n = Math.min(65535, left);
      left -= n;
      out[p++] = left === 0 ? 1 : 0;
      out[p++] = n & 0xff;
      out[p++] = n >> 8;
      out[p++] = ~n & 0xff;
      out[p++] = (~n >> 8) & 0xff;
      for (let i = 0; i < n; i++) {
        const v = col === 0 ? 0 : rgba[row * width * 4 + col - 1];
        out[p++] = v;
        a = (a + v) % 65521;
        b = (b + a) % 65521;
        if (++col === rowLen) {
          col = 0;
          row++;
        }
      }
    }
    dv.setUint32(p, ((b << 16) | a) >>> 0);
    p += 4;
  });
  chunk('IEND', 0, () => {});
  return out;
}

// ---------------------------------------------------------------- Eventos

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== 'GET' || !url.pathname.toLowerCase().endsWith('.tsl')) return;
  // Las peticiones de la propia librería (fetch de JavaScript) reciben el .tsl tal cual.
  if (event.request.destination !== 'image') return;
  event.respondWith(
    (async () => {
      const res = await fetch(event.request);
      if (!res.ok) return res;
      try {
        const img = await decodeTesela(new Uint8Array(await res.arrayBuffer()));
        const headers = { 'Content-Type': 'image/png' };
        const cc = res.headers.get('Cache-Control');
        if (cc) headers['Cache-Control'] = cc;
        return new Response(encodePng(img.width, img.height, img.rgba), { headers });
      } catch (e) {
        return new Response('Tesela: ' + e.message, { status: 415 });
      }
    })(),
  );
});
