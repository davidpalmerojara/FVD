// Comparador: sube fotos y compáralas entre Tesela, WebP, AVIF, JPEG XL y JPEG.

const CODECS = {
  tesela: { label: 'Tesela', color: 'var(--c-tesela)', ext: 'tsl', q: 85 },
  avif: { label: 'AVIF', color: 'var(--c-avif)', ext: 'avif', q: 60 },
  jxl: { label: 'JPEG XL', color: 'var(--c-jxl)', ext: 'jxl', q: 85 },
  webp: { label: 'WebP', color: 'var(--c-webp)', ext: 'webp', q: 85 },
  jpeg: { label: 'JPEG', color: 'var(--c-jpeg)', ext: 'jpg', q: 85 },
};
const ORDER = Object.keys(CODECS);

const $ = (id) => document.getElementById(id);
const kb = (n) => (n >= 1024 * 1024 ? (n / 1048576).toFixed(2) + ' MB' : (n / 1024).toFixed(1) + ' KB');
const ms = (t) => (t >= 1000 ? (t / 1000).toFixed(1) + ' s' : Math.round(t) + ' ms');
const pct = (a, b) => {
  const v = (a / b - 1) * 100;
  return `<span class="${v < -0.5 ? 'neg' : v > 0.5 ? 'pos' : ''}">${v > 0 ? '+' : '−'}${Math.abs(v).toFixed(0)} %</span>`;
};

// ------------------------------------------------------------------ Estado

const state = {
  photos: [],
  selected: null,
  settings: {
    mode: 'target',
    target: 80,
    maxDim: 1024,
    codecs: new Set(ORDER),
    q: Object.fromEntries(ORDER.map((c) => [c, CODECS[c].q])),
    opts: { teselaEffort: 4, teselaTune: 'balanced', webpMethod: 6, avifSpeed: 6, jxlEffort: 7 },
  },
  view: { left: 'tesela', right: 'avif', zoom: 'fit', split: 50, panX: 0, panY: 0, mode: 'detail' },
};
let nextPhotoId = 1;

// ------------------------------------------------------------ Workers

// Memoria aproximada de cada trabajo en WebAssembly, en bytes por píxel
// (medida en Chromium con fotos de 12 y 24 MP, con margen): la del códec más
// la de las métricas (~22 B/px; SSIMULACRA2 se calcula por filas). Con fotos
// grandes, lanzar todos a la vez agota la RAM y puede colgar el ordenador: el
// grupo solo arranca en paralelo los trabajos que caben en el presupuesto, y
// siempre al menos uno.
const METRICS_BYTES_PER_PX = 30;
const BYTES_PER_PX = { jxl: 220, avif: 60, tesela: 40, webp: 30, jpeg: 20 };
const MEMORY_BUDGET = Math.min(navigator.deviceMemory || 4, 8) * 0.3 * 2 ** 30;
// Aunque vaya de uno en uno, un trabajo no puede pasar de esto (más lo que
// ocupa la página: original, resultados y miniaturas, ~100 B/px). Si la foto
// no cabe, se reduce lo justo y se avisa.
const PAGE_BYTES_PER_PX = 100;
const PEAK_BUDGET = Math.max(2, Math.min(navigator.deviceMemory || 4, 8) * 0.5) * 2 ** 30;
// Un worker no puede devolver la memoria de WebAssembly: tras un trabajo
// grande se cierra y se crea otro.
const RECYCLE_BYTES = 384 * 2 ** 20;

function jobBytes(msg) {
  return msg.width * msg.height * ((BYTES_PER_PX[msg.codec] ?? 100) + METRICS_BYTES_PER_PX);
}

class Pool {
  constructor(n) {
    this.n = n;
    this.workers = [];
    this.idle = [];
    this.queue = [];
    this.jobs = new Map();
    this.nextId = 1;
    this.used = 0;
  }
  run(msg, onProgress) {
    return new Promise((resolve, reject) => {
      this.queue.push({ msg, onProgress, resolve, reject, bytes: jobBytes(msg) });
      this.pump();
    });
  }
  cancel(pred) {
    this.queue = this.queue.filter((j) => {
      if (pred(j.msg)) {
        j.reject(new Error('cancelado'));
        return false;
      }
      return true;
    });
  }
  spawn() {
    const w = new Worker(new URL('./lab-worker.js', import.meta.url), { type: 'module' });
    w.onmessage = (ev) => {
      const { id, progress, result, error } = ev.data;
      const job = this.jobs.get(id);
      if (!job) return;
      if (progress) return job.onProgress?.(progress);
      this.jobs.delete(id);
      this.finish(w, job);
      error ? job.reject(new Error(error)) : job.resolve(result);
    };
    w.onerror = (ev) => {
      ev.preventDefault();
      for (const [id, job] of this.jobs) {
        if (job.worker === w) {
          this.jobs.delete(id);
          this.finish(w, job, true);
          job.reject(new Error(ev.message || 'error en el worker (¿sin memoria?)'));
        }
      }
    };
    this.workers.push(w);
    this.idle.push(w);
  }
  finish(w, job, broken = false) {
    this.used -= job.bytes;
    if (broken || job.bytes > RECYCLE_BYTES) {
      w.terminate();
      this.workers = this.workers.filter((x) => x !== w);
    } else {
      this.idle.push(w);
    }
    this.pump();
  }
  pump() {
    while (this.queue.length) {
      const busy = this.jobs.size;
      // El primero que quepa (o cualquiera si no hay nada en marcha).
      const k = this.queue.findIndex((j) => busy === 0 || this.used + j.bytes <= MEMORY_BUDGET);
      if (k < 0) return;
      if (!this.idle.length) {
        if (this.workers.length >= this.n) return;
        this.spawn();
      }
      const w = this.idle.pop();
      const [job] = this.queue.splice(k, 1);
      const id = this.nextId++;
      job.worker = w;
      this.used += job.bytes;
      this.jobs.set(id, job);
      w.postMessage({ id, ...job.msg });
    }
  }
}
const pool = new Pool(Math.max(2, Math.min(6, (navigator.hardwareConcurrency || 4) - 1)));


// ------------------------------------------------------------ Fotos

async function loadPixels(photo) {
  const { bitmap } = photo;
  const md = state.settings.maxDim;
  let s = md && Math.max(bitmap.width, bitmap.height) > md ? md / Math.max(bitmap.width, bitmap.height) : 1;
  // Límite de memoria: el trabajo más pesado de los códecs elegidos.
  const heaviest = Math.max(...[...state.settings.codecs].map((c) => BYTES_PER_PX[c] ?? 100), 0);
  const maxPx = PEAK_BUDGET / (heaviest + METRICS_BYTES_PER_PX + PAGE_BYTES_PER_PX);
  const px = bitmap.width * bitmap.height * s * s;
  photo.capped = null;
  if (px > maxPx) {
    s *= Math.sqrt(maxPx / px);
    photo.capped = { from: bitmap.width * bitmap.height, to: Math.round(bitmap.width * s) * Math.round(bitmap.height * s), jxl: state.settings.codecs.has('jxl') };
  }
  const w = Math.max(8, Math.round(bitmap.width * s));
  const h = Math.max(8, Math.round(bitmap.height * s));
  const c = new OffscreenCanvas(w, h);
  const ctx = c.getContext('2d');
  ctx.fillStyle = '#fff'; // las transparencias se componen sobre blanco
  ctx.fillRect(0, 0, w, h);
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(bitmap, 0, 0, w, h);
  const data = ctx.getImageData(0, 0, w, h).data;
  photo.w = w;
  photo.h = h;
  photo.rgba = new Uint8Array(data.buffer);
  photo.original = new ImageData(new Uint8ClampedArray(data), w, h);
}

async function addFile(file) {
  let bitmap;
  try {
    bitmap = await createImageBitmap(file, { imageOrientation: 'from-image' });
  } catch {
    alert(`No se pudo abrir «${file.name}». Formatos admitidos: los que tu navegador sepa leer (JPEG, PNG, WebP, AVIF…).`);
    return;
  }
  const photo = { id: nextPhotoId++, name: file.name, file, origW: bitmap.width, origH: bitmap.height, origBytes: file.size, bitmap, results: {} };
  const tc = new OffscreenCanvas(104, 104);
  const s = Math.max(104 / bitmap.width, 104 / bitmap.height);
  tc.getContext('2d').drawImage(bitmap, (104 - bitmap.width * s) / 2, (104 - bitmap.height * s) / 2, bitmap.width * s, bitmap.height * s);
  photo.thumb = URL.createObjectURL(await tc.convertToBlob({ type: 'image/jpeg', quality: 0.8 }));
  state.photos.push(photo);
  if (!state.selected) state.selected = photo.id;
  await process(photo);
}

async function process(photo) {
  pool.cancel((m) => m.photoId === photo.id);
  photo.run = (photo.run || 0) + 1;
  const run = photo.run;
  await loadPixels(photo);
  const st = state.settings;
  photo.mode = st.mode;
  photo.target = st.target;
  photo.results = {};
  const big = jobBytes({ width: photo.w, height: photo.h, codec: 'avif' }) > MEMORY_BUDGET / 2;
  for (const c of ORDER) {
    if (!st.codecs.has(c)) continue;
    photo.results[c] = { status: big ? 'en cola (foto grande: de uno en uno)' : 'en cola' };
  }
  render();
  await Promise.all(
    Object.keys(photo.results).map(async (c) => {
      try {
        const res = await pool.run(
          {
            photoId: photo.id,
            codec: c,
            rgba: photo.rgba,
            width: photo.w,
            height: photo.h,
            mode: st.mode,
            target: st.target,
            quality: st.q[c],
            opts: { ...st.opts },
          },
          (p) => {
            if (photo.run !== run) return;
            photo.results[c] = { status: p };
            render();
          },
        );
        if (photo.run !== run) return;
        photo.results[c] = { status: 'ok', ...res };
      } catch (e) {
        if (photo.run !== run || e.message === 'cancelado') return;
        photo.results[c] = { status: 'error', error: e.message };
      }
      render();
    }),
  );
  if (photo.run === run) logRun(photo, st);
}

// ------------------------------------------------------------ Registro

// Con `web/serve.py`, cada foto terminada se guarda en `registro/` (original,
// imagen probada y resultados) para poder revisar después dónde pierde Tesela.
// En un servidor estático la petición falla y no pasa nada.
async function logRun(photo, st) {
  try {
    const results = {};
    for (const [c, r] of Object.entries(photo.results)) {
      results[c] = {
        status: r.status,
        error: r.error,
        q: r.q,
        size: r.size,
        sizeAtTarget: r.sizeAtTarget ?? null,
        note: r.note || '',
        evals: r.evals || null,
        encMs: r.encMs,
        decMs: r.decMs,
        metrics: r.metrics,
      };
    }
    const meta = {
      fecha: new Date().toISOString(),
      nombre: photo.name,
      original: { w: photo.origW, h: photo.origH, bytes: photo.origBytes, tipo: photo.file?.type || '' },
      probada: { w: photo.w, h: photo.h },
      modo: st.mode,
      objetivo: st.target,
      ajustes: { maxDim: st.maxDim, q: st.q, opts: st.opts },
      resultados: results,
      navegador: navigator.userAgent,
      hilos: navigator.hardwareConcurrency || null,
    };
    const c = new OffscreenCanvas(photo.w, photo.h);
    c.getContext('2d').putImageData(photo.original, 0, 0);
    const fd = new FormData();
    fd.append('meta', JSON.stringify(meta));
    if (photo.file) fd.append('original', photo.file, photo.name);
    fd.append('probada', await c.convertToBlob({ type: 'image/png' }), 'probada.png');
    const r = await fetch('/api/registro', { method: 'POST', body: fd });
    if (r.ok) {
      photo.logged = true;
      render();
    }
  } catch {
    // Sin servidor de registro: no se guarda.
  }
}

// ------------------------------------------------------------ Render

let raf = 0;
function render() {
  if (!raf) raf = requestAnimationFrame(() => {
    raf = 0;
    renderList();
    renderDetail();
    renderSummary();
    renderOverview();
  });
}

// SSIMULACRA2 más alto que consiguió un formato en la búsqueda (si no llega al objetivo).
function maxS2(r) {
  return Math.max(r.metrics?.ssimulacra2 ?? 0, ...(r.evals || []).map((e) => e.s2));
}

function sizeOf(r, mode) {
  return mode === 'target' ? r.sizeAtTarget ?? null : r.size;
}

function done(photo) {
  return Object.values(photo.results).filter((r) => r.status === 'ok');
}

function renderList() {
  const ul = $('photos');
  ul.innerHTML = '';
  for (const p of state.photos) {
    const total = Object.keys(p.results).length;
    const ok = done(p).length;
    let status = `${ok}/${total} formatos`;
    const t = p.results.tesela;
    const j = p.results.jpeg;
    if (ok === total && t?.status === 'ok' && j?.status === 'ok' && sizeOf(t, p.mode) && sizeOf(j, p.mode)) {
      status = `Tesela <b>−${Math.round((1 - sizeOf(t, p.mode) / sizeOf(j, p.mode)) * 100)} %</b> vs JPEG`;
    }
    const li = document.createElement('li');
    li.innerHTML = `<button aria-current="${p.id === state.selected}"><img alt="" src="${p.thumb}"><span><span class="pn"></span><span class="ps num">${status}</span></span></button>`;
    li.querySelector('.pn').textContent = p.name;
    li.querySelector('button').onclick = () => {
      state.selected = p.id;
      state.view.panX = state.view.panY = 0;
      render();
    };
    ul.appendChild(li);
  }
  $('rerun').disabled = state.photos.length === 0;
}

function renderDetail() {
  const p = state.photos.find((x) => x.id === state.selected);
  const ov = state.view.mode === 'overview' && state.photos.length > 0;
  $('tabs').hidden = state.photos.length === 0;
  $('tab-detail').setAttribute('aria-selected', String(!ov));
  $('tab-overview').setAttribute('aria-selected', String(ov));
  $('tab-overview').textContent = `Resumen (${state.photos.length})`;
  $('overview').hidden = !ov;
  $('empty').hidden = !!p || ov;
  $('detail').hidden = !p || ov;
  if (!p || ov) return;
  $('d-name').textContent = p.name;
  const scaled = p.w !== p.origW ? ` (reducida desde ${p.origW}×${p.origH})` : '';
  $('d-meta').textContent = `${p.w}×${p.h} px${scaled} · ${p.mode === 'target' ? `misma calidad: SSIMULACRA2 = ${p.target}` : 'calidad fija'}${p.logged ? ' · guardada en el registro' : ''}`;
  $('th-size').textContent = p.mode === 'target' ? 'Tamaño a igual calidad' : 'Tamaño';

  const rows = ORDER.filter((c) => p.results[c]).map((c) => ({ c, r: p.results[c] }));
  const sizes = rows.filter(({ r }) => r.status === 'ok' && sizeOf(r, p.mode)).map(({ c, r }) => ({ c, s: sizeOf(r, p.mode) }));
  const max = Math.max(1, ...sizes.map((x) => x.s));
  const best = sizes.length ? sizes.reduce((a, b) => (b.s < a.s ? b : a)).c : null;
  const ref = (c) => p.results[c]?.status === 'ok' ? sizeOf(p.results[c], p.mode) : null;
  const jpeg = ref('jpeg');
  const webp = ref('webp');
  rows.sort((a, b) => (sizeOf(a.r, p.mode) ?? Infinity) - (sizeOf(b.r, p.mode) ?? Infinity));

  const tb = $('d-rows');
  tb.innerHTML = '';
  for (const { c, r } of rows) {
    const tr = document.createElement('tr');
    const info = CODECS[c];
    if (c === best) tr.className = 'best';
    if (r.status !== 'ok') {
      tr.innerHTML = `<td class="fmt"><span class="swatch" style="background:${info.color}"></span>${info.label}</td>
        <td colspan="10" class="status">${r.status === 'error' ? 'Error: ' + r.error : r.status + '…'}</td>`;
    } else {
      const s = sizeOf(r, p.mode);
      const m = r.metrics;
      tr.innerHTML = `
        <td class="fmt"><span class="swatch" style="background:${info.color}"></span>${info.label}${/croma 4:4:4/.test(r.note || '') ? ' <small class="muted">4:4:4</small>' : ''}</td>
        <td class="r sizecell" title="${p.mode === 'target' ? `Archivo real: ${kb(r.size)} con SSIMULACRA2 ${m.ssimulacra2.toFixed(1)}` : ''}">
          <span class="v">${s ? kb(s) : p.mode === 'target' && /no alcanza/.test(r.note || '') ? `no llega <small>(máx. ${maxS2(r).toFixed(1)})</small>` : '—'}</span>
          <span class="bar" style="width:calc(${s ? (s / max) * 100 : 0}% - 16px);background:${info.color}"></span></td>
        <td class="r">${s && jpeg && c !== 'jpeg' ? pct(s, jpeg) : '—'}</td>
        <td class="r">${s && webp && c !== 'webp' ? pct(s, webp) : '—'}</td>
        <td class="r">${m.ssimulacra2.toFixed(1)}</td>
        <td class="r">${m.psnr.toFixed(2)} dB</td>
        <td class="r">${m.ssim_y.toFixed(4)}</td>
        <td class="r">${r.q}</td>
        <td class="r">${ms(r.encMs)}</td>
        <td class="r">${ms(r.decMs)}</td>
        <td><button class="dl" title="Descargar el archivo ${info.ext}">⬇ .${info.ext}</button></td>`;
      tr.querySelector('.dl').onclick = () => download(r.bytes, `${p.name.replace(/\.[^.]+$/, '')}.${info.ext}`);
      if (r.note) tr.querySelector('.sizecell').title += ` (${r.note})`;
    }
    tb.appendChild(tr);
  }

  const t = ref('tesela');
  const others = sizes.filter((x) => x.c !== 'tesela');
  if (t && others.length && rows.every(({ r }) => r.status === 'ok' || r.status === 'error')) {
    const second = others.reduce((a, b) => (b.s < a.s ? b : a));
    $('d-verdict').innerHTML =
      t < second.s
        ? `Tesela ocupa un ${Math.round((1 - t / second.s) * 100)} % menos que ${CODECS[second.c].label}, el siguiente mejor${jpeg ? `, y un ${Math.round((1 - t / jpeg) * 100)} % menos que JPEG` : ''}.`
        : `${CODECS[best].label} gana en esta foto (Tesela: ${pct(t, sizes.find((x) => x.c === best).s)}).`;
  } else {
    $('d-verdict').textContent = '';
  }
  const noLlegan = rows.filter(({ r }) => r.status === 'ok' && p.mode === 'target' && /no alcanza/.test(r.note || '')).map(({ c }) => CODECS[c].label);
  $('d-note').textContent =
    (p.mode === 'target'
      ? 'Cada formato se codifica varias veces hasta rodear la calidad objetivo; el tamaño se interpola en ese punto exacto. El botón de descarga entrega el archivo real más cercano por encima del objetivo.'
      : 'Las escalas de calidad no son equivalentes entre formatos: compara tamaño y métricas a la vez.') +
    (noLlegan.length
      ? ` ${noLlegan.join(' y ')} no ${noLlegan.length > 1 ? 'llegan' : 'llega'} a la calidad objetivo en esta foto ni con su calidad máxima: con pérdida reduce${noLlegan.length > 1 ? 'n' : ''} el color a la mitad de resolución (4:2:0), y esta foto tiene detalle fino de color.`
      : '');
  if (p.capped) {
    const mp = (n) => (n / 1e6).toFixed(1).replace('.', ',');
    $('d-note').textContent +=
      ` La foto (${mp(p.capped.from)} MP) se ha reducido a ${mp(p.capped.to)} MP para no agotar la memoria del navegador.` +
      (p.capped.jxl ? ' JPEG XL es el que más necesita: desactívalo para comparar fotos más grandes.' : '');
  }
  $('d-note').textContent += self.crossOriginIsolated
    ? ' Tesela codifica con varios hilos; los demás formatos, con uno.'
    : ' Tesela codifica con un solo hilo: abre la página con web/serve.py (o recárgala) para usar varios.';
  renderViewer(p);
}

function download(bytes, name) {
  const url = URL.createObjectURL(new Blob([bytes]));
  const a = Object.assign(document.createElement('a'), { href: url, download: name });
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
}

// ------------------------------------------------------------ Visor

const viewer = $('viewer');
const cvL = $('cv-l');
const cvR = $('cv-r');
const drawn = { l: null, r: null };

function sourceFor(p, key) {
  if (key === 'original') return { data: p.original, label: 'Original' };
  const r = p.results[key];
  if (!r || r.status !== 'ok') return null;
  return {
    data: new ImageData(new Uint8ClampedArray(r.decoded.buffer, r.decoded.byteOffset, r.decoded.byteLength), p.w, p.h),
    label: `${CODECS[key].label} · ${kb(r.size)}`,
  };
}

/** Rellena un selector; si la opción preferida aún no está lista, muestra otra sin olvidarla. */
function fillSelect(sel, p, value, fallback) {
  const opts = ['original', ...ORDER.filter((c) => p.results[c]?.status === 'ok')];
  const cur = opts.includes(value) ? value : opts.includes(fallback) ? fallback : opts[0];
  const html = opts.map((o) => `<option value="${o}" ${o === cur ? 'selected' : ''}>${o === 'original' ? 'Original' : CODECS[o].label}</option>`).join('');
  if (sel.innerHTML !== html) sel.innerHTML = html;
  sel.value = cur;
  return cur;
}

function paint(cv, src, side, p) {
  const key = `${p.id}:${p.run}:${src?.label}`;
  if (drawn[side] === key) return;
  drawn[side] = key;
  cv.width = p.w;
  cv.height = p.h;
  if (src) cv.getContext('2d').putImageData(src.data, 0, 0);
}

function renderViewer(p) {
  const v = state.view;
  const left = fillSelect($('v-left'), p, v.left, 'original');
  const right = fillSelect($('v-right'), p, v.right, left === 'original' ? 'tesela' : 'original');
  const L = sourceFor(p, left);
  const R = sourceFor(p, right);
  paint(cvL, L, 'l', p);
  paint(cvR, R, 'r', p);
  $('vl-l').textContent = L ? '◀ ' + L.label : '';
  $('vl-r').textContent = R ? R.label + ' ▶' : '';
  layoutViewer(p);
}

function layoutViewer(p) {
  const v = state.view;
  const W = viewer.clientWidth;
  const H = viewer.clientHeight;
  const scale = v.zoom === 'fit' ? Math.min(W / p.w, H / p.h) : Number(v.zoom);
  const sw = p.w * scale;
  const sh = p.h * scale;
  let x;
  let y;
  if (v.zoom === 'fit' || sw <= W) {
    x = (W - sw) / 2;
    v.panX = 0;
  } else {
    v.panX = Math.min(0, Math.max(W - sw, v.panX));
    x = v.panX;
  }
  if (v.zoom === 'fit' || sh <= H) {
    y = (H - sh) / 2;
    v.panY = 0;
  } else {
    v.panY = Math.min(0, Math.max(H - sh, v.panY));
    y = v.panY;
  }
  for (const cv of [cvL, cvR]) cv.style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
  viewer.style.setProperty('--split', v.split + '%');
  viewer.classList.toggle('fit', v.zoom === 'fit');
  viewer.classList.toggle('pixel', scale >= 2);
}

let drag = null;
viewer.addEventListener('pointerdown', (e) => {
  const rect = viewer.getBoundingClientRect();
  const splitX = rect.left + (rect.width * state.view.split) / 100;
  const onSplit = Math.abs(e.clientX - splitX) < 22 || state.view.zoom === 'fit';
  drag = { mode: onSplit ? 'split' : 'pan', x: e.clientX, y: e.clientY, px: state.view.panX, py: state.view.panY };
  viewer.setPointerCapture(e.pointerId);
  if (onSplit) moveSplit(e);
});
viewer.addEventListener('pointermove', (e) => {
  if (!drag) return;
  const p = state.photos.find((x) => x.id === state.selected);
  if (!p) return;
  if (drag.mode === 'split') moveSplit(e);
  else {
    state.view.panX = drag.px + (e.clientX - drag.x);
    state.view.panY = drag.py + (e.clientY - drag.y);
    layoutViewer(p);
  }
});
viewer.addEventListener('pointerup', () => (drag = null));
viewer.addEventListener('pointercancel', () => (drag = null));
function moveSplit(e) {
  const rect = viewer.getBoundingClientRect();
  state.view.split = Math.max(0, Math.min(100, ((e.clientX - rect.left) / rect.width) * 100));
  viewer.style.setProperty('--split', state.view.split + '%');
}
window.addEventListener('resize', () => {
  const p = state.photos.find((x) => x.id === state.selected);
  if (p) layoutViewer(p);
});
$('v-left').onchange = (e) => ((state.view.left = e.target.value), render());
$('v-right').onchange = (e) => ((state.view.right = e.target.value), render());
for (const b of document.querySelectorAll('.zoom button')) {
  b.onclick = () => {
    const p = state.photos.find((x) => x.id === state.selected);
    const old = state.view.zoom;
    state.view.zoom = b.dataset.zoom;
    for (const o of document.querySelectorAll('.zoom button')) o.setAttribute('aria-pressed', String(o === b));
    if (p && old === 'fit' && state.view.zoom !== 'fit') {
      // centra la vista al ampliar
      const s = Number(state.view.zoom);
      state.view.panX = viewer.clientWidth / 2 - (p.w * s) / 2;
      state.view.panY = viewer.clientHeight / 2 - (p.h * s) / 2;
    }
    if (p) layoutViewer(p);
  };
}

// ------------------------------------------------------------ Resumen

function renderSummary() {
  const complete = state.photos.filter((p) => {
    const rs = Object.values(p.results);
    return rs.length && rs.every((r) => r.status === 'ok' || r.status === 'error');
  });
  $('summary').hidden = complete.length < 2 || state.view.mode === 'overview';
  if (complete.length < 2) return;
  const tot = {};
  const n = {};
  for (const p of complete) {
    for (const c of ORDER) {
      const r = p.results[c];
      const s = r?.status === 'ok' ? sizeOf(r, p.mode) : null;
      if (s) {
        tot[c] = (tot[c] || 0) + s;
        n[c] = (n[c] || 0) + 1;
      }
    }
  }
  const codecs = ORDER.filter((c) => n[c] === complete.length).sort((a, b) => tot[a] - tot[b]);
  const max = Math.max(...codecs.map((c) => tot[c]));
  const ref = tot.jpeg;
  $('s-body').innerHTML =
    `<p class="muted">${complete.length} fotos. Tamaño total de todas las fotos con cada formato.</p>` +
    codecs
      .map(
        (c) => `<div class="sumrow"><span><span class="swatch" style="background:${CODECS[c].color}"></span>${CODECS[c].label}</span>
          <span class="track"><i style="width:${(tot[c] / max) * 100}%;background:${CODECS[c].color}"></i></span>
          <span class="r num">${kb(tot[c])}${ref && c !== 'jpeg' ? ' · ' + pct(tot[c], ref) : ''}</span></div>`,
      )
      .join('');
}

// ------------------------------------------------------------ Resumen de todas las fotos

const RIVALS = ORDER.filter((c) => c !== 'tesela');

// Fila del resumen: tamaño de cada formato (null si no llega o falta) y la
// comparación de Tesela con el mejor de los demás.
function overviewRow(p) {
  const cell = {};
  for (const c of ORDER) {
    const r = p.results[c];
    if (!r) cell[c] = { k: 'off' };
    else if (r.status === 'error') cell[c] = { k: 'err', txt: r.error };
    else if (r.status !== 'ok') cell[c] = { k: 'run' };
    else if (sizeOf(r, p.mode)) cell[c] = { k: 'ok', s: sizeOf(r, p.mode), sub: /croma 4:4:4/.test(r.note || '') };
    else cell[c] = { k: 'nollega', max: maxS2(r) };
  }
  const t = cell.tesela.s;
  const rivals = RIVALS.filter((c) => cell[c].s);
  const best = rivals.length ? rivals.reduce((a, b) => (cell[b].s < cell[a].s ? b : a)) : null;
  const winner = [...ORDER].filter((c) => cell[c].s).reduce((a, b) => (a && cell[a].s <= cell[b].s ? a : b), null);
  return { p, cell, best, winner, dif: t && best ? t / cell[best].s - 1 : null };
}

function renderOverview() {
  if (state.view.mode !== 'overview' || !state.photos.length) return;
  const o = state.overview || (state.overview = { lose: false, sort: 'orden' });
  const cols = ORDER.filter((c) => state.photos.some((p) => p.results[c]));
  let rows = state.photos.map(overviewRow);
  const all = rows;
  if (o.lose) rows = rows.filter((r) => r.dif == null || r.dif > 0);
  if (o.sort === 'dif') rows = [...rows].sort((a, b) => (b.dif ?? -9) - (a.dif ?? -9));
  else if (o.sort === 'nombre') rows = [...rows].sort((a, b) => a.p.name.localeCompare(b.p.name));

  const modes = new Set(state.photos.map((p) => (p.mode === 'target' ? `SSIMULACRA2 = ${p.target}` : 'calidad fija')));
  $('o-meta').textContent = `${state.photos.length} fotos · ${[...modes].join(', ')}`;

  // Media geométrica de Tesela frente a cada formato (solo fotos con los dos).
  const stat = (c) => {
    const v = all.filter((r) => r.cell.tesela.s && r.cell[c]?.s).map((r) => Math.log(r.cell.tesela.s / r.cell[c].s));
    const nl = all.filter((r) => r.cell.tesela.s && r.cell[c]?.k === 'nollega').length;
    return { n: v.length, wins: v.filter((x) => x < 0).length + nl, total: v.length + nl, mean: v.length ? (Math.exp(v.reduce((a, b) => a + b, 0) / v.length) - 1) * 100 : null };
  };
  const finished = all.filter((r) => r.cell.tesela.s && r.best);
  const winsAll = finished.filter((r) => r.dif < 0).length + all.filter((r) => r.cell.tesela.s && !r.best && RIVALS.some((c) => r.cell[c]?.k === 'nollega')).length;
  $('o-stats').innerHTML =
    `<div class="card"><b>${winsAll} / ${all.filter((r) => r.cell.tesela.s).length}</b><span>fotos en que Tesela gana a todos</span></div>` +
    cols.filter((c) => c !== 'tesela').map((c) => {
      const st = stat(c);
      return `<div class="card"><b>${st.mean == null ? '—' : (st.mean > 0 ? '+' : '−') + Math.abs(st.mean).toFixed(1) + ' %'}</b><span>Tesela frente a ${CODECS[c].label} (gana en ${st.wins} de ${st.total})</span></div>`;
    }).join('');

  $('o-head').innerHTML = `<tr><th>Foto</th>${cols.map((c) => `<th class="r"><span class="swatch" style="background:${CODECS[c].color}"></span>${CODECS[c].label}</th>`).join('')}<th class="r">Tesela frente al mejor</th></tr>`;
  const fmtCell = (r, c) => {
    const x = r.cell[c];
    if (x.k === 'ok') return `<td class="r${c === r.winner ? ' win' : ''}">${kb(x.s)}${x.sub ? ' <small class="muted">4:4:4</small>' : ''}</td>`;
    if (x.k === 'nollega') return `<td class="r muted">no llega <small>(${x.max.toFixed(1)})</small></td>`;
    if (x.k === 'err') return `<td class="r muted" title="${x.txt}">error</td>`;
    if (x.k === 'run') return `<td class="r muted">…</td>`;
    return '<td></td>';
  };
  $('o-rows').innerHTML = rows.length
    ? rows.map((r) => `<tr data-id="${r.p.id}"><td class="pn"><img alt="" src="${r.p.thumb}"></td>${cols.map((c) => fmtCell(r, c)).join('')}
        <td class="r">${r.dif == null ? (r.cell.tesela.s && RIVALS.some((c) => r.cell[c]?.k === 'nollega') && !r.best ? '<span class="neg">solo llega Tesela</span>' : '—') : `<span class="${r.dif > 0.005 ? 'pos' : r.dif < -0.005 ? 'neg' : ''}">${r.dif > 0 ? '+' : '−'}${Math.abs(r.dif * 100).toFixed(1)} %</span> <small class="muted">${CODECS[r.best].label}</small>`}</td></tr>`).join('')
    : `<tr><td colspan="${cols.length + 2}" class="muted">Nada que mostrar con este filtro.</td></tr>`;
  // Nombre como texto (no HTML) y clic para abrir la foto.
  $('o-rows').querySelectorAll('tr[data-id]').forEach((tr) => {
    const p = state.photos.find((x) => x.id === Number(tr.dataset.id));
    tr.querySelector('.pn').append(p.name);
    tr.onclick = () => {
      state.selected = p.id;
      state.view.mode = 'detail';
      render();
    };
  });
}

function overviewCsv() {
  const cols = ORDER.filter((c) => state.photos.some((p) => p.results[c]));
  const lines = [['foto', 'ancho', 'alto', 'modo', ...cols.map((c) => `${c}_bytes`), ...cols.map((c) => `${c}_ssimulacra2`), 'mejor_otro', 'tesela_frente_al_mejor_pct'].join(',')];
  for (const p of state.photos) {
    const r = overviewRow(p);
    const q = (v) => `"${String(v).replace(/"/g, '""')}"`;
    lines.push([
      q(p.name), p.w, p.h, p.mode === 'target' ? `ssimulacra2=${p.target}` : 'fija',
      ...cols.map((c) => (r.cell[c].s ? Math.round(r.cell[c].s) : r.cell[c].k === 'nollega' ? 'no_llega' : '')),
      ...cols.map((c) => (p.results[c]?.metrics ? p.results[c].metrics.ssimulacra2.toFixed(2) : '')),
      r.best || '', r.dif == null ? '' : (r.dif * 100).toFixed(2),
    ].join(','));
  }
  download(new TextEncoder().encode(lines.join('\n') + '\n'), 'comparativa-tesela.csv');
}

$('tab-detail').onclick = () => ((state.view.mode = 'detail'), render());
$('tab-overview').onclick = () => ((state.view.mode = 'overview'), render());
$('o-lose').onchange = (e) => ((state.overview = { ...(state.overview || {}), lose: e.target.checked }), render());
$('o-sort').onchange = (e) => ((state.overview = { ...(state.overview || {}), sort: e.target.value }), render());
$('o-csv').onclick = overviewCsv;

// ------------------------------------------------------------ Controles

const TARGET_HINTS = [
  [90, 'Casi perfecta: indistinguible del original.'],
  [80, 'Alta: diferencias apenas visibles.'],
  [70, 'Buena: típica de webs bien optimizadas.'],
  [60, 'Media: se notan artefactos al ampliar.'],
  [0, 'Baja: artefactos visibles.'],
];
function updateTargetHint() {
  const t = state.settings.target;
  $('target-val').textContent = t;
  $('target-hint').textContent = TARGET_HINTS.find(([min]) => t >= min)[1];
}

$('target').oninput = (e) => {
  state.settings.target = Number(e.target.value);
  updateTargetHint();
};
for (const b of document.querySelectorAll('#mode button')) {
  b.onclick = () => {
    state.settings.mode = b.dataset.mode;
    for (const o of document.querySelectorAll('#mode button')) o.setAttribute('aria-pressed', String(o === b));
    $('target-box').hidden = state.settings.mode !== 'target';
    $('fixed-box').hidden = state.settings.mode !== 'fixed';
  };
}
$('maxdim').onchange = (e) => (state.settings.maxDim = Number(e.target.value));
for (const k of Object.keys(state.settings.opts)) {
  const el = $(k);
  el.onchange = () => (state.settings.opts[k] = el.type === 'number' ? Number(el.value) : el.value);
}
$('codecs').innerHTML = ORDER.map(
  (c) => `<label class="codec-check"><input type="checkbox" data-c="${c}" checked><span class="swatch" style="background:${CODECS[c].color}"></span>${CODECS[c].label}</label>`,
).join('');
for (const cb of document.querySelectorAll('#codecs input')) {
  cb.onchange = () => (cb.checked ? state.settings.codecs.add(cb.dataset.c) : state.settings.codecs.delete(cb.dataset.c));
}
$('fixed-q').innerHTML = ORDER.map(
  (c) => `<label class="row"><span><span class="swatch" style="background:${CODECS[c].color}"></span>${CODECS[c].label}</span><input type="number" min="1" max="100" value="${CODECS[c].q}" data-c="${c}"></label>`,
).join('');
for (const inp of document.querySelectorAll('#fixed-q input')) {
  inp.onchange = () => (state.settings.q[inp.dataset.c] = Number(inp.value));
}
$('rerun').onclick = () => state.photos.forEach((p) => process(p));

const drop = $('drop');
const fileInput = $('file');
fileInput.onchange = () => {
  [...fileInput.files].forEach(addFile);
  fileInput.value = '';
};
for (const t of [drop, document.body]) {
  t.addEventListener('dragover', (e) => {
    e.preventDefault();
    drop.classList.add('over');
  });
  t.addEventListener('dragleave', () => drop.classList.remove('over'));
  t.addEventListener('drop', (e) => {
    e.preventDefault();
    drop.classList.remove('over');
    [...e.dataTransfer.files].filter((f) => f.type.startsWith('image/') || /\.(png|jpe?g|webp|avif|gif|bmp)$/i.test(f.name)).forEach(addFile);
  });
}
$('sample').onclick = async () => {
  const blob = await (await fetch('sample.png')).blob();
  addFile(new File([blob], 'kodim23.png', { type: 'image/png' }));
};
updateTargetHint();
render();
