// Worker de Tesela: decodifica/codifica teselas fuera del hilo principal.
import { loadWasm } from './tesela-core.js';

self.onmessage = async (ev) => {
  const { id, op, args } = ev.data;
  try {
    const core = await loadWasm(args.wasmUrl);
    let result;
    let transfer = [];
    if (op === 'decodeTile') {
      result = core.decodeTile(args.header, args.payload, args.w, args.h);
      transfer = [result.buffer];
    } else if (op === 'encodeTile') {
      result = core.encodeTile(args.rgba, args.tw, args.th, args.header, args.quality, args.effort, args.tune);
      transfer = [result.buffer];
    } else if (op === 'decodeFull') {
      const d = core.decode(args.bytes);
      result = { width: d.width, height: d.height, rgba: d.rgba.slice() };
      transfer = [result.rgba.buffer];
    } else if (op === 'header') {
      result = core.header(args.rgba, args.w, args.h, args.quality, args.effort, args.tune);
    } else {
      throw new Error(`operación desconocida: ${op}`);
    }
    self.postMessage({ id, result }, transfer);
  } catch (e) {
    self.postMessage({ id, error: String(e && e.message ? e.message : e) });
  }
};
