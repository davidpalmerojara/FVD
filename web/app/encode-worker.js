// Worker del conversor: codifica una imagen RGBA a .tsl con el mismo camino
// que el comparador (loadEncoder: tesela-mt.wasm con varios hilos si la página
// está aislada entre orígenes; si no, tesela.wasm con uno).

import { loadEncoder } from '../tesela-threads.js';

self.onmessage = async (ev) => {
  const { id, rgba, width, height, quality, effort } = ev.data;
  try {
    const core = await loadEncoder();
    const t0 = performance.now();
    const bytes = core.encode(rgba, width, height, quality, effort, 0);
    self.postMessage({ id, bytes, ms: performance.now() - t0, threads: core.threads }, [bytes.buffer]);
  } catch (e) {
    self.postMessage({ id, error: String(e && e.message ? e.message : e) });
  }
};
