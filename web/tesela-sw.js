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

/** Perfil ICC del archivo .tsl (sección de metadatos de la v2), o null. */
function teselaIcc(bytes) {
  const HEADER = 23;
  if (bytes.length < HEADER + 4 || bytes[3] < 2 || !(bytes[22] & 32)) return null;
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const end = HEADER + 4 + dv.getUint32(HEADER, true);
  for (let p = HEADER + 4; p + 8 <= Math.min(end, bytes.length); ) {
    const n = dv.getUint32(p + 4, true);
    if (p + 8 + n > end) break;
    if (String.fromCharCode(...bytes.subarray(p, p + 4)) === 'ICCP') return bytes.slice(p + 8, p + 8 + n);
    p += 8 + n;
  }
  return null;
}

/** Contenido de un fragmento iCCP: nombre, método 0 y el perfil comprimido con zlib. */
async function iccpChunk(icc) {
  if (!icc || typeof CompressionStream === 'undefined') return null;
  const z = new Uint8Array(await new Response(new Blob([icc]).stream().pipeThrough(new CompressionStream('deflate'))).arrayBuffer());
  const name = new TextEncoder().encode('ICC profile');
  const out = new Uint8Array(name.length + 2 + z.length);
  out.set(name, 0);
  out.set(z, name.length + 2); // separador 0 y método de compresión 0
  return out;
}

/** Flujo zlib con bloques «stored» (sin comprimir) de las filas RGBA con filtro 0. */
function zlibRows(width, height, rgba) {
  const rowLen = width * 4 + 1;
  const raw = height * rowLen;
  const nblocks = Math.max(1, Math.ceil(raw / 65535));
  const out = new Uint8Array(2 + raw + nblocks * 5 + 4);
  let p = 0;
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
  new DataView(out.buffer).setUint32(p, ((b << 16) | a) >>> 0);
  return out;
}

/**
 * PNG RGBA sin comprimir; con varios fotogramas, APNG (lo reproducen todos los
 * navegadores actuales en un <img>). `frames`: [{ rgba, duration }].
 */
function encodePng(width, height, frames, loops = 0, iccp = null) {
  const parts = [new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])];
  const chunk = (type, data) => {
    const c = new Uint8Array(12 + data.length);
    const dv = new DataView(c.buffer);
    dv.setUint32(0, data.length);
    for (let i = 0; i < 4; i++) c[4 + i] = type.charCodeAt(i);
    c.set(data, 8);
    dv.setUint32(8 + data.length, (crc32(c, 4, 8 + data.length) ^ 0xffffffff) >>> 0);
    parts.push(c);
  };
  const u32 = (...v) => {
    const b = new Uint8Array(4 * v.length);
    v.forEach((x, i) => new DataView(b.buffer).setUint32(4 * i, x));
    return b;
  };
  const ihdr = new Uint8Array(13);
  ihdr.set(u32(width, height), 0);
  ihdr.set([8, 6, 0, 0, 0], 8); // 8 bits, RGBA, deflate, sin filtro, sin entrelazado
  chunk('IHDR', ihdr);
  // Perfil de color: el navegador gestiona el color como con cualquier PNG.
  if (iccp) chunk('iCCP', iccp);
  const anim = frames.length > 1;
  if (anim) chunk('acTL', u32(frames.length, loops));
  let seq = 0;
  frames.forEach((f, k) => {
    if (anim) {
      const fc = new Uint8Array(26);
      fc.set(u32(seq++, width, height, 0, 0), 0);
      new DataView(fc.buffer).setUint16(20, Math.min(65535, f.duration));
      new DataView(fc.buffer).setUint16(22, 1000);
      // dispose 0, blend 0 (cada fotograma es el lienzo completo)
      chunk('fcTL', fc);
    }
    const z = zlibRows(width, height, f.rgba);
    if (k === 0) chunk('IDAT', z);
    else {
      const fd = new Uint8Array(4 + z.length);
      fd.set(u32(seq++), 0);
      fd.set(z, 4);
      chunk('fdAT', fd);
    }
  });
  chunk('IEND', new Uint8Array(0));
  return new Blob(parts, { type: 'image/png' });
}

/** Fotogramas de una animación .tsl (o null si no cabe). */
async function decodeTeselaAnimation(bytes) {
  const ex = await loadWasm();
  const ptr = ex.tsl_alloc(bytes.length);
  new Uint8Array(ex.memory.buffer).set(bytes, ptr);
  const out = ex.tsl_decode_animation(ptr, bytes.length);
  ex.tsl_free(ptr, bytes.length);
  if (out === 0) return null;
  const len = new DataView(ex.memory.buffer).getUint32(out, true);
  const d = new Uint8Array(ex.memory.buffer).slice(out + 4, out + len);
  ex.tsl_free(out, len);
  const dv = new DataView(d.buffer);
  const [width, height, loops, n] = [0, 4, 8, 12].map((o) => dv.getUint32(o, true));
  const size = width * height * 4;
  const frames = [];
  for (let i = 0, p = 16; i < n; i++, p += 4 + size) {
    frames.push({ duration: dv.getUint32(p, true), rgba: d.subarray(p + 4, p + 4 + size) });
  }
  return { width, height, loops, frames };
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
        const bytes = new Uint8Array(await res.arrayBuffer());
        const iccp = await iccpChunk(teselaIcc(bytes));
        const headers = { 'Content-Type': 'image/png' };
        const cc = res.headers.get('Cache-Control');
        if (cc) headers['Cache-Control'] = cc;
        // Animación (bit 64 de tools en la v2): APNG si cabe (≤ 96 MB sin comprimir).
        if (bytes.length > 23 && bytes[3] >= 2 && bytes[22] & 64) {
          const a = await decodeTeselaAnimation(bytes);
          if (a && a.frames.length * a.width * a.height * 4 <= 96 << 20) {
            return new Response(encodePng(a.width, a.height, a.frames, a.loops, iccp), { headers });
          }
        }
        const img = await decodeTesela(bytes);
        return new Response(encodePng(img.width, img.height, [{ rgba: img.rgba, duration: 0 }], 0, iccp), { headers });
      } catch (e) {
        return new Response('Tesela: ' + e.message, { status: 415 });
      }
    })(),
  );
});
