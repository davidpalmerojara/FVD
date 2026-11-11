// Un hilo de Rust de tesela-mt.wasm (ver tesela-threads.js). Instancia el
// módulo sobre la memoria compartida y espera trabajos con Atomics.
import { threadSpawn, wasiImports } from './tesela-threads.js';

const port = globalThis.self ?? (await import('node:worker_threads')).parentPort;

const onMessage = async (ev) => {
  const { module, memory, ctl, slot, slots } = ev.data ?? ev;
  let instance;
  try {
    instance = await WebAssembly.instantiate(module, {
      env: { memory },
      wasi_snapshot_preview1: wasiImports(() => memory),
      wasi: { 'thread-spawn': threadSpawn(ctl, slots) },
    });
  } catch (e) {
    port.postMessage(String(e));
    return;
  }
  port.postMessage('ready');
  for (;;) {
    // Se lee una sola vez por vuelta: si se leyera otra vez para esperar,
    // podría dormirse con el valor «listo» y no despertar nunca.
    for (let v; (v = Atomics.load(ctl, slot)) !== 2; ) Atomics.wait(ctl, slot, v);
    const arg = Atomics.load(ctl, slot + 1);
    const tid = Atomics.load(ctl, slot + 2);
    try {
      instance.exports.wasi_thread_start(tid, arg);
    } catch (e) {
      console.error('Tesela: error en un hilo', e);
    }
    Atomics.store(ctl, slot, 0);
    Atomics.notify(ctl, ctl.length - 1);
  }
};

if (globalThis.self) self.onmessage = onMessage;
else port.once('message', (data) => onMessage({ data }));
