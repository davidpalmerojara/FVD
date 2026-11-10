// Núcleo de bajo nivel: carga del módulo WebAssembly de Tesela y llamadas a
// su ABI. Se usa tanto en los workers como (si hace falta) en el hilo principal.

const DEFAULT_WASM_URL = new URL('./tesela.wasm', import.meta.url);

let instancePromise = null;

/** Carga (una sola vez) el módulo WebAssembly. */
export function loadWasm(url = DEFAULT_WASM_URL) {
  if (!instancePromise) {
    instancePromise = (async () => {
      const bytes = await (await fetch(url)).arrayBuffer();
      const module = await WebAssembly.compile(bytes);
      // Algunas dependencias declaran imports de wasm-bindgen que nunca se
      // llaman en tiempo de ejecución: se rellenan con funciones que fallan.
      const imports = {};
      for (const imp of WebAssembly.Module.imports(module)) {
        (imports[imp.module] ??= {})[imp.name] = () => {
          throw new Error(`import inesperado: ${imp.module}.${imp.name}`);
        };
      }
      const instance = await WebAssembly.instantiate(module, imports);
      return new Core(instance.exports);
    })();
  }
  return instancePromise;
}

export class Core {
  constructor(exports) {
    this.ex = exports;
  }

  mem() {
    return new Uint8Array(this.ex.memory.buffer);
  }

  /** Copia bytes a la memoria del módulo. Devuelve [puntero, longitud]. */
  put(bytes) {
    const ptr = this.ex.tsl_alloc(bytes.length);
    this.mem().set(bytes, ptr);
    return [ptr, bytes.length];
  }

  free([ptr, len]) {
    this.ex.tsl_free(ptr, len);
  }

  /** Lee y libera un búfer devuelto por el módulo ([len u32][datos]). */
  take(ptr) {
    if (ptr === 0) {
      throw new Error(new TextDecoder().decode(this.take(this.ex.tsl_last_error())) || 'error de Tesela');
    }
    const len = new DataView(this.ex.memory.buffer).getUint32(ptr, true);
    const out = this.mem().slice(ptr + 4, ptr + len);
    this.ex.tsl_free(ptr, len);
    return out;
  }

  /** Decodifica la carga útil de una tesela a RGBA. */
  decodeTile(header, payload, tw, th) {
    const h = this.put(header);
    const p = this.put(payload);
    try {
      return this.take(this.ex.tsl_decode_tile(h[0], h[1], p[0], p[1], tw, th));
    } finally {
      this.free(h);
      this.free(p);
    }
  }

  /** Codifica una tesela RGBA con la cabecera de su imagen. */
  encodeTile(rgba, tw, th, header, quality, effort, tune) {
    const r = this.put(rgba);
    const h = this.put(header);
    try {
      return this.take(this.ex.tsl_encode_tile(r[0], tw, th, h[0], h[1], quality, effort, tune));
    } finally {
      this.free(r);
      this.free(h);
    }
  }

  /** Cabecera para una imagen RGBA completa (decide las herramientas según el contenido). */
  header(rgba, w, h, quality, effort, tune) {
    const r = this.put(rgba);
    try {
      return this.take(this.ex.tsl_header(r[0], w, h, quality, effort, tune));
    } finally {
      this.free(r);
    }
  }

  /** Codifica una imagen RGBA completa (un solo hilo). Devuelve el .tsl. */
  encode(rgba, w, h, quality, effort, tune) {
    const r = this.put(rgba);
    try {
      return this.take(this.ex.tsl_encode(r[0], w, h, quality, effort, tune));
    } finally {
      this.free(r);
    }
  }

  /** Decodifica un .tsl completo a { width, height, rgba }. */
  decode(bytes) {
    const d = this.put(bytes);
    try {
      const out = this.take(this.ex.tsl_decode(d[0], d[1]));
      const dv = new DataView(out.buffer, out.byteOffset, 8);
      return { width: dv.getUint32(0, true), height: dv.getUint32(4, true), rgba: out.subarray(8) };
    } finally {
      this.free(d);
    }
  }

  /** Métricas entre dos RGBA del mismo tamaño. */
  metrics(a, b, w, h) {
    const pa = this.put(a);
    const pb = this.put(b);
    try {
      const out = this.take(this.ex.tsl_metrics(pa[0], pb[0], w, h));
      const f = new Float64Array(out.buffer, out.byteOffset, 4);
      return { psnr: f[0], psnr_y: f[1], ssim_y: f[2], ssimulacra2: f[3] };
    } finally {
      this.free(pa);
      this.free(pb);
    }
  }
}

export const TUNES = { balanced: 0, perceptual: 1, psnr: 2 };

export const TOOL_ALPHA = 4;
export const TOOL_PREVIEW = 8;
const HEADER = 23;

/**
 * Si ya han llegado suficientes bytes, devuelve el rango de la vista previa
 * ({ start, end }), null si el archivo no tiene, o undefined si faltan datos.
 */
export function previewRange(bytes) {
  if (bytes.length < HEADER + 4) return undefined;
  if (!(bytes[22] & TOOL_PREVIEW)) return null;
  const len = new DataView(bytes.buffer, bytes.byteOffset).getUint32(HEADER, true);
  const r = { start: HEADER + 4, end: HEADER + 4 + len };
  return bytes.length >= r.end ? r : undefined;
}

/** Lee la cabecera y la tabla de teselas de un archivo .tsl. */
export function parseTesela(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  if (bytes.length < HEADER || String.fromCharCode(...bytes.subarray(0, 4)) !== 'TSL1') {
    throw new Error('no es un archivo Tesela (.tsl)');
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = dv.getUint32(4, true);
  const height = dv.getUint32(8, true);
  const tile = dv.getUint16(20, true) * 64;
  const tools = bytes[22];
  if (!width || !height || !tile) throw new Error('cabecera Tesela no válida');
  let base = HEADER;
  if (tools & TOOL_PREVIEW) base += 4 + dv.getUint32(HEADER, true);
  const tiles = [];
  for (let y = 0; y < height; y += tile) {
    for (let x = 0; x < width; x += tile) {
      tiles.push({ x, y, w: Math.min(tile, width - x), h: Math.min(tile, height - y) });
    }
  }
  if (bytes.length < base + 4 * tiles.length) throw new Error('archivo Tesela truncado');
  let pos = base + 4 * tiles.length;
  tiles.forEach((t, i) => {
    const len = dv.getUint32(base + 4 * i, true);
    t.start = pos;
    t.end = pos + len;
    pos += len;
  });
  if (pos > bytes.length) throw new Error('archivo Tesela truncado');
  return { width, height, tile, tools, tiles, header: bytes.subarray(0, HEADER), bytes };
}
