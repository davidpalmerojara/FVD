// Escritor de PNG RGBA de 8 bits a partir de los píxeles exactos del
// decodificador. No pasa por <canvas>: el lienzo guarda el color
// premultiplicado por el alfa y redondearía los píxeles semitransparentes.

import { crc32 } from './zip.js';

/** zlib con deflate del navegador (CompressionStream) o, si no lo hay, bloques sin comprimir. */
async function zlib(data) {
  if (typeof CompressionStream !== 'undefined') {
    const stream = new Blob([data]).stream().pipeThrough(new CompressionStream('deflate'));
    return new Uint8Array(await new Response(stream).arrayBuffer());
  }
  const n = Math.max(1, Math.ceil(data.length / 65535));
  const out = new Uint8Array(2 + data.length + 5 * n + 4);
  out[0] = 0x78;
  out[1] = 0x01;
  let p = 2;
  let a = 1;
  let b = 0;
  for (let i = 0; i < n; i++) {
    const chunk = data.subarray(i * 65535, Math.min(data.length, (i + 1) * 65535));
    out[p++] = i === n - 1 ? 1 : 0;
    out[p++] = chunk.length & 0xff;
    out[p++] = chunk.length >> 8;
    out[p++] = ~chunk.length & 0xff;
    out[p++] = (~chunk.length >> 8) & 0xff;
    out.set(chunk, p);
    p += chunk.length;
    for (let k = 0; k < chunk.length; k++) {
      a = (a + chunk[k]) % 65521;
      b = (b + a) % 65521;
    }
  }
  new DataView(out.buffer).setUint32(p, ((b << 16) | a) >>> 0);
  return out;
}

/** Predicción de cada filtro de PNG (0 nada, 1 izquierda, 2 arriba, 3 media, 4 Paeth). */
function predict(f, left, above, ul) {
  if (f === 1) return left;
  if (f === 2) return above;
  if (f === 3) return (left + above) >> 1;
  if (f === 4) {
    const pa = Math.abs(above - ul);
    const pb = Math.abs(left - ul);
    const pc = Math.abs(left + above - 2 * ul);
    return pa <= pb && pa <= pc ? left : pb <= pc ? above : ul;
  }
  return 0;
}

/** Filas con el filtro de PNG que menos suma (heurística habitual de libpng). */
function filterRows(width, height, rgba) {
  const stride = width * 4;
  const out = new Uint8Array(height * (stride + 1));
  const residual = (row, up, f, i) =>
    (row[i] - predict(f, i >= 4 ? row[i - 4] : 0, up ? up[i] : 0, up && i >= 4 ? up[i - 4] : 0)) & 0xff;
  for (let y = 0; y < height; y++) {
    const row = rgba.subarray(y * stride, (y + 1) * stride);
    const up = y > 0 ? rgba.subarray((y - 1) * stride, y * stride) : null;
    let best = 0;
    let bestSum = Infinity;
    for (let f = 0; f < 5; f++) {
      let sum = 0;
      for (let i = 0; i < stride && sum < bestSum; i++) {
        const v = residual(row, up, f, i);
        sum += v < 128 ? v : 256 - v;
      }
      if (sum < bestSum) {
        bestSum = sum;
        best = f;
      }
    }
    const o = y * (stride + 1);
    out[o] = best;
    for (let i = 0; i < stride; i++) out[o + 1 + i] = residual(row, up, best, i);
  }
  return out;
}

/**
 * PNG RGBA (Blob) con los píxeles tal cual. `icc`: perfil de color del .tsl
 * (opcional), que se guarda en un fragmento iCCP.
 */
export async function encodePng(width, height, rgba, { icc = null } = {}) {
  const parts = [new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])];
  const enc = new TextEncoder();
  const chunk = (type, data) => {
    const c = new Uint8Array(12 + data.length);
    const dv = new DataView(c.buffer);
    dv.setUint32(0, data.length);
    c.set(enc.encode(type), 4);
    c.set(data, 8);
    dv.setUint32(8 + data.length, crc32(c.subarray(4, 8 + data.length)));
    parts.push(c);
  };
  const ihdr = new Uint8Array(13);
  const hv = new DataView(ihdr.buffer);
  hv.setUint32(0, width);
  hv.setUint32(4, height);
  ihdr.set([8, 6, 0, 0, 0], 8); // 8 bits, RGBA, deflate, filtros estándar, sin entrelazado
  chunk('IHDR', ihdr);
  if (icc && icc.length) {
    const name = enc.encode('ICC profile');
    const z = await zlib(icc);
    const data = new Uint8Array(name.length + 2 + z.length);
    data.set(name, 0);
    data.set(z, name.length + 2); // separador nulo y método 0
    chunk('iCCP', data);
  }
  chunk('IDAT', await zlib(filterRows(width, height, rgba)));
  chunk('IEND', new Uint8Array(0));
  return new Blob(parts, { type: 'image/png' });
}
