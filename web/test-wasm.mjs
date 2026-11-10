// Comprueba que el módulo WebAssembly decodifica bit a bit igual que el
// binario nativo:  node web/test-wasm.mjs archivo.tsl referencia.rgb
import fs from 'node:fs';
import { Core } from './tesela-core.js';

const [tsl, ref] = process.argv.slice(2);
const module = await WebAssembly.compile(fs.readFileSync(new URL('./tesela.wasm', import.meta.url)));
const imports = {};
for (const i of WebAssembly.Module.imports(module)) (imports[i.module] ??= {})[i.name] = () => { throw new Error(i.name); };
const core = new Core((await WebAssembly.instantiate(module, imports)).exports);

const { width, height, rgba } = core.decode(fs.readFileSync(tsl));
const rgb = fs.readFileSync(ref);
let diff = 0;
for (let i = 0; i < width * height; i++) for (let c = 0; c < 3; c++) if (rgba[i * 4 + c] !== rgb[i * 3 + c]) diff++;
console.log(`${width}×${height}: ${diff} valores distintos`);
process.exit(diff === 0 && rgb.length === width * height * 3 ? 0 : 1);
