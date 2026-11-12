// Worker del comparador: codifica, decodifica y mide una imagen con un códec.
// Todos los códecs se ejecutan aquí en WebAssembly. Tesela codifica con varios
// hilos si la página está aislada (crossOriginIsolated); los demás, con uno.

import { loadWasm } from '../tesela-core.js';
import { loadEncoder } from '../tesela-threads.js';

const TUNES = { balanced: 0, perceptual: 1, psnr: 2 };
const mods = {};
async function jsq(name, kind) {
  const key = `${name}/${kind}`;
  if (!mods[key]) mods[key] = import(`../vendor/jsquash-${name}/${kind}.js`).then((m) => m.default);
  return mods[key];
}

const CODECS = {
  tesela: {
    async encode(img, q, o) {
      const core = await loadEncoder();
      return core.encode(img.data, img.width, img.height, q, o.teselaEffort ?? 4, TUNES[o.teselaTune] ?? 0);
    },
    async decode(bytes) {
      const core = await loadWasm();
      return core.decode(bytes).rgba;
    },
  },
  webp: {
    async encode(img, q, o) {
      return new Uint8Array(await (await jsq('webp', 'encode'))(img, { quality: q, method: o.webpMethod ?? 6 }));
    },
    async decode(bytes) {
      return (await (await jsq('webp', 'decode'))(bytes.buffer)).data;
    },
  },
  avif: {
    async encode(img, q, o) {
      // subsample: 1 = 4:2:0 (por defecto), 3 = 4:4:4.
      return new Uint8Array(await (await jsq('avif', 'encode'))(img, { quality: q, speed: o.avifSpeed ?? 6, subsample: o.avifSub ?? 1 }));
    },
    async decode(bytes) {
      return (await (await jsq('avif', 'decode'))(bytes.buffer)).data;
    },
  },
  jxl: {
    async encode(img, q, o) {
      return new Uint8Array(await (await jsq('jxl', 'encode'))(img, { quality: q, effort: o.jxlEffort ?? 7 }));
    },
    async decode(bytes) {
      return (await (await jsq('jxl', 'decode'))(bytes.buffer)).data;
    },
  },
  jpeg: {
    async encode(img, q) {
      const c = new OffscreenCanvas(img.width, img.height);
      c.getContext('2d').putImageData(img, 0, 0);
      const blob = await c.convertToBlob({ type: 'image/jpeg', quality: q / 100 });
      return new Uint8Array(await blob.arrayBuffer());
    },
    async decode(bytes, w, h) {
      const bmp = await createImageBitmap(new Blob([bytes], { type: 'image/jpeg' }));
      const c = new OffscreenCanvas(w, h);
      const ctx = c.getContext('2d');
      ctx.drawImage(bmp, 0, 0);
      return ctx.getImageData(0, 0, w, h).data;
    },
  },
};

/** Calidad inicial aproximada para alcanzar un SSIMULACRA2 objetivo. */
function guess(codec, target) {
  const t = Math.max(30, Math.min(95, target));
  const table = {
    tesela: [20, 85, 95],
    jpeg: [20, 85, 97],
    webp: [15, 85, 97],
    avif: [20, 65, 90],
    jxl: [20, 85, 97],
  }[codec];
  // interpola entre (30→a), (80→b), (95→c)
  const [a, b, c] = table;
  return Math.round(t <= 80 ? a + ((b - a) * (t - 30)) / 50 : b + ((c - b) * (t - 80)) / 15);
}

async function runOnce(codec, img, q, opts) {
  const impl = CODECS[codec];
  const t0 = performance.now();
  const bytes = await impl.encode(img, q, opts);
  const t1 = performance.now();
  const decoded = await impl.decode(bytes, img.width, img.height);
  const t2 = performance.now();
  const core = await loadWasm();
  const metrics = core.metrics(img.data, decoded, img.width, img.height);
  return { q, bytes, decoded: new Uint8Array(decoded.buffer, decoded.byteOffset, decoded.byteLength), encMs: t1 - t0, decMs: t2 - t1, metrics };
}

async function search(codec, img, target, opts, progress) {
  const evals = [];
  const seen = new Set();
  let q = guess(codec, target);
  for (let it = 0; it < 7; it++) {
    q = Math.max(1, Math.min(100, Math.round(q)));
    if (seen.has(q)) break;
    seen.add(q);
    const r = await runOnce(codec, img, q, opts);
    evals.push(r);
    progress(`q=${q} → ${r.metrics.ssimulacra2.toFixed(1)}`);
    const above = evals.filter((e) => e.metrics.ssimulacra2 >= target).sort((a, b) => a.q - b.q);
    const below = evals.filter((e) => e.metrics.ssimulacra2 < target).sort((a, b) => b.q - a.q);
    const hi = above[0];
    const lo = below[0];
    if (hi && Math.abs(hi.metrics.ssimulacra2 - target) < 0.4) break;
    if (hi && lo) {
      if (hi.q - lo.q <= 1) break;
      const f = (target - lo.metrics.ssimulacra2) / (hi.metrics.ssimulacra2 - lo.metrics.ssimulacra2);
      q = Math.min(hi.q - 1, Math.max(lo.q + 1, lo.q + f * (hi.q - lo.q)));
    } else if (hi) {
      if (hi.q <= 1) break;
      q = hi.q - Math.max(4, (hi.metrics.ssimulacra2 - target) * 2);
    } else {
      if (lo.q >= 100) break;
      q = lo.q + Math.max(4, (target - lo.metrics.ssimulacra2) * 2);
    }
  }
  const above = evals.filter((e) => e.metrics.ssimulacra2 >= target).sort((a, b) => a.q - b.q);
  const below = evals.filter((e) => e.metrics.ssimulacra2 < target).sort((a, b) => b.q - a.q);
  const hi = above[0];
  const lo = below[0];
  let sizeAtTarget = null;
  let note = '';
  if (hi && lo) {
    // Interpolación log-lineal del tamaño en el punto exacto de calidad objetivo.
    const f = (target - lo.metrics.ssimulacra2) / (hi.metrics.ssimulacra2 - lo.metrics.ssimulacra2);
    sizeAtTarget = Math.exp(Math.log(lo.bytes.length) + f * (Math.log(hi.bytes.length) - Math.log(lo.bytes.length)));
  } else if (hi) {
    sizeAtTarget = hi.bytes.length;
    note = 'supera el objetivo incluso con la calidad mínima';
  } else {
    note = 'no alcanza el objetivo ni con calidad 100';
  }
  const best = hi || below.sort((a, b) => b.metrics.ssimulacra2 - a.metrics.ssimulacra2)[0];
  return { ...best, sizeAtTarget, note, evals: evals.map((e) => ({ q: e.q, size: e.bytes.length, s2: e.metrics.ssimulacra2 })) };
}

self.onmessage = async (ev) => {
  const { id, codec, rgba, width, height, mode, target, quality, opts } = ev.data;
  try {
    const img = new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height);
    const progress = (msg) => self.postMessage({ id, progress: msg });
    let r;
    if (mode === 'target' && codec === 'avif') {
      // AVIF admite croma completa (4:4:4): como Tesela elige sola, a AVIF se
      // le prueban las dos y se queda la que ocupe menos a igual calidad.
      const a = await search(codec, img, target, { ...opts, avifSub: 1 }, (m) => progress('4:2:0 ' + m));
      const b = await search(codec, img, target, { ...opts, avifSub: 3 }, (m) => progress('4:4:4 ' + m));
      const sa = a.sizeAtTarget ?? Infinity;
      const sb = b.sizeAtTarget ?? Infinity;
      r = sb < sa ? { ...b, note: (b.note ? b.note + '; ' : '') + 'croma 4:4:4' } : a;
    } else {
      r = mode === 'target' ? await search(codec, img, target, opts, progress) : await runOnce(codec, img, quality, opts);
    }
    const result = {
      q: r.q,
      size: r.bytes.length,
      sizeAtTarget: r.sizeAtTarget ?? null,
      note: r.note || '',
      evals: r.evals || null,
      encMs: r.encMs,
      decMs: r.decMs,
      metrics: r.metrics,
      bytes: r.bytes,
      decoded: r.decoded,
    };
    self.postMessage({ id, result }, [result.bytes.buffer, result.decoded.buffer]);
  } catch (e) {
    self.postMessage({ id, error: String(e && e.message ? e.message : e) });
  }
};
