// Worker del conversor: codifica a .tsl con el mismo camino que el comparador
// (loadEncoder: tesela-mt.wasm con varios hilos si la página está aislada
// entre orígenes; si no, tesela.wasm con uno).
//
// Mensajes: { id, rgba, width, height, quality, effort, lossless } para una
// imagen, o { id, frames: [rgba…], durations, loops, … } para una animación.

import { loadEncoder } from '../tesela-threads.js';

self.onmessage = async (ev) => {
  const { id, rgba, frames, durations, loops, width, height, quality, effort, lossless } = ev.data;
  try {
    const core = await loadEncoder();
    const t0 = performance.now();
    let bytes;
    if (frames) {
      bytes = core.encodeAnimation(frames, durations, width, height, { loops, quality, effort, lossless });
    } else if (lossless) {
      bytes = core.encodeEx(rgba, width, height, quality, effort, 0, { lossless: true });
    } else {
      bytes = core.encode(rgba, width, height, quality, effort, 0);
    }
    self.postMessage({ id, bytes, ms: performance.now() - t0, threads: core.threads }, [bytes.buffer]);
  } catch (e) {
    self.postMessage({ id, error: String(e && e.message ? e.message : e) });
  }
};
