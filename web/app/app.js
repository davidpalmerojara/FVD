// Convertir y abrir: convierte imágenes a .tsl y abre archivos .tsl, todo en
// el navegador. Codifica con el mismo camino que el comparador (loadEncoder
// en un worker) y decodifica con la librería de la web (tesela.js).

import { decode, decodeAnimation, parseTesela } from '../tesela.js';
import { readMetadata, TOOL_META } from '../tesela-core.js';
import { makeZip } from './zip.js';
import { encodePng } from './png.js';

const $ = (id) => document.getElementById(id);
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');

// ------------------------------------------------------------ Formato

const fmt1 = new Intl.NumberFormat('es-ES', { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const fmt0 = new Intl.NumberFormat('es-ES', { maximumFractionDigits: 0 });
function bytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${n < 100 * 1024 ? fmt1.format(n / 1024) : fmt0.format(n / 1024)} KB`;
  return `${fmt1.format(n / 1048576)} MB`;
}
const secs = (ms) => `${fmt1.format(ms / 1000)} s`;
/** «−62 %» si ocupa menos, «+8 %» si ocupa más. */
function delta(after, before) {
  const v = Math.round((after / before - 1) * 100);
  return v <= 0 ? `−${-v} %` : `+${v} %`;
}
const baseName = (name) => name.replace(/\.[^./\\]+$/, '') || 'imagen';

function announce(msg) {
  const el = $('live');
  el.textContent = '';
  setTimeout(() => (el.textContent = msg), 50);
}
let toastTimer = 0;
function toast(msg) {
  const el = $('alert');
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 8000);
}

function download(blob, name) {
  const url = URL.createObjectURL(blob);
  const a = Object.assign(document.createElement('a'), { href: url, download: name });
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
}

function makeCanvas(w, h) {
  if (typeof OffscreenCanvas !== 'undefined') return new OffscreenCanvas(w, h);
  return Object.assign(document.createElement('canvas'), { width: w, height: h });
}
async function canvasBlob(c, type, quality) {
  if (c.convertToBlob) return c.convertToBlob({ type, quality });
  return new Promise((r) => c.toBlob(r, type, quality));
}

// ------------------------------------------------------------ Pestañas

const tabs = { convert: $('tab-convert'), open: $('tab-open') };
function selectTab(name, focus = false) {
  for (const [k, t] of Object.entries(tabs)) {
    const on = k === name;
    t.setAttribute('aria-selected', String(on));
    t.tabIndex = on ? 0 : -1;
    $(t.getAttribute('aria-controls')).hidden = !on;
  }
  if (focus) tabs[name].focus();
  if (name !== 'open') stopPlayback();
}
for (const [k, t] of Object.entries(tabs)) {
  t.addEventListener('click', () => selectTab(k));
  t.addEventListener('keydown', (e) => {
    const keys = Object.keys(tabs);
    const i = keys.indexOf(k);
    const to = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: keys.length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    selectTab(keys[(to + keys.length) % keys.length], true);
  });
}

// ------------------------------------------------------------ Ajustes

const HINTS = [
  [90, 'Casi idéntica al original. Archivos más grandes.'],
  [75, 'Alta: las diferencias apenas se ven. Recomendada para fotos.'],
  [55, 'Media: buena para la web y miniaturas; en detalles finos se nota algo.'],
  [0, 'Baja: archivos muy pequeños, con pérdida visible.'],
];
const settings = () => ({
  quality: Number($('quality').value),
  effort: Number($('effort').value),
  maxdim: Number($('maxdim').value),
});
const sameSettings = (a, b) => a && b && a.quality === b.quality && a.effort === b.effort && a.maxdim === b.maxdim;
function updateSettingsUi() {
  const q = Number($('quality').value);
  $('quality-val').textContent = q;
  $('quality-hint').textContent = HINTS.find(([min]) => q >= min)[1];
  const st = settings();
  const stale = items.some((it) => it.settings && !sameSettings(it.settings, st));
  $('reconvert').hidden = !stale;
  $('reconvert').textContent = `Volver a convertir con calidad ${q}`;
}
for (const id of ['quality', 'effort', 'maxdim']) $(id).addEventListener('input', updateSettingsUi);

// ------------------------------------------------------------ Codificación

// Con aislamiento entre orígenes (COOP/COEP), un solo worker codifica cada
// imagen con varios hilos (tesela-mt.wasm). Sin él, varios workers de un hilo.
const ISOLATED = self.crossOriginIsolated === true && typeof SharedArrayBuffer !== 'undefined';
const CORES = navigator.hardwareConcurrency || 4;
const PARALLEL = ISOLATED ? 1 : Math.max(1, Math.min(4, CORES - 1));
// Memoria del codificador en WebAssembly (~40 B por píxel, medida en el
// comparador) y la de la página: se limita lo que corre a la vez y el tamaño
// de cada imagen para no agotar la RAM.
const ENC_BYTES_PER_PX = 48;
const MEMORY_BUDGET = Math.min(navigator.deviceMemory || 4, 8) * 0.3 * 2 ** 30;
const MAX_PIXELS = 24e6;
const RECYCLE_BYTES = 256 * 2 ** 20;

let engineThreads = ISOLATED ? Math.max(2, Math.min(8, CORES)) : 1;
function updateEngine() {
  $('engine').textContent = engineThreads > 1
    ? `Se codifica en tu dispositivo con ${engineThreads} hilos.`
    : 'Se codifica en tu dispositivo con un hilo por imagen (este navegador no permite más aquí).';
}
updateEngine();

const idleWorkers = [];
function encodeOnWorker(item, msg) {
  return new Promise((resolve, reject) => {
    const w = idleWorkers.pop() || new Worker(new URL('./encode-worker.js', import.meta.url), { type: 'module' });
    const finish = (recycle) => {
      w.onmessage = w.onerror = null;
      item.cancel = null;
      if (recycle) w.terminate();
      else idleWorkers.push(w);
    };
    w.onmessage = (ev) => {
      const d = ev.data;
      if (d.error) {
        finish(true); // tras un fallo, la memoria del módulo puede quedar inservible
        reject(new Error(d.error));
      } else {
        finish(msg.width * msg.height * ENC_BYTES_PER_PX > RECYCLE_BYTES);
        resolve(d);
      }
    };
    w.onerror = (ev) => {
      ev.preventDefault();
      finish(true);
      reject(new Error(ev.message || 'el codificador se detuvo'));
    };
    item.cancel = () => {
      finish(true);
      reject(new Error('cancelado'));
    };
    w.postMessage({ id: item.id, ...msg }, [msg.rgba.buffer]);
  });
}

/** RGBA de una imagen (orientación EXIF aplicada), reducida si hace falta. */
async function readPixels(file, maxdim) {
  const bmp = await createImageBitmap(file, { imageOrientation: 'from-image', premultiplyAlpha: 'none' });
  let s = 1;
  if (maxdim && Math.max(bmp.width, bmp.height) > maxdim) s = maxdim / Math.max(bmp.width, bmp.height);
  let capped = false;
  if (bmp.width * bmp.height * s * s > MAX_PIXELS) {
    s = Math.sqrt(MAX_PIXELS / (bmp.width * bmp.height));
    capped = true;
  }
  const w = Math.max(1, Math.round(bmp.width * s));
  const h = Math.max(1, Math.round(bmp.height * s));
  const c = makeCanvas(w, h);
  const ctx = c.getContext('2d', { willReadFrequently: true });
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(bmp, 0, 0, w, h);
  bmp.close?.();
  let rgba = new Uint8Array(ctx.getImageData(0, 0, w, h).data.buffer);
  if (s === 1 && hasAlpha(rgba)) rgba = (await straightAlpha(file, w, h, rgba)) || rgba;
  return { rgba, w, h, capped };
}

function hasAlpha(rgba) {
  for (let i = 3; i < rgba.length; i += 4) if (rgba[i] !== 255) return true;
  return false;
}

/**
 * Con transparencia, <canvas> guarda el color premultiplicado por el alfa:
 * pierde precisión donde el alfa es bajo y pone a cero el color de los
 * píxeles transparentes, y esos bordes artificiales encarecen el .tsl. Donde
 * se puede (ImageDecoder, en Chrome y Edge), se leen los píxeles sin
 * premultiplicar. Solo se usan si el alfa coincide con el del lienzo (misma
 * orientación y tamaño); si no, null.
 */
async function straightAlpha(file, w, h, ref) {
  if (typeof ImageDecoder === 'undefined' || !file.type) return null;
  let dec;
  let frame;
  try {
    if (!(await ImageDecoder.isTypeSupported(file.type))) return null;
    dec = new ImageDecoder({ data: file.stream(), type: file.type, premultiplyAlpha: 'none' });
    frame = (await dec.decode({ frameIndex: 0 })).image;
    const bgra = frame.format === 'BGRA';
    if ((!bgra && frame.format !== 'RGBA') || frame.displayWidth !== w || frame.displayHeight !== h) return null;
    const buf = new Uint8Array(frame.allocationSize());
    const [plane] = await frame.copyTo(buf);
    const out = new Uint8Array(w * h * 4);
    for (let y = 0; y < h; y++) {
      const src = plane.offset + y * plane.stride;
      for (let x = 0; x < w; x++) {
        const i = src + x * 4;
        const o = (y * w + x) * 4;
        if (buf[i + 3] !== ref[o + 3]) return null;
        out[o] = buf[i + (bgra ? 2 : 0)];
        out[o + 1] = buf[i + 1];
        out[o + 2] = buf[i + (bgra ? 0 : 2)];
        out[o + 3] = buf[i + 3];
      }
    }
    return out;
  } catch {
    return null;
  } finally {
    frame?.close();
    dec?.close();
  }
}

/** Número de fotogramas de un GIF (se para en 2: solo interesa si es animado). */
function gifFrames(b) {
  if (b.length < 13 || b[0] !== 0x47 || b[1] !== 0x49 || b[2] !== 0x46) return 1;
  let p = 13;
  if (b[10] & 0x80) p += 3 * (2 << (b[10] & 7));
  let n = 0;
  const skipBlocks = () => {
    while (p < b.length && b[p]) p += b[p] + 1;
    p++;
  };
  while (p < b.length) {
    const t = b[p++];
    if (t === 0x2c) {
      if (++n > 1) return n;
      p += 8;
      const f = b[p++];
      if (f & 0x80) p += 3 * (2 << (f & 7));
      p++; // tamaño mínimo del código LZW
      skipBlocks();
    } else if (t === 0x21) {
      p++;
      skipBlocks();
    } else break;
  }
  return Math.max(1, n);
}

/** ¿Tiene la imagen varios fotogramas (GIF, APNG o WebP animados)? */
async function isAnimated(file) {
  if (/gif$/i.test(file.type) || /\.gif$/i.test(file.name)) {
    return gifFrames(new Uint8Array(await file.arrayBuffer())) > 1;
  }
  if (!/png|webp|avif/i.test(file.type) || typeof ImageDecoder === 'undefined') return false;
  try {
    if (!(await ImageDecoder.isTypeSupported(file.type))) return false;
    const dec = new ImageDecoder({ data: file.stream(), type: file.type });
    await dec.tracks.ready;
    const n = dec.tracks.selectedTrack?.frameCount ?? 1;
    dec.close();
    return n > 1;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------ Lista de conversión

const items = [];
let nextId = 1;
let queue = [];
let running = new Set();
let usedBytes = 0;
let batch = { total: 0, announced: true };

const IMAGE_EXT = /\.(jpe?g|png|webp|avif|gif|bmp|ico|heic|heif|jxl|tiff?)$/i;

async function addImages(files) {
  const fresh = [];
  for (const file of files) {
    const it = { id: nextId++, file, name: file.name || 'imagen', origBytes: file.size, status: 'queued' };
    items.push(it);
    fresh.push(it);
    renderItem(it);
  }
  $('convert-empty').hidden = true;
  batch = { total: (batch.announced ? 0 : batch.total) + fresh.length, announced: false };
  announce(fresh.length === 1 ? `Convirtiendo ${fresh[0].name}…` : `Convirtiendo ${fresh.length} imágenes…`);
  // Miniatura y dimensiones (y si es animada) antes de codificar.
  for (const it of fresh) {
    try {
      const bmp = await createImageBitmap(it.file, { imageOrientation: 'from-image' });
      it.srcW = bmp.width;
      it.srcH = bmp.height;
      const t = 128;
      const s = Math.max(t / bmp.width, t / bmp.height);
      const c = makeCanvas(t, t);
      c.getContext('2d').drawImage(bmp, (t - bmp.width * s) / 2, (t - bmp.height * s) / 2, bmp.width * s, bmp.height * s);
      bmp.close?.();
      it.thumb = URL.createObjectURL(await canvasBlob(c, 'image/png'));
      if (await isAnimated(it.file)) it.animated = true;
    } catch {
      it.status = 'error';
      it.error = IMAGE_EXT.test(it.name) || it.file.type.startsWith('image/')
        ? 'Tu navegador no puede leer esta imagen (formato no compatible o archivo dañado).'
        : 'No es una imagen. Formatos admitidos: JPG, PNG, WebP, AVIF y GIF.';
    }
    if (items.includes(it)) {
      renderItem(it);
      if (it.status === 'queued') queue.push(it);
    }
    pump();
  }
  renderSummary();
}

function itemBytes(it) {
  const st = it.pending || settings();
  let px = (it.srcW || 1000) * (it.srcH || 1000);
  if (st.maxdim) px = Math.min(px, st.maxdim * st.maxdim);
  return Math.min(px, MAX_PIXELS) * ENC_BYTES_PER_PX;
}

function pump() {
  while (queue.length && running.size < PARALLEL) {
    const it = queue[0];
    const need = itemBytes(it);
    if (running.size > 0 && usedBytes + need > MEMORY_BUDGET) return;
    queue.shift();
    running.add(it);
    usedBytes += need;
    convert(it).finally(() => {
      running.delete(it);
      usedBytes -= need;
      pump();
      renderSummary();
    });
  }
  tick();
}

async function convert(it) {
  const st = settings();
  const run = (it.run = (it.run || 0) + 1);
  it.status = 'encoding';
  it.error = null;
  it.startedAt = performance.now();
  renderItem(it);
  try {
    const px = await readPixels(it.file, st.maxdim);
    if (it.run !== run || !items.includes(it)) return;
    it.w = px.w;
    it.h = px.h;
    it.capped = px.capped;
    const res = await encodeOnWorker(it, { rgba: px.rgba, width: px.w, height: px.h, quality: st.quality, effort: st.effort });
    if (it.run !== run) return;
    it.tsl = res.bytes;
    it.ms = performance.now() - it.startedAt;
    it.settings = st;
    it.status = 'done';
    if (res.threads !== engineThreads) {
      engineThreads = res.threads;
      updateEngine();
    }
    announce(`${it.name} convertida: ${bytes(it.origBytes)} a ${bytes(it.tsl.length)}, ${delta(it.tsl.length, it.origBytes).replace('−', 'menos ').replace('+', 'más ')}.`);
  } catch (e) {
    if (it.run !== run || e.message === 'cancelado') return;
    it.status = 'error';
    it.error = /memory|memoria|allocation|out of bounds|unreachable/i.test(e.message)
      ? 'No hay memoria suficiente para esta imagen. Prueba con un tamaño máximo menor en «Más opciones».'
      : `No se pudo convertir: ${e.message}`;
    announce(`${it.name}: error al convertir.`);
  }
  renderItem(it);
  updateSettingsUi();
}

function cancel(it) {
  it.run = (it.run || 0) + 1;
  queue = queue.filter((x) => x !== it);
  it.cancel?.();
}

function removeItem(it) {
  cancel(it);
  const i = items.indexOf(it);
  items.splice(i, 1);
  const li = it.li;
  const next = li?.nextElementSibling || li?.previousElementSibling;
  li?.remove();
  if (it.thumb) URL.revokeObjectURL(it.thumb);
  (next?.querySelector('[data-act="remove"]') || $('pick-images')).focus();
  if (!items.length) $('convert-empty').hidden = false;
  announce(`${it.name} quitada de la lista.`);
  renderSummary();
  updateSettingsUi();
}

function clearAll() {
  for (const it of [...items]) {
    cancel(it);
    if (it.thumb) URL.revokeObjectURL(it.thumb);
  }
  items.length = 0;
  queue = [];
  $('files').innerHTML = '';
  $('convert-empty').hidden = false;
  batch = { total: 0, announced: true };
  renderSummary();
  updateSettingsUi();
  $('pick-images').focus();
  announce('Lista vaciada.');
}

function reconvertAll() {
  const redo = items.filter((it) => it.status !== 'error' || it.srcW);
  for (const it of redo) {
    cancel(it);
    it.status = 'queued';
    it.tsl = null;
    renderItem(it);
  }
  queue = [...redo];
  batch = { total: redo.length, announced: false };
  announce(`Convirtiendo de nuevo ${redo.length === 1 ? 'la imagen' : `${redo.length} imágenes`} con calidad ${settings().quality}…`);
  pump();
  renderSummary();
  updateSettingsUi();
}

function tslName(it) {
  return `${baseName(it.name)}.tsl`;
}

function renderItem(it) {
  if (!it.li) {
    const li = document.createElement('li');
    li.className = 'file';
    li.innerHTML = `
      <img class="thumb" alt="" width="64" height="64">
      <div class="finfo">
        <p class="fname"></p>
        <p class="fmeta num"></p>
        <p class="fnote" hidden></p>
        <div class="fprog" hidden><i></i></div>
      </div>
      <div class="factions">
        <button class="btn primary small" type="button" data-act="dl" hidden>Descargar</button>
        <button class="btn small" type="button" data-act="view" hidden>Ver</button>
        <button class="icon" type="button" data-act="remove">×</button>
      </div>`;
    li.querySelector('.fname').textContent = it.name;
    li.querySelector('.fname').title = it.name;
    li.querySelector('[data-act="dl"]').setAttribute('aria-label', `Descargar ${tslName(it)}`);
    li.querySelector('[data-act="view"]').setAttribute('aria-label', `Ver ${tslName(it)}`);
    li.querySelector('[data-act="remove"]').setAttribute('aria-label', `Quitar ${it.name}`);
    li.querySelector('[data-act="remove"]').title = 'Quitar';
    li.querySelector('[data-act="dl"]').onclick = () => download(new Blob([it.tsl], { type: 'image/x-tesela' }), tslName(it));
    li.querySelector('[data-act="view"]').onclick = () => {
      openEntries([{ name: tslName(it), bytes: it.tsl.slice() }]);
      selectTab('open', true);
    };
    li.querySelector('[data-act="remove"]').onclick = () => removeItem(it);
    $('files').appendChild(li);
    it.li = li;
  }
  const li = it.li;
  li.classList.toggle('err', it.status === 'error');
  const img = li.querySelector('.thumb');
  if (it.thumb && img.getAttribute('src') !== it.thumb) img.src = it.thumb;
  const meta = li.querySelector('.fmeta');
  const note = li.querySelector('.fnote');
  const prog = li.querySelector('.fprog');
  li.querySelector('[data-act="dl"]').hidden = it.status !== 'done';
  li.querySelector('[data-act="view"]').hidden = it.status !== 'done';
  prog.hidden = !(it.status === 'queued' || it.status === 'encoding');
  prog.classList.toggle('wait', it.status === 'queued');
  meta.className = 'fmeta num';
  const notes = [];
  if (it.status === 'queued') meta.textContent = `En cola · ${bytes(it.origBytes)}`;
  else if (it.status === 'encoding') meta.textContent = `Convirtiendo… ${secs(performance.now() - it.startedAt)}`;
  else if (it.status === 'error') {
    meta.className = 'ferr';
    meta.textContent = it.error;
  } else if (it.status === 'done') {
    const cls = it.tsl.length <= it.origBytes ? 'saved' : 'bigger';
    meta.innerHTML = `<b>${bytes(it.origBytes)} → ${bytes(it.tsl.length)}</b> · <span class="${cls}">${delta(it.tsl.length, it.origBytes)}</span> · ${it.w}×${it.h} · ${secs(it.ms)}`;
    if (it.tsl.length > it.origBytes) notes.push('Ocupa más que el original: prueba con menos calidad.');
    if (it.capped) notes.push(`Reducida a ${it.w}×${it.h} para no agotar la memoria del navegador.`);
    else if (it.srcW && (it.w !== it.srcW || it.h !== it.srcH)) notes.push(`Reducida desde ${it.srcW}×${it.srcH}.`);
  }
  if (it.animated && it.status !== 'error') {
    notes.push('Imagen animada: se convierte el primer fotograma. Para animaciones, usa la línea de comandos (tesela encode).');
  }
  note.textContent = notes.join(' ');
  note.hidden = !notes.length;
}

let ticker = 0;
function tick() {
  if (ticker) return;
  ticker = setInterval(() => {
    const active = items.filter((it) => it.status === 'encoding');
    active.forEach(renderItem);
    if (!active.length && !queue.length) {
      clearInterval(ticker);
      ticker = 0;
    }
  }, 500);
}

function renderSummary() {
  const done = items.filter((it) => it.status === 'done');
  const pending = items.filter((it) => it.status === 'queued' || it.status === 'encoding');
  const errors = items.filter((it) => it.status === 'error');
  $('summary').hidden = items.length === 0;
  $('zip').disabled = done.length === 0;
  $('zip').textContent = done.length > 1 ? `Descargar todo (.zip, ${done.length})` : 'Descargar todo (.zip)';
  const before = done.reduce((s, it) => s + it.origBytes, 0);
  const after = done.reduce((s, it) => s + it.tsl.length, 0);
  const n = items.length;
  if (pending.length) {
    $('sum-main').textContent = `Convertidas ${done.length} de ${done.length + pending.length}…`;
  } else if (done.length) {
    const saved = before - after;
    $('sum-main').innerHTML = saved >= 0
      ? `Ahorro total: <b>${bytes(saved)}</b> (${delta(after, before)})`
      : `Los .tsl ocupan ${bytes(-saved)} más (${delta(after, before)})`;
  } else {
    $('sum-main').textContent = errors.length ? 'No se pudo convertir ninguna imagen' : '';
  }
  $('sum-sub').textContent = done.length
    ? `${done.length} ${done.length === 1 ? 'imagen' : 'imágenes'}: ${bytes(before)} en el original → ${bytes(after)} en .tsl${errors.length ? ` · ${errors.length} con error` : ''}`
    : errors.length ? `${errors.length} con error` : '';
  const total = done.length + pending.length;
  $('sum-bar').style.width = total ? `${(done.length / total) * 100}%` : '0';
  if (!pending.length && !batch.announced && n) {
    batch.announced = true;
    if (done.length) {
      const saved = before - after;
      announce(`Listo: ${done.length} ${done.length === 1 ? 'imagen convertida' : 'imágenes convertidas'}. ` +
        (saved >= 0 ? `Ahorro total: ${bytes(saved)}, un ${Math.round((saved / before) * 100)} %.` : `Ocupan ${bytes(-saved)} más que los originales.`));
    }
  }
}

$('zip').addEventListener('click', () => {
  const done = items.filter((it) => it.status === 'done');
  const used = new Map();
  const files = done.map((it) => {
    let name = tslName(it);
    const key = name.toLowerCase();
    const k = (used.get(key) || 0) + 1;
    used.set(key, k);
    if (k > 1) name = `${baseName(name)} (${k}).tsl`;
    return { name, bytes: it.tsl };
  });
  download(makeZip(files), done.length === 1 ? `${baseName(done[0].name)}.zip` : 'imagenes-tesela.zip');
  announce(`Descargando ${files.length} ${files.length === 1 ? 'archivo' : 'archivos'} .tsl en un .zip.`);
});
$('clear').addEventListener('click', clearAll);
$('reconvert').addEventListener('click', reconvertAll);
$('pick-images').addEventListener('click', () => $('file-images').click());
$('file-images').addEventListener('change', (e) => {
  route([...e.target.files], 'convert');
  e.target.value = '';
});

// ------------------------------------------------------------ Abrir .tsl

const opened = [];
let current = null;
const view = { zoom: 'fit', frames: null, image: null, i: 0, playing: false, timer: 0, round: 0 };

async function openEntries(entries) {
  let last = null;
  for (const e of entries) {
    const entry = { id: nextId++, name: e.name, size: e.bytes.length, bytes: e.bytes };
    try {
      entry.info = parseTesela(entry.bytes);
    } catch (err) {
      entry.error = `No se puede abrir: ${err.message}.`;
    }
    opened.push(entry);
    renderOpened();
    if (entry.error) {
      toast(`«${entry.name}»: ${entry.error}`);
      continue;
    }
    last = entry;
  }
  if (last) await show(last);
  else if (!current && opened.length) await show(opened[opened.length - 1]);
}

function renderOpened() {
  const ul = $('opened');
  ul.innerHTML = '';
  for (const e of opened) {
    const li = document.createElement('li');
    li.innerHTML = '<button type="button"><span class="on"></span><span class="os num"></span></button>';
    if (e.error) li.className = 'err';
    li.querySelector('.on').textContent = e.name;
    li.querySelector('.os').textContent = e.error ? 'error' : bytes(e.size);
    const b = li.querySelector('button');
    b.setAttribute('aria-current', String(e === current));
    b.onclick = () => show(e);
    ul.appendChild(li);
  }
  $('open-empty').hidden = opened.length > 0;
}

function stopPlayback() {
  clearTimeout(view.timer);
  view.timer = 0;
  if (view.playing) {
    view.playing = false;
    updateAnimUi();
  }
}

async function show(e) {
  stopPlayback();
  current = e;
  renderOpened();
  $('viewer').hidden = false;
  $('open-empty').hidden = true;
  $('v-name').textContent = e.name;
  const canvas = $('canvas');
  if (e.error) {
    $('v-facts').innerHTML = '';
    $('anim').hidden = true;
    $('save-png').hidden = true;
    canvas.width = canvas.height = 0;
    $('stage').hidden = true;
    const p = document.createElement('div');
    p.innerHTML = `<dt>Error</dt><dd class="ferr"></dd>`;
    p.querySelector('dd').textContent = e.error;
    $('v-facts').appendChild(p);
    return;
  }
  $('stage').hidden = false;
  $('save-png').hidden = false;
  $('v-facts').innerHTML = '<div><dt>Estado</dt><dd>decodificando…</dd></div>';
  view.frames = null;
  view.image = null;
  try {
    const t0 = performance.now();
    if (e.info.animated) {
      const a = await decodeAnimation(e.bytes);
      if (current !== e) return;
      view.frames = a.frames;
      view.loops = a.loops;
    } else {
      view.image = await decode(e.bytes);
      if (current !== e) return;
    }
    e.decodeMs = performance.now() - t0;
  } catch (err) {
    if (current !== e) return;
    e.error = `El archivo está dañado o incompleto (${err.message}).`;
    toast(`«${e.name}»: ${e.error}`);
    announce(`No se pudo abrir ${e.name}.`);
    return show(e);
  }
  const first = view.frames ? view.frames[0].image : view.image;
  canvas.width = first.width;
  canvas.height = first.height;
  canvas.setAttribute('aria-label', `${e.name}, ${first.width} × ${first.height} píxeles`);
  view.i = 0;
  view.round = 0;
  drawFrame();
  applyZoom();
  renderFacts(e, first);
  const n = view.frames ? view.frames.length : 1;
  $('anim').hidden = n < 2;
  if (n > 1 && !reduceMotion.matches) play();
  else updateAnimUi();
  announce(`${e.name} abierto: ${first.width} por ${first.height} píxeles${n > 1 ? `, ${n} fotogramas` : ''}.`);
  // En pantallas estrechas el visor queda debajo de la lista: se lleva a la vista.
  if (matchMedia('(max-width: 900px)').matches) {
    $('viewer').scrollIntoView({ behavior: reduceMotion.matches ? 'auto' : 'smooth', block: 'start' });
  }
}

function renderFacts(e, img) {
  const n = view.frames ? view.frames.length : 1;
  let alpha = false;
  const d = img.data;
  for (let i = 3; i < d.length; i += 4) {
    if (d[i] !== 255) {
      alpha = true;
      break;
    }
  }
  const facts = [
    ['Dimensiones', `${img.width} × ${img.height} px`],
    ['Tamaño', `${bytes(e.size)} · ${fmt1.format((e.size * 8) / (img.width * img.height)).replace(/,0$/, '')} bits/píxel`],
    ['Fotogramas', n > 1
      ? `${n} · ${secs(view.frames.reduce((s, f) => s + f.duration, 0))} · ${view.loops ? `${view.loops} ${view.loops === 1 ? 'vez' : 'veces'}` : 'en bucle'}`
      : '1'],
    ['Transparencia', alpha ? 'sí' : 'no'],
    ['Decodificado en', secs(e.decodeMs)],
  ];
  const dl = $('v-facts');
  dl.innerHTML = '';
  for (const [k, v] of facts) {
    const div = document.createElement('div');
    div.innerHTML = '<dt></dt><dd></dd>';
    div.firstChild.textContent = k;
    div.lastChild.textContent = v;
    dl.appendChild(div);
  }
}

function frameImage() {
  return view.frames ? view.frames[view.i].image : view.image;
}

function drawFrame() {
  const img = frameImage();
  if (img) $('canvas').getContext('2d').putImageData(img, 0, 0);
  updateAnimUi();
}

function updateAnimUi() {
  if (!view.frames || view.frames.length < 2) return;
  $('frame').textContent = `Fotograma ${view.i + 1} de ${view.frames.length}`;
  $('play').textContent = view.playing ? 'Pausa' : 'Reproducir';
  $('play').setAttribute('aria-pressed', String(view.playing));
}

function play() {
  if (!view.frames || view.frames.length < 2) return;
  view.playing = true;
  const step = () => {
    if (!view.playing) return;
    const delay = Math.max(20, view.frames[view.i].duration);
    view.timer = setTimeout(() => {
      view.i++;
      if (view.i === view.frames.length) {
        view.i = 0;
        view.round++;
        if (view.loops && view.round >= view.loops) {
          view.i = view.frames.length - 1;
          view.playing = false;
          drawFrame();
          return;
        }
      }
      drawFrame();
      step();
    }, delay);
  };
  updateAnimUi();
  step();
}

function stepFrame(d) {
  stopPlayback();
  const n = view.frames.length;
  view.i = (view.i + d + n) % n;
  drawFrame();
}
$('play').addEventListener('click', () => {
  if (view.playing) stopPlayback();
  else {
    view.round = 0;
    play();
  }
});
$('prev').addEventListener('click', () => stepFrame(-1));
$('next').addEventListener('click', () => stepFrame(1));

function applyZoom() {
  const fit = view.zoom === 'fit';
  $('stage').classList.toggle('fit', fit);
  const c = $('canvas');
  c.style.width = fit ? '' : `${c.width}px`;
  c.style.height = fit ? '' : `${c.height}px`;
  for (const b of document.querySelectorAll('[data-zoom]')) b.setAttribute('aria-pressed', String(b.dataset.zoom === view.zoom));
}
for (const b of document.querySelectorAll('[data-zoom]')) {
  b.addEventListener('click', () => {
    view.zoom = b.dataset.zoom;
    applyZoom();
  });
}

$('save-png').addEventListener('click', async () => {
  const e = current;
  const img = frameImage();
  if (!e || !img) return;
  const b = e.bytes;
  const icc = b[3] >= 2 && b[22] & TOOL_META ? readMetadata(b)?.icc : null;
  const png = await encodePng(img.width, img.height, new Uint8Array(img.data.buffer, img.data.byteOffset, img.data.byteLength), { icc });
  const suffix = view.frames && view.frames.length > 1 ? `-fotograma-${view.i + 1}` : '';
  download(png, `${baseName(e.name)}${suffix}.png`);
  announce(`Guardado ${baseName(e.name)}${suffix}.png.`);
});

$('pick-tsl').addEventListener('click', () => $('file-tsl').click());
$('file-tsl').addEventListener('change', (e) => {
  route([...e.target.files], 'open');
  e.target.value = '';
});
for (const b of document.querySelectorAll('[data-sample]')) {
  b.addEventListener('click', async () => {
    try {
      const res = await fetch(b.dataset.sample);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      await openEntries([{ name: b.dataset.sample.split('/').pop(), bytes: new Uint8Array(await res.arrayBuffer()) }]);
    } catch (err) {
      toast(`No se pudo cargar el ejemplo (${err.message}).`);
    }
  });
}

// ------------------------------------------------------------ Entrada de archivos

/** ¿Es un .tsl? Por la extensión o, si no la tiene, por la firma «TSL». */
async function isTsl(file) {
  if (/\.tsl$/i.test(file.name) || file.type === 'image/x-tesela') return true;
  if (file.type.startsWith('image/') || IMAGE_EXT.test(file.name)) return false;
  const head = new Uint8Array(await file.slice(0, 3).arrayBuffer());
  return head[0] === 0x54 && head[1] === 0x53 && head[2] === 0x4c;
}

/** Reparte archivos: los .tsl se abren y el resto se convierte. */
async function route(files, preferred) {
  if (!files.length) return;
  const tsl = [];
  const imgs = [];
  for (const f of files) (await isTsl(f) ? tsl : imgs).push(f);
  if (tsl.length) {
    selectTab('open');
    await openEntries(await Promise.all(tsl.map(async (f) => ({ name: f.name, bytes: new Uint8Array(await f.arrayBuffer()) }))));
  }
  if (imgs.length) {
    if (!tsl.length || preferred === 'convert') selectTab('convert');
    addImages(imgs);
  }
}

// Arrastrar y soltar en cualquier parte de la página.
let dragDepth = 0;
const hasFiles = (e) => [...(e.dataTransfer?.types || [])].includes('Files');
addEventListener('dragenter', (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth++;
  $('dropover').hidden = false;
});
addEventListener('dragover', (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  e.dataTransfer.dropEffect = 'copy';
});
addEventListener('dragleave', (e) => {
  if (!hasFiles(e)) return;
  dragDepth = Math.max(0, dragDepth - 1);
  if (!dragDepth) $('dropover').hidden = true;
});
addEventListener('drop', (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth = 0;
  $('dropover').hidden = true;
  const panel = tabs.open.getAttribute('aria-selected') === 'true' ? 'open' : 'convert';
  route([...e.dataTransfer.files], panel);
});

// Archivos abiertos desde el sistema con la aplicación instalada (File Handling API).
if ('launchQueue' in window) {
  window.launchQueue.setConsumer(async (params) => {
    if (!params.files?.length) return;
    const files = await Promise.all(params.files.map((h) => h.getFile()));
    route(files, 'open');
  });
}

// ------------------------------------------------------------ Aplicación instalable

let installEvent = null;
addEventListener('beforeinstallprompt', (e) => {
  e.preventDefault();
  installEvent = e;
  $('install').hidden = false;
});
$('install-btn').addEventListener('click', async () => {
  if (!installEvent) return;
  installEvent.prompt();
  await installEvent.userChoice.catch(() => null);
  installEvent = null;
  $('install').hidden = true;
});
addEventListener('appinstalled', () => ($('install').hidden = true));

// Service worker: guarda la aplicación para usarla sin conexión y, si el
// servidor no envía COOP/COEP, los añade para poder codificar con varios
// hilos. En ese caso, la primera visita se recarga una vez cuando toma el
// control (solo si aún no se ha empezado a trabajar).
if ('serviceWorker' in navigator && location.protocol !== 'file:') {
  navigator.serviceWorker.register('sw.js').then(() => {
    if (self.crossOriginIsolated) return;
    navigator.serviceWorker.addEventListener('controllerchange', () => {
      if (items.length || opened.length || self.crossOriginIsolated) return;
      let tried = false;
      try {
        tried = sessionStorage.getItem('tsl-app-coi') === '1';
        sessionStorage.setItem('tsl-app-coi', '1');
      } catch {}
      if (!tried) location.reload();
    }, { once: true });
  }, (e) => console.warn('Tesela: sin service worker', e));
}

updateSettingsUi();
renderSummary();
if (location.hash === '#abrir' || new URLSearchParams(location.search).has('abrir')) selectTab('open');
