// Interfaz de Tesela para escritorio. Sin dependencias ni compilación: usa la
// API global de Tauri (app.withGlobalTauri) y los comandos de src/main.rs.
'use strict';

const T = window.__TAURI__;
const invoke = T.core.invoke;
const $ = (id) => document.getElementById(id);

const IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp'];
const ext = (p) => (p.match(/\.([^./\\]+)$/) || [])[1]?.toLowerCase() || '';
const baseName = (p) => p.split(/[\\/]/).pop();
const dirName = (p) => p.slice(0, Math.max(0, p.length - baseName(p).length - 1));

const num1 = new Intl.NumberFormat('es-ES', { maximumFractionDigits: 1 });
const num2 = new Intl.NumberFormat('es-ES', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const int = new Intl.NumberFormat('es-ES');
function bytes(n) {
  if (n < 1024) return `${int.format(n)} B`;
  if (n < 1024 * 1024) return `${num1.format(n / 1024)} KB`;
  return `${num1.format(n / 1024 / 1024)} MB`;
}
function errText(e) { return typeof e === 'string' ? e : (e && e.message) || String(e); }

// Aviso breve en la parte inferior.
let toastTimer = 0;
function toast(msg, isError = false) {
  const t = $('aviso');
  t.textContent = msg;
  t.classList.toggle('error', isError);
  t.classList.add('on');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove('on'), isError ? 6000 : 3500);
}

// ---------------------------------------------------------------- pestañas
const tabs = [$('tab-visor'), $('tab-conv')];
function selectTab(tab, focus = false) {
  for (const t of tabs) {
    const on = t === tab;
    t.setAttribute('aria-selected', String(on));
    t.tabIndex = on ? 0 : -1;
    $(t.getAttribute('aria-controls')).hidden = !on;
  }
  if (focus) tab.focus();
  if (tab === tabs[0]) requestAnimationFrame(relayout);
}
for (const t of tabs) {
  t.addEventListener('click', () => selectTab(t));
  t.addEventListener('keydown', (e) => {
    const i = tabs.indexOf(t);
    if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
      e.preventDefault();
      e.stopPropagation();
      selectTab(tabs[(i + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length], true);
    }
  });
}
const viewerActive = () => !$('visor').hidden;

// ------------------------------------------------------------------- visor
const stage = $('stage');
const canvas = $('lienzo');
const ctx = canvas.getContext('2d');
const V = {
  info: null,       // DocInfo de Rust
  frames: [],       // ImageData por fotograma
  cur: 0,
  playing: false,
  timer: 0,
  loopsDone: 0,
  s: 1, tx: 0, ty: 0,
  mode: 'fit',      // 'fit' | 'real' | 'libre'
  token: 0,         // descarta cargas obsoletas
  target: null,     // último archivo pedido (para encadenar ← →)
};
const isAnim = () => V.info && V.info.durations.length > 1;
const dpr = () => window.devicePixelRatio || 1;
// 100 % = un píxel de la imagen por píxel de la pantalla.
const realScale = () => 1 / dpr();

function stageSize() {
  const r = stage.getBoundingClientRect();
  return [r.width, r.height];
}
function fitScale() {
  const [sw, sh] = stageSize();
  const { width: w, height: h } = V.info;
  return Math.min(realScale(), Math.max(1, sw - 32) / w, Math.max(1, sh - 32) / h);
}
function clampPan() {
  const [sw, sh] = stageSize();
  const iw = V.info.width * V.s;
  const ih = V.info.height * V.s;
  V.tx = iw <= sw ? (sw - iw) / 2 : Math.min(0, Math.max(sw - iw, V.tx));
  V.ty = ih <= sh ? (sh - ih) / 2 : Math.min(0, Math.max(sh - ih, V.ty));
  stage.classList.toggle('pannable', iw > sw + 0.5 || ih > sh + 0.5);
}
function applyView() {
  canvas.style.transform = `translate(${V.tx}px, ${V.ty}px) scale(${V.s})`;
  canvas.classList.toggle('pixelated', V.s * dpr() >= 2);
  $('zoom').textContent = `${int.format(Math.round(V.s * dpr() * 100))} %`;
  $('ajustar').setAttribute('aria-pressed', String(V.mode === 'fit'));
  $('real').setAttribute('aria-pressed', String(V.mode === 'real'));
}
function setMode(mode) {
  if (!V.info) return;
  V.mode = mode;
  V.s = mode === 'fit' ? fitScale() : realScale();
  V.tx = V.ty = 0;
  clampPan();
  if (mode === 'real') {
    // Centrado.
    const [sw, sh] = stageSize();
    V.tx = Math.min(0, (sw - V.info.width * V.s) / 2) || V.tx;
    V.ty = Math.min(0, (sh - V.info.height * V.s) / 2) || V.ty;
    clampPan();
  }
  applyView();
}
function zoomAt(newS, px, py) {
  if (!V.info) return;
  newS = Math.min(32, Math.max(0.01, newS));
  V.tx = px - (px - V.tx) * (newS / V.s);
  V.ty = py - (py - V.ty) * (newS / V.s);
  V.s = newS;
  V.mode = Math.abs(newS - realScale()) < 1e-6 ? 'real' : 'libre';
  clampPan();
  applyView();
}
function zoomStep(dir) {
  const [sw, sh] = stageSize();
  zoomAt(V.s * (dir > 0 ? 1.25 : 0.8), sw / 2, sh / 2);
}
function relayout() {
  if (!V.info) return;
  if (V.mode === 'fit') setMode('fit');
  else { clampPan(); applyView(); }
}
window.addEventListener('resize', relayout);

stage.addEventListener('wheel', (e) => {
  if (!V.info) return;
  e.preventDefault();
  const r = stage.getBoundingClientRect();
  const d = e.deltaMode === 1 ? e.deltaY * 40 : e.deltaY;
  zoomAt(V.s * Math.exp(-d * 0.0015), e.clientX - r.left, e.clientY - r.top);
}, { passive: false });

let drag = null;
stage.addEventListener('pointerdown', (e) => {
  if (!V.info || e.button !== 0 || !stage.classList.contains('pannable')) return;
  drag = { x: e.clientX, y: e.clientY, tx: V.tx, ty: V.ty };
  stage.setPointerCapture(e.pointerId);
  stage.classList.add('panning');
});
stage.addEventListener('pointermove', (e) => {
  if (!drag) return;
  V.tx = drag.tx + e.clientX - drag.x;
  V.ty = drag.ty + e.clientY - drag.y;
  clampPan();
  applyView();
});
const endDrag = () => { drag = null; stage.classList.remove('panning'); };
stage.addEventListener('pointerup', endDrag);
stage.addEventListener('pointercancel', endDrag);
stage.addEventListener('dblclick', (e) => {
  if (!V.info) return;
  if (V.mode === 'fit') {
    const r = stage.getBoundingClientRect();
    zoomAt(realScale(), e.clientX - r.left, e.clientY - r.top);
  } else setMode('fit');
});

function pan(dx, dy) {
  if (!V.info) return;
  V.tx += dx;
  V.ty += dy;
  clampPan();
  applyView();
}

function toImageData(buf, w, h) {
  const px = buf instanceof ArrayBuffer ? new Uint8ClampedArray(buf) : Uint8ClampedArray.from(buf);
  return new ImageData(px, w, h);
}

function showFrame(i) {
  const f = V.frames[i];
  if (!f) return;
  V.cur = i;
  ctx.putImageData(f, 0, 0);
  if (isAnim()) {
    $('fotograma').textContent = `${int.format(i + 1)} / ${int.format(V.info.durations.length)}`;
  }
}

// Reproducción con la duración de cada fotograma.
function schedule() {
  clearTimeout(V.timer);
  V.timer = setTimeout(tick, Math.max(10, V.info.durations[V.cur] || 100));
}
function tick() {
  if (!V.playing || !isAnim()) return;
  let n = V.cur + 1;
  if (n >= V.info.durations.length) {
    if (V.info.loops && ++V.loopsDone >= V.info.loops) { pause(); return; }
    n = 0;
  }
  if (!V.frames[n]) { V.timer = setTimeout(tick, 30); return; } // aún cargando
  showFrame(n);
  schedule();
}
function play() {
  if (!isAnim()) return;
  if (V.info.loops && V.loopsDone >= V.info.loops) V.loopsDone = 0;
  V.playing = true;
  $('play').textContent = 'Pausa';
  schedule();
}
function pause() {
  V.playing = false;
  clearTimeout(V.timer);
  $('play').textContent = 'Reproducir';
}
function stepFrame(d) {
  if (!isAnim()) return;
  pause();
  const n = V.info.durations.length;
  const i = (V.cur + d + n) % n;
  if (V.frames[i]) showFrame(i);
}

function renderInfo() {
  const i = V.info;
  const parts = [];
  parts.push(`<span class="name">${escapeHtml(i.name)}</span>`);
  parts.push(`<span><b>${int.format(i.width)} × ${int.format(i.height)}</b> px</span>`);
  const bpp = i.frameCount > 1 ? '' : ` · ${num2.format((i.bytes * 8) / (i.width * i.height))} bpp`;
  parts.push(`<span><b>${bytes(i.bytes)}</b>${bpp}</span>`);
  if (i.durations.length > 1) {
    const total = i.durations.reduce((a, b) => a + b, 0) / 1000;
    const rep = i.loops === 0 ? 'en bucle' : i.loops === 1 ? 'una vez' : `${i.loops} veces`;
    parts.push(`<span><b>${int.format(i.durations.length)}</b> fotogramas · ${num1.format(total)} s · ${rep}</span>`);
  } else if (i.frameCount > 1) {
    parts.push(`<span>animación de ${int.format(i.frameCount)} fotogramas (demasiado grande: se muestra el primero)</span>`);
  }
  const tags = [];
  if (i.alpha) tags.push('transparencia');
  if (i.lossless) tags.push('sin pérdida');
  tags.push(`v${i.version}`);
  parts.push(`<span>${tags.join(' · ')}</span>`);
  parts.push(`<span>${int.format(i.index)} de ${int.format(i.total)} en la carpeta</span>`);
  parts.push(`<span class="faint">decodificado en ${int.format(i.decodeMs)} ms</span>`);
  $('info').innerHTML = parts.join('');
}
function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

async function openPath(path) {
  const token = ++V.token;
  V.target = path;
  $('ocupado').hidden = false;
  try {
    const info = await invoke('open_tsl', { path });
    if (token !== V.token) return;
    const first = await invoke('frame', { index: 0 });
    if (token !== V.token) return;
    pause();
    V.info = info;
    V.frames = [toImageData(first, info.width, info.height)];
    V.cur = 0;
    V.loopsDone = 0;
    canvas.width = info.width;
    canvas.height = info.height;
    canvas.hidden = false;
    $('vacio').hidden = true;
    showFrame(0);
    setMode('fit');
    renderInfo();
    for (const id of ['anterior', 'siguiente']) $(id).disabled = info.total < 2;
    for (const id of ['ajustar', 'real', 'alejar', 'acercar', 'guardar']) $(id).disabled = false;
    $('anim').hidden = !isAnim();
    T.window.getCurrentWindow().setTitle(`${info.name} — Tesela`).catch(() => {});
    if (isAnim()) {
      $('fotograma').textContent = `1 / ${int.format(info.durations.length)}`;
      play();
      // El resto de fotogramas, sin bloquear la reproducción.
      for (let k = 1; k < info.durations.length; k++) {
        const buf = await invoke('frame', { index: k });
        if (token !== V.token) return;
        V.frames[k] = toImageData(buf, info.width, info.height);
      }
    }
  } catch (e) {
    if (token === V.token) toast(`No se pudo abrir ${baseName(path)}: ${errText(e)}`, true);
  } finally {
    if (token === V.token) $('ocupado').hidden = true;
  }
}

async function openDialog() {
  const p = await invoke('pick_tsl');
  if (p) { selectTab(tabs[0]); openPath(p); }
}
// Las pulsaciones seguidas se encadenan: cada una parte del archivo que pidió
// la anterior, aunque aún no se haya terminado de decodificar.
let navChain = Promise.resolve();
function neighbor(step) {
  navChain = navChain.then(async () => {
    const from = V.target || (V.info && V.info.path);
    if (!from || !V.info || V.info.total < 2) return;
    const p = await invoke('neighbor', { path: from, step });
    if (p && p !== from) openPath(p);
  }).catch((e) => toast(errText(e), true));
}
async function savePng() {
  if (!V.info) return;
  try {
    const out = await invoke('save_png', { index: V.cur });
    if (out) toast(`Guardado: ${out}`);
  } catch (e) {
    toast(`No se pudo guardar: ${errText(e)}`, true);
  }
}

$('abrir').addEventListener('click', openDialog);
$('anterior').addEventListener('click', () => neighbor(-1));
$('siguiente').addEventListener('click', () => neighbor(1));
$('ajustar').addEventListener('click', () => setMode('fit'));
$('real').addEventListener('click', () => setMode('real'));
$('acercar').addEventListener('click', () => zoomStep(1));
$('alejar').addEventListener('click', () => zoomStep(-1));
$('play').addEventListener('click', () => (V.playing ? pause() : play()));
$('guardar').addEventListener('click', savePng);

// --------------------------------------------------------------- conversor
const C = { items: [], busy: false, folder: null };

function quality() { return Number($('calidad').value); }
$('calidad').addEventListener('input', () => { $('calidad-v').textContent = $('calidad').value; });
$('sinperdida').addEventListener('change', () => { $('calidad').disabled = $('sinperdida').checked; });

function destino() { return document.querySelector('input[name="destino"]:checked').value; }
async function chooseFolder() {
  const d = await invoke('pick_folder');
  if (d) {
    C.folder = d;
    $('carpeta').textContent = d;
    $('carpeta').title = d;
    document.querySelector('input[name="destino"][value="carpeta"]').checked = true;
  }
  return d;
}
$('elegir-carpeta').addEventListener('click', chooseFolder);

function updateConvButtons() {
  const pending = C.items.some((it) => it.status === 'pendiente' || it.status === 'error');
  $('convertir').disabled = C.busy || !pending;
  $('vaciar').disabled = C.busy || C.items.length === 0;
}

function setStatus(it, html) { it.el.querySelector('.estado').innerHTML = html; }

function addImages(paths) {
  let added = 0;
  for (const p of paths) {
    if (!IMAGE_EXTS.includes(ext(p))) continue;
    if (C.items.some((it) => it.path === p && it.status !== 'hecho')) continue;
    const li = document.createElement('li');
    li.innerHTML = `<span class="fname"></span><span class="dir"></span><span class="estado">Pendiente</span>`;
    li.querySelector('.fname').textContent = baseName(p);
    li.querySelector('.dir').textContent = `\u200E${dirName(p)}\u200E`;
    $('lista').append(li);
    C.items.push({ path: p, el: li, status: 'pendiente' });
    added++;
  }
  updateConvButtons();
  return added;
}

async function pickImages() {
  const paths = await invoke('pick_images');
  if (paths.length) addImages(paths);
}
$('zona').addEventListener('click', pickImages);
$('zona').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pickImages(); }
});

async function convertAll() {
  let outDir = null;
  if (destino() === 'carpeta') {
    outDir = C.folder || (await chooseFolder());
    if (!outDir) return;
  }
  C.busy = true;
  updateConvButtons();
  const q = quality();
  const lossless = $('sinperdida').checked;
  let orig = 0, tsl = 0, ok = 0;
  for (const it of C.items) {
    if (it.status !== 'pendiente' && it.status !== 'error') continue;
    it.status = 'conv';
    setStatus(it, '<span class="spinner" aria-hidden="true"></span>Codificando…');
    try {
      const r = await invoke('convert', { path: it.path, quality: q, lossless, outDir });
      it.status = 'hecho';
      it.result = r;
      it.el.classList.add('hecho');
      const saved = 1 - r.tslBytes / r.originalBytes;
      const pct = `${saved >= 0 ? '−' : '+'}${num1.format(Math.abs(saved) * 100)} %`;
      const frames = r.frames > 1 ? ` · ${int.format(r.frames)} fotogramas` : '';
      setStatus(it,
        `<span>${bytes(r.originalBytes)} → <b>${bytes(r.tslBytes)}</b></span>` +
        `<span class="${saved >= 0 ? 'ahorro' : 'peor'}">${pct}</span>` +
        `<span class="faint">${num1.format(r.millis / 1000)} s${frames}</span>` +
        `<button class="btn small" data-ver>Ver</button><button class="btn small" data-mostrar>Mostrar</button>`);
      it.el.querySelector('[data-ver]').addEventListener('click', () => { selectTab(tabs[0]); openPath(r.output); });
      it.el.querySelector('[data-mostrar]').addEventListener('click', () => invoke('reveal', { path: r.output }).catch((e) => toast(errText(e), true)));
      it.el.querySelector('.dir').textContent = `\u200E${r.output}\u200E`;
      orig += r.originalBytes;
      tsl += r.tslBytes;
      ok++;
    } catch (e) {
      it.status = 'error';
      setStatus(it, '<span class="error"></span>');
      it.el.querySelector('.error').textContent = errText(e);
    }
  }
  C.busy = false;
  updateConvButtons();
  if (ok) {
    $('resumen').textContent =
      `${int.format(ok)} convertida${ok > 1 ? 's' : ''}: ${bytes(orig)} → ${bytes(tsl)} (−${num1.format((1 - tsl / orig) * 100)} %)`;
  }
}
$('convertir').addEventListener('click', convertAll);
$('vaciar').addEventListener('click', () => {
  C.items = [];
  $('lista').replaceChildren();
  $('resumen').textContent = '';
  updateConvButtons();
});

// ------------------------------------------- archivos de fuera (arrastrar, abrir con)
function route(paths) {
  const tsl = paths.filter((p) => ext(p) === 'tsl');
  const imgs = paths.filter((p) => IMAGE_EXTS.includes(ext(p)));
  const other = paths.filter((p) => !tsl.includes(p) && !imgs.includes(p));
  if (imgs.length) { addImages(imgs); selectTab(tabs[1]); }
  if (tsl.length) {
    if (!imgs.length) selectTab(tabs[0]);
    openPath(tsl[0]);
  }
  if (other.length) toast(`Formato no admitido: ${other.map(baseName).join(', ')}`, true);
}

T.webview.getCurrentWebview().onDragDropEvent((ev) => {
  const p = ev.payload;
  if (p.type === 'enter' || p.type === 'over') {
    $('velo').hidden = false;
    $('zona').classList.add('over');
  } else {
    $('velo').hidden = true;
    $('zona').classList.remove('over');
    if (p.type === 'drop' && p.paths && p.paths.length) route(p.paths);
  }
});

// ------------------------------------------------------------------ teclado
function typing(el) {
  return el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable);
}
document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === 'o') { e.preventDefault(); openDialog(); return; }
  if (!viewerActive()) return;
  if (mod && e.key.toLowerCase() === 's') { e.preventDefault(); savePng(); return; }
  if (mod || e.altKey || typing(e.target)) return;
  const onButton = e.target && e.target.tagName === 'BUTTON';
  switch (e.key) {
    case 'ArrowLeft': e.preventDefault(); e.shiftKey ? pan(80, 0) : neighbor(-1); break;
    case 'ArrowRight': e.preventDefault(); e.shiftKey ? pan(-80, 0) : neighbor(1); break;
    case 'ArrowUp': if (e.shiftKey) { e.preventDefault(); pan(0, 80); } break;
    case 'ArrowDown': if (e.shiftKey) { e.preventDefault(); pan(0, -80); } break;
    case '+': case '=': e.preventDefault(); zoomStep(1); break;
    case '-': case '_': e.preventDefault(); zoomStep(-1); break;
    case '0': case 'f': e.preventDefault(); setMode('fit'); break;
    case '1': e.preventDefault(); setMode('real'); break;
    case ' ':
      if (onButton || !isAnim()) return;
      e.preventDefault();
      V.playing ? pause() : play();
      break;
    case ',': stepFrame(-1); break;
    case '.': stepFrame(1); break;
    default:
  }
});

// ------------------------------------------------------------------ inicio
(async () => {
  // Primero el oyente; después se piden los archivos con los que se abrió.
  await T.event.listen('abrir', (ev) => route(ev.payload));
  const initial = await invoke('initial_paths');
  if (initial.length) route(initial);
})();
