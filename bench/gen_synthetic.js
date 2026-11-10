// Genera imágenes de prueba de "capturas de pantalla" e "ilustraciones" con
// Chromium (Playwright), para validar el códec fuera de la fotografía.
//
//   cargo doc --no-deps && node bench/gen_synthetic.js bench/data
//
// Crea bench/data/screen/*.png y bench/data/illus/*.png.

const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');
const http = require('http');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.resolve(process.argv[2] || path.join(ROOT, 'bench', 'data'));
const EMOJI = ['1F600', '1F98A', '1F680', '1F333', '1F355', '1F431', '1F308', '1F3B8', '1F30B', '1F422', '1F34E', '1F3E0',
  '1F697', '1F41D', '1F984', '1F335', '1F369', '1F6B2'];

function serve(dir) {
  const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm', '.png': 'image/png', '.jpg': 'image/jpeg', '.tsl': 'application/octet-stream' };
  const srv = http.createServer((req, res) => {
    const f = path.join(dir, decodeURIComponent(req.url.split('?')[0]));
    fs.readFile(f, (err, data) => {
      if (err) { res.writeHead(404); return res.end(); }
      res.writeHead(200, { 'Content-Type': types[path.extname(f)] || 'application/octet-stream' });
      res.end(data);
    });
  });
  return new Promise((r) => srv.listen(0, () => r(srv)));
}

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;');

function terminalHtml() {
  const src = fs.readFileSync(path.join(ROOT, 'bench', 'results', 'REPORT.md'), 'utf8');
  const lines = [
    '$ cargo test --release',
    '   Compiling tesela v0.1.0 (/home/user/tesela)',
    '    Finished `release` profile [optimized] target(s) in 24.71s',
    '     Running unittests src/lib.rs',
    'running 2 tests',
    'test dct::tests::roundtrip ... ok',
    'test rc::tests::roundtrip_bits_and_ints ... ok',
    'test result: ok. 2 passed; 0 failed; 0 ignored',
    '     Running tests/roundtrip.rs',
    'test decoder_matches_encoder_on_odd_sizes ... ok',
    'test tiles_roundtrip ... ok',
    'test result: ok. 5 passed; 0 failed; 0 ignored',
    '$ cat bench/results/REPORT.md',
    ...src.split('\n'),
  ];
  const body = lines.map((l) => {
    const c = l.startsWith('$') ? '#7ee787' : l.includes('ok') ? '#79c0ff' : l.startsWith('|') ? '#e6edf3' : '#c9d1d9';
    return `<div style="color:${c}">${esc(l) || '&nbsp;'}</div>`;
  }).join('');
  return `<html><body style="margin:0;background:#0d1117;font:14px/1.45 'DejaVu Sans Mono',monospace;padding:18px 22px">${body}</body></html>`;
}

function sheetHtml() {
  const rows = JSON.parse(fs.readFileSync(path.join(ROOT, 'bench', 'results', 'raw.json'), 'utf8')).slice(0, 48);
  const cols = ['codec', 'image', 'param', 'bytes', 'bpp', 'psnr', 'psnr_y', 'ssim_y', 'ssimulacra2', 'enc_s', 'dec_s'];
  const letters = 'ABCDEFGHIJKL';
  const fmt = (v) => (typeof v === 'number' ? (Number.isInteger(v) ? v : v.toFixed(4)) : esc(String(v)));
  let h = '<tr><th></th>' + cols.map((_, i) => `<th>${letters[i]}</th>`).join('') + '</tr>';
  h += '<tr><th>1</th>' + cols.map((c) => `<td style="font-weight:700;background:#e8f0fe">${c}</td>`).join('') + '</tr>';
  rows.forEach((r, i) => {
    h += `<tr><th>${i + 2}</th>` + cols.map((c) => `<td style="text-align:${typeof r[c] === 'number' ? 'right' : 'left'}">${fmt(r[c])}</td>`).join('') + '</tr>';
  });
  return `<html><body style="margin:0;font:13px Arial,sans-serif;background:#fff">
    <div style="height:38px;background:#f1f3f4;border-bottom:1px solid #dadce0;display:flex;align-items:center;padding-left:12px;color:#444">Archivo &nbsp; Editar &nbsp; Ver &nbsp; Insertar &nbsp; Formato &nbsp; Datos</div>
    <table style="border-collapse:collapse">${h}</table>
    <style>th{background:#f8f9fa;color:#666;font-weight:400;border:1px solid #dadce0;padding:3px 6px;min-width:26px}td{border:1px solid #e2e3e3;padding:3px 7px;white-space:nowrap}</style></body></html>`;
}

function emojiHtml(base, ids, bg) {
  const imgs = ids.map((id) => `<img src="${base}/${id}.svg" style="width:300px;height:300px">`).join('');
  return `<html><body style="margin:0;background:${bg};display:grid;grid-template-columns:repeat(3,300px);gap:24px;padding:38px">${imgs}</body></html>`;
}

function landscapeHtml() {
  return `<html><body style="margin:0"><svg width="1600" height="1000" viewBox="0 0 1600 1000" xmlns="http://www.w3.org/2000/svg">
  <defs><linearGradient id="sky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ff9a62"/><stop offset="0.55" stop-color="#ffd59a"/><stop offset="1" stop-color="#fff1d6"/></linearGradient>
  <linearGradient id="sea" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3d7ea6"/><stop offset="1" stop-color="#1b3b5a"/></linearGradient></defs>
  <rect width="1600" height="1000" fill="url(#sky)"/>
  <circle cx="1120" cy="420" r="120" fill="#fff4c2"/>
  <path d="M0 560 L220 360 L380 500 L560 300 L760 520 L940 380 L1120 560 L1300 420 L1600 600 L1600 700 L0 700Z" fill="#6b4f7a"/>
  <path d="M0 620 L180 500 L360 600 L520 470 L720 640 L900 520 L1100 650 L1320 540 L1600 680 L1600 760 L0 760Z" fill="#8c5e7c"/>
  <rect y="700" width="1600" height="300" fill="url(#sea)"/>
  ${Array.from({ length: 40 }, (_, i) => `<rect x="${(i * 97) % 1600}" y="${720 + ((i * 53) % 260)}" width="${40 + (i % 5) * 18}" height="3" rx="1.5" fill="#ffffff" opacity="0.35"/>`).join('')}
  <path d="M120 1000 Q160 820 230 760 Q260 900 300 1000Z" fill="#203b2c"/><path d="M1380 1000 Q1420 780 1500 720 Q1540 880 1560 1000Z" fill="#203b2c"/>
  <text x="80" y="140" font-family="DejaVu Sans, sans-serif" font-size="72" font-weight="700" fill="#5a2e46">Atardecer en la costa</text>
  <text x="84" y="200" font-family="DejaVu Sans, sans-serif" font-size="30" fill="#7a4a5e">Ilustración vectorial de prueba</text></svg></body></html>`;
}

(async () => {
  for (const d of ['screen', 'illus']) fs.mkdirSync(path.join(OUT, d), { recursive: true });
  const web = await serve(path.join(ROOT, 'web'));
  const webUrl = `http://localhost:${web.address().port}`;
  const browser = await chromium.launch();
  const shot = async (file, { url, html, w, h, dpr = 1, before }) => {
    const p = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: dpr });
    if (url) await p.goto(url, { waitUntil: 'networkidle' });
    else await p.setContent(html, { waitUntil: 'networkidle' });
    if (before) await before(p);
    await p.waitForTimeout(400);
    await p.screenshot({ path: path.join(OUT, file) });
    await p.close();
    console.log(file);
  };
  const doc = (f) => 'file://' + path.join(ROOT, 'target', 'doc', f);
  const dark = async (p) => {
    await p.evaluate(() => {
      localStorage.setItem('rustdoc-use-system-theme', 'false');
      localStorage.setItem('rustdoc-theme', 'dark');
    });
    await p.reload({ waitUntil: 'networkidle' });
  };

  await shot('screen/s01_landing.png', { url: `${webUrl}/index.html`, w: 1280, h: 900 });
  await shot('screen/s02_demo.png', { url: `${webUrl}/demo/index.html`, w: 1280, h: 900, before: (p) => p.waitForTimeout(2500) });
  await shot('screen/s03_comparador.png', { url: `${webUrl}/compare/index.html`, w: 1440, h: 900 });
  await shot('screen/s04_rustdoc.png', { url: doc('tesela/codec/struct.Params.html'), w: 1280, h: 860 });
  await shot('screen/s05_codigo.png', { url: doc('src/tesela/codec.rs.html') + '#560', w: 1440, h: 900 });
  await shot('screen/s06_codigo_oscuro.png', { url: doc('src/tesela/restore.rs.html') + '#150', w: 1440, h: 900, before: dark });
  await shot('screen/s07_terminal.png', { html: terminalHtml(), w: 1200, h: 800 });
  await shot('screen/s08_hoja_calculo.png', { html: sheetHtml(), w: 1400, h: 900 });
  await shot('screen/s09_retina.png', { url: doc('tesela/index.html'), w: 960, h: 640, dpr: 2 });

  const em = 'https://raw.githubusercontent.com/hfg-gmuend/openmoji/master/color/svg';
  await shot('illus/i01_emoji.png', { html: emojiHtml(em, EMOJI.slice(0, 9), '#ffffff'), w: 1024, h: 1024 });
  await shot('illus/i02_emoji_fondo.png', { html: emojiHtml(em, EMOJI.slice(9, 18), '#1f2a44'), w: 1024, h: 1024 });
  await shot('illus/i03_paisaje.png', { html: landscapeHtml(), w: 1600, h: 1000 });
  await shot('illus/i04_logo_mosaico.png', { url: `${webUrl}/index.html`, w: 1280, h: 700, before: (p) => p.evaluate(() => {
    document.querySelector('.hero > div').style.visibility = 'hidden';
  }) });
  await browser.close();
  web.close();
})();
