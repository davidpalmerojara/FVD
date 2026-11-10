# Tesela

**Tesela** es un formato de imagen con pérdida escrito desde cero en Rust. Divide cada foto
en *teselas* de tamaño adaptativo, como un mosaico. En las 24 imágenes Kodak ocupa menos que
WebP, AVIF y JPEG XL a igual calidad, en cinco métricas distintas. Además funciona en cualquier
navegador gracias a WebAssembly.

- **Comparador**: sube tus fotos y compáralas con WebP, AVIF, JPEG XL y JPEG, todo dentro del
  navegador (`web/compare/`).
- **Librería web**: `<tesela-img src="foto.tsl">`, con decodificación en paralelo en Web Workers
  (`web/tesela.js`).
- **Banco de pruebas reproducible** con BD-rate, curvas y datos crudos (`bench/`).

## Resultados

BD-rate frente a WebP: porcentaje medio de bytes que ahorra cada formato a igual calidad
(negativo = mejor). Se midió en las 24 imágenes Kodak, con el mismo programa de métricas para
todos los formatos.

| Formato | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Decodificación |
|---|---|---|---|---|---|---|
| **Tesela** (por defecto) | **−26,7 %** | **−23,4 %** | **−17,8 %** | **−24,3 %** | **−22,8 %** | **62 ms** |
| Tesela `--tune psnr` | −7,4 % | −7,4 % | −25,8 % | −20,4 % | −25,7 % | 60 ms |
| AVIF (libavif 1.4 + aom, `speed=4`) | −24,8 % | −19,7 % | −16,4 % | −22,1 % | −17,4 % | 164 ms |
| JPEG XL (libjxl 0.12, `effort=7`) | −12,1 % | −13,4 % | +9,4 % | +4,4 % | +12,2 % | 146 ms |
| WebP (libwebp 1.6, `method=6`) | 0 % | 0 % | 0 % | 0 % | 0 % | 115 ms |

![curvas](bench/results/curves.png)

**Cómo leer estos resultados:**

- **Margen frente a AVIF.** Tesela supera a AVIF en todas las columnas con un único perfil. El
  margen es pequeño en SSIMULACRA2 (−2,5 % relativo) y mayor en alta calidad (−4,6 %), SSIM y PSNR.
- **Dónde gana y dónde no.** Las curvas muestran que **a tasas muy bajas (< 0,4 bpp) AVIF sigue
  siendo mejor** en SSIMULACRA2. Tesela gana en las calidades medias y altas, las habituales en la
  web. En PSNR a tasas muy altas, AVIF vuelve a estar ligeramente por encima del perfil por defecto.
- **Frente a JPEG XL.** Tesela gana en todas las métricas (−17 % relativo en SSIMULACRA2).
- **Calibración de la escala.** La escala de calidad imita la de JPEG: `-q 85` en Tesela se ve
  parecido a un JPEG q85. En las imágenes de la demo, a igual SSIMULACRA2, Tesela ocupa entre
  un 27 % y un 56 % menos.
- **Sin trampas en el entrenamiento.** Las probabilidades iniciales del codificador se entrenaron
  con **BSD100**, no con Kodak.
- **Alcance.** Kodak son imágenes de 768×512. Faltaría validar con fotos más grandes y con otros
  tipos de contenido (capturas de pantalla, ilustraciones).

El informe completo está en [`bench/results/REPORT.md`](bench/results/REPORT.md).

## Diseño

| Etapa | Técnica |
|---|---|
| Contenedor | Cabecera y teselas independientes de 1024×1024 que se codifican y decodifican en paralelo (rayon en nativo, Web Workers en el navegador). En imágenes de hasta 1024 px no hay ninguna pérdida. |
| Color | YCbCr. Croma 4:2:0 centrado con interpolación bilineal, o 4:4:4 a calidades muy altas. |
| Partición | Superbloques de 64×64 divididos en un quadtree hasta 4×4. Cada decisión se toma por RDO (`D + λ·R`) con la tasa estimada a partir de los contextos reales. |
| Predicción | Planar, DC y 33 direcciones angulares con filtrado de referencias. Para croma también hay **CCLM**: la croma se predice desde la luma, con parámetros deducidos de los vecinos que no se transmiten. La búsqueda de modos se hace en dos fases. |
| Transformada | DCT-II ortonormal de 4 a 64 puntos con el algoritmo rápido de Lee (O(n log n)), que salta las filas nulas. |
| Cuantización | Matriz perceptual (`1 + s·r²`), cuantización adaptativa por actividad en celdas de 32×32 y **RDOQ**. |
| Entropía | Range coder binario con modelos de doble velocidad. Los coeficientes se codifican con `cbf`, `eob` y un barrido diagonal inverso con contextos que dependen del tamaño, la frecuencia y la plantilla de vecinos. Las **probabilidades iniciales están entrenadas**, con una tabla por banda de calidad. |
| Restauración | Filtro tipo ALF. Cada bloque 4×4 se clasifica en una de 12 clases (actividad × dirección). Cada clase tiene un filtro simétrico de 15 taps que el codificador estima por mínimos cuadrados y activa por superbloque. |
| Determinismo | El decodificador WebAssembly produce **exactamente** los mismos bytes que el nativo, y lo comprueba el CI. |

Toda la sintaxis del bitstream se escribe una sola vez sobre un trait `Coder` con tres
implementaciones: codificar, decodificar y estimar coste. Así el codificador y el decodificador no
pueden desincronizarse.

```
src/rc.rs         codificador aritmético, modelos y binarizaciones
src/dct.rs        DCT rápida y SATD
src/predict.rs    predicción intra y CCLM
src/coeff.rs      codificación de coeficientes y RDOQ
src/restore.rs    filtro de restauración por clases
src/codec.rs      contenedor, teselas, codificador (RDO) y decodificador
src/init_probs.rs probabilidades iniciales entrenadas (generado)
src/wasm.rs       interfaz WebAssembly (ABI C, sin bindgen)
src/metrics.rs    PSNR, SSIM y SSIMULACRA2
web/              librería JS, comparador, demo y codecs de referencia (vendor/)
bench/            banco de pruebas
```

## Diario de experimentos

Esta es la evolución medida con el BD-rate frente a WebP en SSIMULACRA2 y PSNR-Y, sobre 8 imágenes
de ajuste. Las filas con signo (+/−) son cambios respecto a la fila anterior. También recojo lo que
no funcionó.

| Cambio | SSIMULACRA2 | PSNR-Y | Resultado |
|---|---|---|---|
| Primera versión (quadtree hasta 32, 35 modos, RDOQ) | −4 % | −24 % | Base |
| Cuantización perceptual (matriz, AQ y croma) | −29 % | −4 % | Gran salto perceptual a costa de PSNR |
| Bloques de 64×64 | ±0 | ±0 | Se mantiene: no aporta en Kodak, pero ayuda en fotos grandes |
| AQ a 16×16 en vez de 32×32 | ±0 | −3 | Descartado |
| Mezcla logística de contextos (estilo PAQ) | −0,3 | −0,7 | **Descartado**: no compensa su coste |
| Poda de la partición por velocidad | −1 a −3 | | **Descartado**: cuesta calidad |
| Filtro Wiener separable | +0,3 | +1,4 | Superado por el siguiente |
| Filtro por clases (ALF, 12 clases) | +0,4 | +2,4 | **Adoptado** |
| Perfil equilibrado (búsqueda en rejilla) | −24,7 | −17,7 | Supera a AVIF en las cuatro, por márgenes mínimos |
| Probabilidades iniciales entrenadas con BSD100 | +1,7 | +0,8 | **Adoptado**: amplía el margen sobre AVIF |

## Uso

```sh
cargo build --release
./target/release/tesela encode foto.png foto.tsl -q 85
./target/release/tesela decode foto.tsl salida.png
./target/release/tesela compare foto.png salida.png     # métricas en JSON
./target/release/tesela roundtrip foto.png -q 85        # codifica, decodifica, verifica y mide
```

Opciones de `encode`:

| Opción | Efecto |
|---|---|
| `-q 1..100` | Calidad, en una escala parecida a la de JPEG. |
| `-e 1..9` | Esfuerzo de búsqueda. |
| `--tune balanced\|perceptual\|psnr` | Perfil de ajuste. |
| `--tile N` | Tamaño de las teselas en píxeles (múltiplo de 64). |
| `--420` / `--444` | Fuerza el submuestreo de croma. |
| `--no-restore` | Desactiva el filtro de restauración. |
| `--aq`, `--cq`, `--qm`, `--qmc`, `--lambda` | Ajustes finos. |

## En la web

```html
<script type="module" src="tesela.js"></script>
<tesela-img src="foto.tsl" alt="Descripción" fallback="foto.jpg" loading="lazy"></tesela-img>
```

```js
import { decode, encode, toObjectURL } from './tesela.js';
const imageData = await decode(bytes);                 // ImageData
const tsl = await encode(imageData, { quality: 85 });  // codificar en el navegador
const url = await toObjectURL(bytes);                  // para cualquier <img>
```

- **Tamaño del módulo.** `web/tesela.wasm` pesa 360 KB e incluye el codificador, el
  decodificador y las métricas.
- **Paralelismo.** La librería reparte las teselas entre varios Web Workers.
- **Recompilar.** `web/build.sh` regenera el módulo (necesitas
  `rustup target add wasm32-unknown-unknown`).

Para probarlo en local:

```sh
python3 -m http.server -d web 8000
# http://localhost:8000/            portada
# http://localhost:8000/compare/    comparador
# http://localhost:8000/demo/       demo de <tesela-img>
```

El workflow `.github/workflows/pages.yml` publica `web/` en GitHub Pages. Hay que activarlo una
vez en *Settings → Pages → Source: GitHub Actions*.

### Comparador

Sube fotos y elige uno de dos modos:

- **Misma calidad.** Cada formato se codifica varias veces hasta rodear un objetivo de
  SSIMULACRA2. El tamaño se interpola en ese punto exacto para que ningún formato salga
  perjudicado por pasarse del objetivo.
- **Calidad fija.** Cada formato usa la calidad que le indiques.

Muestra tamaño, métricas y tiempos, y tiene un visor con cortina y zoom para comparar dos formatos
cualquiera, además de descargas de cada archivo. Los códecs de referencia son las bibliotecas
oficiales compiladas a WebAssembly ([jSquash](https://github.com/jamsinclair/jSquash), Apache-2.0,
incluidas en `web/vendor/`) y se ejecutan con un hilo por formato. Nada sale del navegador.

## Banco de pruebas

```sh
pip install -r bench/requirements.txt   # Pillow (WebP y AVIF), imagecodecs (libjxl 0.12), numpy, scipy, matplotlib
bench/fetch_kodak.sh
python3 bench/bench.py                  # todos los códecs
python3 bench/bench.py --codecs tesela --reuse --tesela-args "--aq 1.5"   # iterar solo Tesela
```

Para reentrenar las probabilidades iniciales (con imágenes que no sean las de evaluación):

```sh
./target/release/tesela train src/init_probs.rs entrenamiento/*.png
```

## Limitaciones

- **Soporte.** Ningún navegador ni sistema operativo lo soporta de forma nativa: hace falta
  `tesela.js` y WebAssembly. Mientras tanto, conviene servir un `fallback` en JPEG.
- **Funcionalidad.** Solo admite 8 bits por canal. No tiene canal alfa (se compone sobre blanco),
  modo sin pérdida, animación ni metadatos ICC o EXIF.
- **Velocidad de codificación.** Codificar es lento: unos 2 s por imagen de 0,4 MP en un hilo.
  Tesela está pensado para codificar una vez y servir muchas.
- **Alcance de las pruebas.** Las comparativas se hicieron solo con Kodak. Falta validar con fotos
  grandes y con otros tipos de contenido.
- **Licencias de los datos.** Las imágenes Kodak se usan para las pruebas y las de BSD100 solo se
  usaron para estimar probabilidades: ninguna de BSD100 se distribuye.
