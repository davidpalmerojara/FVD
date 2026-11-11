// Tesela con hilos en WebAssembly (tesela-mt.wasm, compilado para
// wasm32-wasip1-threads). El codificador reparte los superbloques entre
// varios hilos igual que en nativo, con el mismo resultado byte a byte.
//
// Cada hilo de Rust (std::thread) corre en un Web Worker con su propia
// instancia del módulo y la misma memoria compartida. Los workers se crean
// antes de codificar y esperan con Atomics: mientras se codifica, el hilo que
// llama está bloqueado y no podría arrancar workers nuevos.
//
// Requiere aislamiento entre orígenes (crossOriginIsolated) para poder usar
// SharedArrayBuffer. Si no lo hay, `loadEncoder` usa el módulo de un hilo.

import { Core, loadWasm } from './tesela-core.js';

const MT_URL = new URL('./tesela-mt.wasm', import.meta.url);
const THREAD_URL = new URL('./tesela-thread-worker.js', import.meta.url);
const MAX_PAGES = 32768; // 2 GB, como en .cargo/config.toml
const SLOT = 4; // por hilo: [estado, argumento, id, libre]
const IDLE = 0;
const TAKEN = 1;
const READY = 2;

/** Funciones WASI mínimas que usa la biblioteca estándar de Rust. */
export function wasiImports(getMemory) {
  const dv = () => new DataView(getMemory().buffer);
  const text = new TextDecoder();
  return {
    fd_write(fd, iovs, n, nwritten) {
      const d = dv();
      let total = 0;
      let s = '';
      for (let i = 0; i < n; i++) {
        const ptr = d.getUint32(iovs + 8 * i, true);
        const len = d.getUint32(iovs + 8 * i + 4, true);
        s += text.decode(new Uint8Array(getMemory().buffer, ptr, len).slice());
        total += len;
      }
      d.setUint32(nwritten, total, true);
      if (s.trim()) console.warn('[tesela]', s.trimEnd());
      return 0;
    },
    // Ceros: el resultado no puede depender del azar (semillas de HashMap).
    random_get(buf, len) {
      new Uint8Array(getMemory().buffer, buf, len).fill(0);
      return 0;
    },
    environ_sizes_get(count, size) {
      dv().setUint32(count, 0, true);
      dv().setUint32(size, 0, true);
      return 0;
    },
    environ_get: () => 0,
    clock_time_get(_id, _precision, out) {
      dv().setBigUint64(out, BigInt(Math.round(performance.now() * 1e6)), true);
      return 0;
    },
    sched_yield: () => 0,
    proc_exit(code) {
      throw new Error(`Tesela terminó con código ${code}`);
    },
  };
}

/** `wasi.thread-spawn`: entrega el hilo a un worker libre del grupo. */
export function threadSpawn(ctl, slots) {
  return (arg) => {
    const tid = Atomics.add(ctl, 0, 1) + 1;
    const deadline = Date.now() + 10000;
    for (;;) {
      for (let i = 0; i < slots; i++) {
        const b = 1 + i * SLOT;
        if (Atomics.compareExchange(ctl, b, IDLE, TAKEN) === IDLE) {
          Atomics.store(ctl, b + 1, arg);
          Atomics.store(ctl, b + 2, tid);
          Atomics.store(ctl, b, READY);
          Atomics.notify(ctl, b);
          return tid;
        }
      }
      // Todos ocupados: un hilo anterior está terminando. Espera un poco.
      if (Date.now() > deadline) return -1;
      Atomics.wait(ctl, ctl.length - 1, 0, 1);
    }
  };
}

/**
 * Worker creado desde un `blob:` que importa el script real. Un worker con
 * URL `blob:` hereda la política de aislamiento de quien lo crea; uno con la
 * URL del script necesitaría que su respuesta trajera la cabecera COEP, y eso
 * falla cuando la añade un service worker (GitHub Pages): la petición de un
 * worker creado dentro de otro worker no siempre pasa por él.
 */
function blobWorker(url) {
  const src = `import ${JSON.stringify(String(url))};`;
  return new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })), { type: 'module' });
}

/**
 * Carga tesela-mt.wasm con `threads` hilos de trabajo. `makeWorker(url)`
 * permite usarlo fuera del navegador (en las pruebas con Node).
 */
export async function loadThreadedWasm({ url = MT_URL, bytes, threads, makeWorker } = {}) {
  threads = threads ?? Math.max(2, Math.min(8, globalThis.navigator?.hardwareConcurrency || 4));
  makeWorker = makeWorker ?? blobWorker;
  const module = await WebAssembly.compile(bytes ?? (await (await fetch(url)).arrayBuffer()));
  const memory = new WebAssembly.Memory({ initial: 64, maximum: MAX_PAGES, shared: true });
  // ctl[0]: último id de hilo; luego un hueco por worker; el último entero, para esperas.
  const slots = threads + 2;
  const ctl = new Int32Array(new SharedArrayBuffer(4 * (2 + slots * SLOT)));
  const imports = {
    env: { memory },
    wasi_snapshot_preview1: wasiImports(() => memory),
    wasi: { 'thread-spawn': threadSpawn(ctl, slots) },
  };
  // La instancia principal primero: inicializa la memoria compartida.
  const instance = await WebAssembly.instantiate(module, imports);
  // Lo que haría `_initialize` de wasi-libc: estructura del hilo principal y constructores.
  instance.exports.__wasi_init_tp();
  instance.exports.__wasm_call_ctors();
  await Promise.all(
    Array.from({ length: slots }, (_, i) =>
      new Promise((resolve, reject) => {
        const w = makeWorker(THREAD_URL);
        w.onmessage = (ev) => (ev.data === 'ready' ? resolve() : reject(new Error(ev.data)));
        w.onerror = (ev) => reject(new Error(ev.message || 'error al crear un hilo de Tesela'));
        w.postMessage({ module, memory, ctl, slot: 1 + i * SLOT, slots });
      }),
    ),
  );
  const core = new Core(instance.exports);
  instance.exports.tsl_set_threads(threads);
  core.threads = threads;
  return core;
}

let encoderPromise = null;

/**
 * Módulo para codificar: con hilos si la página lo permite, y si no (o si
 * falla al cargar), el normal de un hilo.
 */
export function loadEncoder() {
  if (!encoderPromise) {
    encoderPromise = (async () => {
      if (globalThis.crossOriginIsolated && typeof SharedArrayBuffer !== 'undefined') {
        try {
          return await loadThreadedWasm();
        } catch (e) {
          console.warn('Tesela: sin hilos en WebAssembly, se codifica con uno.', e);
        }
      }
      const core = await loadWasm();
      core.threads = 1;
      return core;
    })();
  }
  return encoderPromise;
}
