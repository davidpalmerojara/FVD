// Tesela para la web.
//
//   import { decode, encode } from './tesela.js';
//   const imageData = await decode(await (await fetch('foto.tsl')).arrayBuffer());
//
//   <script type="module" src="tesela.js"></script>
//   <tesela-img src="foto.tsl" alt="..." fallback="foto.jpg"></tesela-img>
//
// La decodificación se reparte entre varios Web Workers (una tesela por
// trabajo), así que una foto grande se decodifica en paralelo sin bloquear la
// página.

import { parseTesela, TUNES } from './tesela-core.js';

export { parseTesela };

const WASM_URL = new URL('./tesela.wasm', import.meta.url).href;
const WORKER_URL = new URL('./tesela-worker.js', import.meta.url);

class Pool {
  constructor(size) {
    this.size = size;
    this.workers = [];
    this.idle = [];
    this.queue = [];
    this.pending = new Map();
    this.nextId = 1;
  }

  spawn() {
    const w = new Worker(WORKER_URL, { type: 'module' });
    w.onmessage = (ev) => {
      const { id, result, error } = ev.data;
      const p = this.pending.get(id);
      this.pending.delete(id);
      error ? p.reject(new Error(error)) : p.resolve(result);
      this.idle.push(w);
      this.pump();
    };
    w.onerror = (ev) => {
      for (const [id, p] of this.pending) {
        if (p.worker === w) {
          this.pending.delete(id);
          p.reject(new Error(ev.message || 'error en el worker de Tesela'));
        }
      }
    };
    this.workers.push(w);
    this.idle.push(w);
  }

  run(op, args, transfer = []) {
    return new Promise((resolve, reject) => {
      this.queue.push({ op, args: { ...args, wasmUrl: WASM_URL }, transfer, resolve, reject });
      this.pump();
    });
  }

  pump() {
    while (this.queue.length) {
      if (!this.idle.length) {
        if (this.workers.length >= this.size) return;
        this.spawn();
      }
      const w = this.idle.pop();
      const job = this.queue.shift();
      const id = this.nextId++;
      this.pending.set(id, { resolve: job.resolve, reject: job.reject, worker: w });
      w.postMessage({ id, op: job.op, args: job.args }, job.transfer);
    }
  }
}

let pool = null;
function getPool() {
  if (!pool) {
    const n = Math.max(1, Math.min(8, (navigator.hardwareConcurrency || 4) - 1));
    pool = new Pool(n);
  }
  return pool;
}

/** Decodifica un archivo .tsl (ArrayBuffer/Uint8Array) a ImageData. */
export async function decode(buffer) {
  const info = parseTesela(buffer);
  const out = new Uint8ClampedArray(info.width * info.height * 4);
  await Promise.all(
    info.tiles.map(async (t) => {
      const payload = info.bytes.slice(t.start, t.end);
      const rgba = await getPool().run(
        'decodeTile',
        { header: info.header.slice(), payload, w: t.w, h: t.h },
        [payload.buffer],
      );
      for (let y = 0; y < t.h; y++) {
        out.set(rgba.subarray(y * t.w * 4, (y + 1) * t.w * 4), ((t.y + y) * info.width + t.x) * 4);
      }
    }),
  );
  return new ImageData(out, info.width, info.height);
}

/**
 * Codifica un ImageData a .tsl (Uint8Array).
 * Opciones: quality (0-100, 75), effort (1-9, 4), tune ('balanced'|'perceptual'|'psnr').
 */
export async function encode(imageData, { quality = 75, effort = 4, tune = 'balanced' } = {}) {
  const { width: w, height: h, data } = imageData;
  const tn = TUNES[tune] ?? 0;
  const header = await getPool().run('header', { w, h, quality, effort, tune: tn });
  const tile = new DataView(header.buffer, header.byteOffset).getUint16(20, true) * 64;
  const rects = [];
  for (let y = 0; y < h; y += tile) {
    for (let x = 0; x < w; x += tile) rects.push({ x, y, w: Math.min(tile, w - x), h: Math.min(tile, h - y) });
  }
  const payloads = await Promise.all(
    rects.map((r) => {
      const rgba = new Uint8Array(r.w * r.h * 4);
      for (let y = 0; y < r.h; y++) {
        const src = ((r.y + y) * w + r.x) * 4;
        rgba.set(data.subarray(src, src + r.w * 4), y * r.w * 4);
      }
      return getPool().run('encodeTile', { rgba, tw: r.w, th: r.h, w, h, quality, effort, tune: tn }, [rgba.buffer]);
    }),
  );
  const total = header.length + 4 * payloads.length + payloads.reduce((s, p) => s + p.length, 0);
  const out = new Uint8Array(total);
  out.set(header, 0);
  const dv = new DataView(out.buffer);
  let pos = header.length + 4 * payloads.length;
  payloads.forEach((p, i) => {
    dv.setUint32(header.length + 4 * i, p.length, true);
    out.set(p, pos);
    pos += p.length;
  });
  return out;
}

/** Decodifica y devuelve una URL de objeto PNG, utilizable en cualquier <img>. */
export async function toObjectURL(buffer) {
  const img = await decode(buffer);
  const canvas = new OffscreenCanvas(img.width, img.height);
  canvas.getContext('2d').putImageData(img, 0, 0);
  return URL.createObjectURL(await canvas.convertToBlob({ type: 'image/png' }));
}

/** <tesela-img src="foto.tsl" alt="" fallback="foto.jpg" loading="lazy"> */
class TeselaImage extends HTMLElement {
  static get observedAttributes() {
    return ['src'];
  }

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = '<style>:host{display:inline-block;line-height:0}canvas,img{max-width:100%;height:auto;border-radius:inherit}</style>';
    this.canvas = document.createElement('canvas');
    this.canvas.setAttribute('role', 'img');
    root.appendChild(this.canvas);
  }

  connectedCallback() {
    this.canvas.setAttribute('aria-label', this.getAttribute('alt') || '');
    if (this.getAttribute('loading') === 'lazy' && 'IntersectionObserver' in window) {
      const io = new IntersectionObserver((entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          this.load();
        }
      }, { rootMargin: '200px' });
      io.observe(this);
    } else {
      this.load();
    }
  }

  attributeChangedCallback(name, oldV, newV) {
    if (name === 'src' && oldV !== null && oldV !== newV && this.isConnected) this.load();
  }

  async load() {
    const src = this.getAttribute('src');
    if (!src) return;
    try {
      const res = await fetch(src);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const img = await decode(await res.arrayBuffer());
      this.canvas.width = img.width;
      this.canvas.height = img.height;
      this.canvas.getContext('2d').putImageData(img, 0, 0);
      this.dispatchEvent(new Event('load'));
    } catch (e) {
      const fb = this.getAttribute('fallback');
      if (fb) {
        const im = document.createElement('img');
        im.src = fb;
        im.alt = this.getAttribute('alt') || '';
        this.shadowRoot.replaceChild(im, this.canvas);
      }
      this.dispatchEvent(new CustomEvent('error', { detail: e }));
    }
  }
}

if (typeof customElements !== 'undefined' && !customElements.get('tesela-img')) {
  customElements.define('tesela-img', TeselaImage);
}
