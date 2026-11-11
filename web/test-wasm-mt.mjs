// Comprueba que tesela-mt.wasm (con hilos) codifica byte a byte igual que el
// módulo de un hilo (y que el nativo, si se da):
//   node web/test-wasm-mt.mjs imagen.rgb|.rgba ancho alto [nativo.tsl]
// TH=1,2,4 elige los números de hilos que se prueban.
import fs from 'node:fs';
import { Worker } from 'node:worker_threads';
import { Core } from './tesela-core.js';
import { loadThreadedWasm } from './tesela-threads.js';

const [rgbaPath, w, h, nativePath] = process.argv.slice(2);
let rgba = new Uint8Array(fs.readFileSync(rgbaPath));
if (rgba.length === w * h * 3) {
  const rgb = rgba;
  rgba = new Uint8Array(w * h * 4).fill(255);
  for (let i = 0; i < w * h; i++) rgba.set(rgb.subarray(3 * i, 3 * i + 3), 4 * i);
}
const here = (f) => new URL(f, import.meta.url);

// Adaptador de worker_threads a la interfaz de Web Worker que usa la librería.
const makeWorker = (url) => {
  const w = new Worker(url);
  const o = { postMessage: (m) => w.postMessage(m) };
  w.on('message', (data) => o.onmessage?.({ data }));
  w.on('error', (e) => o.onerror?.({ message: String(e) }));
  return o;
};

const single = await (async () => {
  const module = await WebAssembly.compile(fs.readFileSync(here('./tesela.wasm')));
  const imports = {};
  for (const i of WebAssembly.Module.imports(module)) (imports[i.module] ??= {})[i.name] = () => { throw new Error(i.name); };
  return new Core((await WebAssembly.instantiate(module, imports)).exports);
})();

let ok = true;
let t = performance.now();
const ref = single.encode(rgba, +w, +h, +(process.env.Q ?? 80), 4, 0);
console.log(`1 hilo:   ${ref.length} bytes, ${((performance.now() - t) / 1000).toFixed(2)} s`);
for (const threads of (process.env.TH ?? "2,4").split(",").map(Number)) {
  const mt = await loadThreadedWasm({ bytes: fs.readFileSync(here('./tesela-mt.wasm')), threads, makeWorker });
  t = performance.now();
  const out = mt.encode(rgba, +w, +h, +(process.env.Q ?? 80), 4, 0);
  const same = out.length === ref.length && out.every((v, i) => v === ref[i]);
  console.log(`${threads} hilos:  ${out.length} bytes, ${((performance.now() - t) / 1000).toFixed(2)} s, ${same ? 'idéntico' : 'DISTINTO'}`);
  ok &&= same;
  const back = mt.decode(out);
  ok &&= back.width === +w && back.height === +h;
}
if (nativePath) {
  const nat = new Uint8Array(fs.readFileSync(nativePath));
  const same = nat.length === ref.length && nat.every((v, i) => v === ref[i]);
  console.log(`nativo:   ${same ? 'idéntico' : 'DISTINTO'}`);
  ok &&= same;
}
process.exit(ok ? 0 : 1);
