//! Asignador para WebAssembly con hilos (`wasm32-wasip1-threads`).
//!
//! El `malloc` de wasi-libc tiene un único cerrojo global, y el RDO pide y
//! libera muchos búferes pequeños: con varios hilos se pasan el rato
//! esperándose. Aquí cada hilo guarda los bloques pequeños que libera en
//! listas propias por tamaño (potencias de dos de 16 B a 64 KB) y los reutiliza
//! sin cerrojo. Los bloques grandes van directos a `malloc`.
//!
//! Cada hilo del codificador llama a [`flush`] al terminar para devolver su
//! caché.
//!
//! [`reserve`] evita que la memoria crezca mientras hay hilos: en V8, cuando
//! un hilo hace crecer la memoria compartida, los demás pueden seguir viendo
//! el tamaño anterior y fallar con «memory access out of bounds» al tocar la
//! parte nueva.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ptr::null_mut;

const MIN_SHIFT: u32 = 4; // 16 B
const CLASSES: usize = 13; // hasta 64 KB
const ALIGN: usize = 16;

thread_local! {
    static FREE: [Cell<*mut u8>; CLASSES] = const { [const { Cell::new(null_mut()) }; CLASSES] };
}

#[inline]
fn class(layout: &Layout) -> Option<usize> {
    if layout.align() > ALIGN {
        return None;
    }
    let size = layout.size().max(1 << MIN_SHIFT);
    let c = (usize::BITS - (size - 1).leading_zeros() - MIN_SHIFT) as usize;
    (c < CLASSES).then_some(c)
}

#[inline]
fn class_layout(c: usize) -> Layout {
    // SAFETY: tamaño potencia de dos y alineación válida.
    unsafe { Layout::from_size_align_unchecked(1 << (c as u32 + MIN_SHIFT), ALIGN) }
}

pub struct ThreadCache;

unsafe impl GlobalAlloc for ThreadCache {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match class(&layout) {
            Some(c) => {
                let p = FREE.with(|f| {
                    let head = f[c].get();
                    if !head.is_null() {
                        // SAFETY: el primer puntero de un bloque libre es el siguiente.
                        f[c].set(unsafe { *(head as *mut *mut u8) });
                    }
                    head
                });
                if p.is_null() {
                    unsafe { System.alloc(class_layout(c)) }
                } else {
                    p
                }
            }
            None => unsafe { System.alloc(layout) },
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        match class(&layout) {
            Some(c) => FREE.with(|f| {
                // SAFETY: el bloque es nuestro y mide al menos 16 bytes.
                unsafe { *(ptr as *mut *mut u8) = f[c].get() };
                f[c].set(ptr);
            }),
            None => unsafe { System.dealloc(ptr, layout) },
        }
    }
}

/// Devuelve a `malloc` los bloques guardados por el hilo actual.
pub fn flush() {
    FREE.with(|f| {
        for (c, head) in f.iter().enumerate() {
            let mut p = head.replace(null_mut());
            while !p.is_null() {
                // SAFETY: lista enlazada de bloques libres de la clase c.
                let next = unsafe { *(p as *mut *mut u8) };
                unsafe { System.dealloc(p, class_layout(c)) };
                p = next;
            }
        }
    });
}

/// Antes de crear los hilos de un plano: pide y libera un bloque del tamaño
/// que pueden llegar a necesitar entre todos. El `malloc` de wasi-libc no
/// devuelve memoria al sistema, así que ese espacio queda libre y la memoria
/// no tiene que crecer mientras trabajan. Ahora no hay más hilos, así que
/// crecer es seguro. Estimación holgada: fija, por hilo (copia del plano,
/// búferes del RDO y caché) y por superbloque (árbol y probabilidades).
pub fn reserve(threads: usize, plane_px: usize, superblocks: usize) {
    let bytes = (16 << 20)
        + threads * ((6 << 20) + 3 * plane_px)
        + superblocks * (128 << 10)
        + 4 * plane_px;
    // SAFETY: tamaño no nulo y alineación válida.
    unsafe {
        let layout = Layout::from_size_align_unchecked(bytes, ALIGN);
        let p = System.alloc(layout);
        if !p.is_null() {
            System.dealloc(p, layout);
        }
    }
}
