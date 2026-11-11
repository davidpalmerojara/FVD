// Vectores de conformidad v1 decodificados con el módulo WebAssembly:
//   node web/test-vectors.mjs
// Comprueba que la salida RGBA coincide con el hash de tests/vectors/manifest.txt.
import fs from 'node:fs';
import { Core } from './tesela-core.js';

const root = new URL('../tests/vectors/', import.meta.url);
const module = await WebAssembly.compile(fs.readFileSync(new URL('./tesela.wasm', import.meta.url)));
const imports = {};
for (const i of WebAssembly.Module.imports(module)) (imports[i.module] ??= {})[i.name] = () => { throw new Error(i.name); };
const core = new Core((await WebAssembly.instantiate(module, imports)).exports);

const fnv64 = (data) => {
  let h = 0xcbf29ce484222325n;
  for (const b of data) h = BigInt.asUintN(64, (h ^ BigInt(b)) * 0x100000001b3n);
  return h.toString(16).padStart(16, '0');
};

let fails = 0;
const lines = fs.readFileSync(new URL('manifest.txt', root), 'utf8').split('\n').filter((l) => l && !l.startsWith('#'));
for (const line of lines) {
  const [name, w, h, hash] = line.split(/\s+/);
  const d = core.decode(fs.readFileSync(new URL(name, root)));
  const ok = d.width === +w && d.height === +h && fnv64(d.rgba) === hash;
  if (!ok) fails++;
  console.log(`${ok ? 'ok   ' : 'FALLO'} ${name}`);
}
console.log(`${lines.length - fails}/${lines.length} vectores correctos`);
process.exit(fails ? 1 : 0);
