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

  /** Todos los fotogramas de una animación: { width, height, loops, frames: [{ rgba, duration }] }. */
  decodeAnimation(bytes) {
    const d = this.put(bytes);
    try {
      const out = this.take(this.ex.tsl_decode_animation(d[0], d[1]));
      const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
      const [width, height, loops, n] = [0, 4, 8, 12].map((o) => dv.getUint32(o, true));
      const size = width * height * 4;
      const frames = [];
      for (let i = 0, p = 16; i < n; i++, p += 4 + size) {
        frames.push({ duration: dv.getUint32(p, true), rgba: out.slice(p + 4, p + 4 + size) });
      }
      return { width, height, loops, frames };
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

/** Versiones del flujo de bits que entiende este decodificador. */
export const MIN_VERSION = 1;
export const VERSION = 2;
export const TOOL_ALPHA = 4;
export const TOOL_PREVIEW = 8;
export const TOOL_META = 32;
export const TOOL_ANIM = 64;
const HEADER = 23;

/**
 * Sección de metadatos (v2): `u32` de longitud y fragmentos
 * `etiqueta (4 B) | u32 | datos`. Devuelve { icc, exif, end } o undefined si
 * aún no han llegado sus bytes.
 */
export function readMetadata(bytes, pos = HEADER) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.length < pos + 4) return undefined;
  const end = pos + 4 + dv.getUint32(pos, true);
  if (bytes.length < end) return undefined;
  const out = { icc: null, exif: null, end };
  for (let p = pos + 4; p + 8 <= end; ) {
    const tag = String.fromCharCode(...bytes.subarray(p, p + 4));
    const n = dv.getUint32(p + 4, true);
    if (p + 8 + n > end) break;
    const data = bytes.subarray(p + 8, p + 8 + n);
    if (tag === 'ICCP') out.icc = data;
    else if (tag === 'EXIF') out.exif = data;
    p += 8 + n;
  }
  return out;
}

/**
 * Si ya han llegado suficientes bytes, devuelve el rango de la vista previa
 * ({ start, end }), null si el archivo no tiene, o undefined si faltan datos.
 */
export function previewRange(bytes) {
  if (bytes.length < HEADER + 4) return undefined;
  if (!(bytes[22] & TOOL_PREVIEW)) return null;
  let base = HEADER;
  if (bytes[22] & TOOL_META) {
    const m = readMetadata(bytes);
    if (!m) return undefined;
    base = m.end;
  }
  if (bytes.length < base + 4) return undefined;
  const len = new DataView(bytes.buffer, bytes.byteOffset).getUint32(base, true);
  const r = { start: base + 4, end: base + 4 + len };
  return bytes.length >= r.end ? r : undefined;
}

/** Lee la cabecera y la tabla de teselas de un archivo .tsl. */
export function parseTesela(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  if (bytes.length < 4 || String.fromCharCode(...bytes.subarray(0, 3)) !== 'TSL') {
    throw new Error('no es un archivo Tesela (.tsl)');
  }
  if (bytes[3] < MIN_VERSION || bytes[3] > VERSION) {
    throw new Error(bytes[3] === 0x31
      ? 'archivo de una versión de desarrollo anterior a la v1: vuelve a codificarlo'
      : `versión ${bytes[3]} del formato Tesela no soportada (se admiten de la v${MIN_VERSION} a la v${VERSION})`);
  }
  if (bytes.length < HEADER) throw new Error('archivo Tesela truncado');
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = dv.getUint32(4, true);
  const height = dv.getUint32(8, true);
  const tile = dv.getUint16(20, true) * 64;
  const tools = bytes[22];
  if (!width || !height || !tile || width > 65536 || height > 65536) throw new Error('cabecera Tesela no válida');
  // Mismo límite que el decodificador (MAX_DECODE_PIXELS): un archivo de pocos
  // bytes no puede obligar a reservar gigas ni a recorrer millones de teselas.
  if (width * height > 2 ** 28) throw new Error(`imagen demasiado grande para decodificar (${width}×${height})`);
  // Animación: no hay tabla de teselas (se decodifica entera con decodeAnimation).
  if (tools & TOOL_ANIM) return { width, height, tile, tools, tiles: [], header: bytes.subarray(0, HEADER), bytes, animated: true };
  let base = HEADER;
  let meta = { icc: null, exif: null };
  if (tools & TOOL_META) {
    const m = readMetadata(bytes);
    if (!m) throw new Error('archivo Tesela truncado');
    meta = m;
    base = m.end;
  }
  if (tools & TOOL_PREVIEW) {
    if (bytes.length < base + 4) throw new Error('archivo Tesela truncado');
    base += 4 + dv.getUint32(base, true);
  }
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
  return { width, height, tile, tools, tiles, header: bytes.subarray(0, HEADER), bytes, icc: meta.icc, exif: meta.exif };
}
