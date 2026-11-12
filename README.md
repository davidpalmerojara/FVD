# Tesela

**Tesela** es un formato de imagen escrito desde cero en Rust. Divide cada imagen en *teselas*
de tamaño adaptativo, como un mosaico. A igual calidad ocupa menos que WebP, AVIF y JPEG XL en todos los
tipos de contenido probados: fotos pequeñas y grandes, capturas de pantalla e ilustraciones. En
fotos decodifica de 2 a 5 veces más rápido que AVIF, y funciona en cualquier navegador gracias a
WebAssembly.

Admite canal alfa, modo sin pérdida y una vista previa progresiva opcional.

- **Comparador**: sube tus fotos y compáralas con WebP, AVIF, JPEG XL y JPEG, todo dentro del
  navegador (`web/compare/`).
- **Librería web**: `<tesela-img src="foto.tsl">`, con decodificación en paralelo en Web Workers
  (`web/tesela.js`).
- **Banco de pruebas reproducible** con BD-rate, curvas y datos crudos (`bench/`).

## Formato

La versión 1 del flujo de bits está **congelada**:

- [`docs/especificacion-v1.md`](docs/especificacion-v1.md) describe el contenedor, la sintaxis y
  la reconstrucción con el detalle necesario para escribir otro decodificador.
- [`tests/vectors/`](tests/vectors) contiene 14 archivos de conformidad con el hash de su salida,
  que se comprueban en nativo y en WebAssembly.
- Cada archivo lleva su número de versión: las mejoras futuras que cambien el flujo de bits irán
  en una versión nueva, y un `.tsl` v1 seguirá abriéndose.

La **versión 2** está en desarrollo y es la que escribe el codificador por defecto (`--v1` escribe
la v1). Añade particiones rectangulares y tablas de probabilidad nuevas. Sus diferencias con la v1
están en [`docs/especificacion-v2-borrador.md`](docs/especificacion-v2-borrador.md). Hasta que se
congele, puede cambiar.

## Pruébalo en tu ordenador

**Comparador con tus fotos.** Solo necesitas Git y Python 3; no hace falta Rust.

```sh
git clone -b ccr-17dbeae9-yi88ht https://github.com/davidpalmerojara/FVD.git tesela
cd tesela
python web/serve.py        # en macOS/Linux/WSL puede ser python3
```

Abre <http://localhost:8000/compare/> y arrastra tus fotos. Se procesan en tu navegador; nada sale
de tu ordenador. Tiene que ser a través de `http://`: abriendo el archivo directamente
(`file://`), el navegador bloquea WebAssembly y los workers.

**Registro.** Con `web/serve.py`, cada foto que termina el comparador se guarda en `registro/`
(el original, la imagen probada tras reducirla y los resultados de cada formato). Se consulta en
<http://localhost:8000/registro/>, con un filtro para ver solo dónde Tesela no gana, y se resume o
se repite con los códecs nativos (todas las métricas, foto a foto):

```sh
python bench/registro.py                   # resumen de lo probado en el navegador
python bench/registro.py --banco           # repite las imágenes con los códecs nativos
```

`registro/` no se sube a Git (son tus fotos). Para compartirlo: `git add -f registro` y un commit,
sabiendo que si el repositorio es público, las fotos también lo serán; o comprímelo y envíalo.

- **Windows (PowerShell).** Se instala con `winget install Git.Git Python.Python.3.12`.
  Si `python` abre la Microsoft Store, usa `py web/serve.py`. Usa `web/serve.py` y no
  `python -m http.server`: en algunos Windows este último sirve los `.js` con un tipo incorrecto
  y el comparador no carga.
- **WSL.** Los mismos comandos que en Linux. Clona dentro de WSL (`~/tesela`), no en `/mnt/c`, y
  abre `http://localhost:8000/compare/` en el navegador de Windows: WSL 2 redirige localhost.

**Línea de comandos.** Necesita [Rust](https://rustup.rs) y acepta PNG y JPEG.

```sh
cargo build --release
./target/release/tesela encode foto.jpg foto.tsl -q 85      # comprimir
./target/release/tesela decode foto.tsl foto_decodificada.png
./target/release/tesela roundtrip foto.jpg -q 85            # tamaño y métricas en JSON
```

En PowerShell el ejecutable es `.\target\release\tesela.exe`. Compilar en Windows nativo
requiere las *Build Tools* de Visual Studio, que `rustup` ofrece instalar; en WSL basta con
`rustup` y `sudo apt install build-essential`.

## Resultados

**Qué se mide.** El BD-rate frente a WebP: el porcentaje medio de bytes que ahorra cada formato a
igual calidad (negativo = mejor). Todas las salidas se miden con el mismo programa de métricas.

**Datos de entrenamiento.** Las probabilidades iniciales de Tesela se entrenaron con BSD100 y con
imágenes sintéticas propias, nunca con las imágenes de evaluación.

### Por tipo de contenido (SSIMULACRA2, calidad perceptual)

| Conjunto | Tesela | AVIF | JPEG XL | Decodificación Tesela / AVIF |
|---|---|---|---|---|
| **CLIC 2020**: 41 fotos profesionales de 2,2 MP (conjunto de referencia en investigación) | **−35,8 %** | −29,6 % | −22,2 % | 0,49 s / 1,46 s |
| Kodak: 24 fotos de 768×512 | **−30,6 %** | −26,3 % | −12,1 % | 81 ms / 256 ms |
| **DIV2K**: 50 fotos de validación de 2,7 MP ([informe](bench/results/div2k-50/REPORT.md)) | **−34,9 %** | −34,4 % | −20,3 % | 0,63 s / 1,88 s |
| Capturas de pantalla: 9 (código, documentación, terminal, hoja de cálculo, webs) | **−60,1 %** | −54,3 % | −36,0 % | 0,14 s / 0,15 s |
| Ilustraciones: 6 (emojis, vectorial, gráficos) | **−61,8 %** | −56,0 % | +9,5 % | 0,13 s / 0,10 s |

AVIF con croma completa (4:4:4): en todos los conjuntos ocupa menos que con 4:2:0, de un 2 a un 7 %
a calidad media y bastante más a calidad alta, así que es la comparación justa. Tesela elige 4:2:0
o 4:4:4 por imagen. Con AVIF en 4:2:0, la ventaja de Tesela parecía mayor (en DIV2K, −34,0 frente a
−32,5 %; ahora −34,9 frente a −34,4 %, prácticamente un empate). Los tiempos son los del banco, que
codifica y decodifica cuatro imágenes a la vez.

### Kodak, todas las métricas

| Formato | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Codificación | Decodificación |
|---|---|---|---|---|---|---|---|
| **Tesela** (por defecto) | **−30,6 %** | **−26,4 %** | −19,0 % | **−26,0 %** | −27,4 % | 2,41 s | **81 ms** |
| Tesela `--tune psnr` | −15,7 % | −13,2 % | **−28,0 %** | −23,3 % | **−30,3 %** | 3,04 s | 88 ms |
| AVIF (libavif 1.4 + aom, `speed=4`, 4:4:4) | −26,3 % | −22,4 % | −12,9 % | −19,3 % | −20,5 % | 2,50 s | 256 ms |
| JPEG XL (libjxl 0.12, `effort=7`) | −12,1 % | −13,4 % | +9,4 % | +4,4 % | +12,2 % | 0,19 s | 146 ms |
| WebP (libwebp 1.6, `method=6`) | 0 % | 0 % | 0 % | 0 % | 0 % | 0,13 s | 115 ms |

![curvas](bench/results/curves.png)

**Cómo leer estos resultados:**

- **Fotos.** En SSIMULACRA2, Tesela supera a AVIF por 4 puntos en Kodak y 6 en CLIC; en las fotos
  grandes de DIV2K empatan (0,5 puntos). En PSNR-Y (solo luma) la ventaja es menor, porque al
  elegir 4:4:4 Tesela dedica más bits al color; en PSNR RGB, mayor.
- **Contenido sintético.** Tesela supera a AVIF por 6 puntos en capturas e ilustraciones. Lo
  consigue con la paleta, la copia intra de bloques, el transform skip y la elección automática
  de 4:4:4.
- **Velocidad.** En fotos, Tesela decodifica de 2 a 5 veces más rápido que AVIF; en contenido
  sintético, más o menos igual. Codificar cuesta más CPU que en la v1 (la búsqueda de particiones
  rectangulares), pero el codificador usa todos los núcleos: una foto de Kodak tarda ~1 s con 4.
  La columna de codificación del banco mide una imagen por núcleo, no ese caso.
- **Calibración de la escala.** La escala de calidad imita la de JPEG: `-q 85` se ve parecido a un
  JPEG q85, y en las imágenes de la demo Tesela ocupa entre un 27 % y un 55 % menos.

Informes completos, con curvas y datos crudos:

- [Kodak](bench/results/REPORT.md)
- [fotos grandes](bench/results/fotos-grandes/REPORT.md)
- [capturas](bench/results/capturas/REPORT.md)
- [ilustraciones](bench/results/ilustraciones/REPORT.md)

### Sin pérdida

Tamaño total de 10 imágenes (3 fotos, 4 capturas y 3 ilustraciones):

| PNG optimizado | JPEG XL | WebP | Tesela |
|---|---|---|---|
| 3033 KB | 1960 KB | **1722 KB** | 2054 KB |

- **En capturas e ilustraciones** el modo sin pérdida de Tesela empata con WebP.
- **En fotos pierde:** ocupa un 21 % más que WebP y un 32 % más que JPEG XL, porque no tiene un
  predictor por píxel. Es la principal mejora pendiente de este modo.

## Diseño

| Etapa | Técnica |
|---|---|
| Contenedor | Cabecera y teselas independientes de 1024×1024 que se codifican y decodifican en paralelo (rayon en nativo, Web Workers en el navegador); dentro de cada tesela, el codificador decide los superbloques en frente de onda con varios hilos. En imágenes de hasta 1024 px no hay ninguna pérdida por teselas. |
| Color | YCbCr. Croma 4:2:0 centrado con interpolación bilineal, o 4:4:4 cuando el submuestreo borraría detalle de color que la calidad pedida conserva (el codificador lo mide en cada imagen). |
| Partición | Superbloques de 64×64 divididos en un quadtree hasta 4×4. En la v2, cada nodo que no se divide en cuatro puede partirse en dos mitades rectangulares (horizontales o verticales). Cada decisión se toma por RDO (`D + λ·R`) con la tasa estimada a partir de los contextos reales. |
| Predicción | Planar, DC y 33 direcciones angulares con filtrado de referencias. Para croma también hay **CCLM**: la croma se predice desde la luma, con parámetros deducidos de los vecinos que no se transmiten. La búsqueda de modos se hace en dos fases. |
| Transformada | DCT-II ortonormal de 4 a 64 puntos: por producto de matrices en 4 y 8, y con el algoritmo rápido de Lee (O(n log n)) desde 16. Si el bloque solo tiene DC, se usa un atajo. |
| Cuantización | Matriz perceptual (`1 + s·r²`), cuantización adaptativa por actividad en celdas de 32×32 y **RDOQ**. |
| Entropía | Range coder binario con modelos de doble velocidad. Los coeficientes se codifican con `cbf`, `eob` y un barrido diagonal inverso con contextos que dependen del tamaño, la frecuencia y la plantilla de vecinos. Las **probabilidades iniciales están entrenadas**, con tablas por tipo de contenido (foto o sintético) y por banda de calidad. |
| Restauración | Filtro tipo ALF. Cada bloque 4×4 se clasifica en una de 12 clases (actividad × dirección). Cada clase tiene un filtro simétrico de 15 taps que el codificador estima por mínimos cuadrados y activa por superbloque. |
| Contenido sintético | Si la imagen parece sintética (pocos valores por bloque), el codificador activa tres herramientas, que en fotos no cuestan ni un bit. **Paleta**: hasta 8 colores, con índices codificados por rango según los vecinos. **Copia intra de bloques**: el codificador busca candidatos con un índice hash de bloques de 8×8. **Transform skip** en bloques de hasta 16×16. Además, el croma pasa a 4:4:4. |
| Alfa y sin pérdida | El alfa es un plano adicional sin pérdida con contextos propios. El modo sin pérdida codifica los planos G, B y R con residuo exacto, y predice B y R desde G con CCLM. |
| Animación (v2) | Lienzo y lista de fotogramas, cada uno con su duración y el rectángulo que cambia. Cada bloque puede predecirse del fotograma anterior con un vector de movimiento de cuarto de píxel o con el movimiento global del fotograma (afín, para paneos y zooms), respecto al anterior o a una referencia larga (el fotograma clave). `tesela encode` acepta GIF, APNG y WebP animados; `tesela decode` escribe un GIF (`salida.gif`) o un PNG por fotograma (`carpeta/`). En la web, `<tesela-img>` la reproduce y el Service Worker la entrega como APNG, así que `<img src="anim.tsl">` se anima. |
| Metadatos (v2) | Perfil de color ICC (por ejemplo, Display P3 de las fotos de móvil) y EXIF. Al codificar se aplica la orientación EXIF a los píxeles. `tesela decode` escribe el perfil en el PNG, y el Service Worker lo incluye en el PNG que entrega al navegador, que gestiona el color como con cualquier imagen. `--sin-metadatos` no los guarda. |
| Vista previa | Opcional (`--preview`): una miniatura de ~300 bytes al principio del archivo. `<tesela-img>` la muestra desenfocada en cuanto llega, mientras se descarga el resto. |
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
src/tools.rs      paleta, copia intra de bloques y detección de contenido sintético
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

En la validación con más tipos de contenido apareció el punto débil: en capturas, Tesela se
quedaba en −26 % frente al −51 % de AVIF. Estos cambios se midieron en las 9 capturas:

| Cambio en capturas | SSIMULACRA2 | Resultado |
|---|---|---|
| Croma 4:4:4 | −26 → −32 % | Se activa solo en contenido sintético |
| Paleta, copia de bloques y transform skip (sin entrenar) | → −55,8 % | **Adoptado**: supera a AVIF |
| Tablas entrenadas por tipo de contenido | → −57,9 % | **Adoptado** |

El análisis por herramienta muestra que la copia de bloques aporta casi todo: una hoja de cálculo
pasa de 110 KB a 31 KB. La paleta solo aporta ~1,5 %.

**Velocidad (perfilado con callgrind):**

- **Decodificación:** −28 % de instrucciones. Antes se ponían a cero búferes de 64×64 en cada
  bloque, incluso en los de 4×4. Además, el filtro de restauración se rehízo con un búfer con
  borde.
- **Codificación:** −40 % de tiempo con la SATD desenrollada, la DCT por matriz y la poda de
  candidatos. Ninguna de estas optimizaciones cambió la compresión.
- **SIMD en WebAssembly:** probado y **descartado**; no mejoraba nada.

**Versión 2: particiones rectangulares** (Kodak-8, BD-rate frente a WebP en SSIMULACRA2):

| Cambio | SSIMULACRA2 | Resultado |
|---|---|---|
| Base (solo quadtree) | −28,4 % | |
| Mitades rectangulares 2:1 en todos los tamaños, con RDO | −29,7 % | Mejora clara |
| Sin rectángulos en los nodos de 64×64 (poda A) | −29,9 % | **Adoptado**: igual o mejor y con menos búsqueda |
| Transformadas múltiples (estilo MTS) | sin ganancia neta | **Descartado** |
| Tablas de probabilidad entrenadas con DIV2K | sin ganancia neta | **Descartado**; se siguen usando BSD100 e imágenes sintéticas |
| Tablas v2 entrenadas (incluyen los contextos nuevos) | −0,2 a −0,3 % en recortes de validación | **Adoptado** |
| Cuantización dependiente (trellis de 4 estados, como en VVC), con tablas reentrenadas | Kodak −29,8 % (igual); recortes: +0,8 % en calidad baja, −0,8 % en alta | **Descartado**: neutro de media y un 25 % más lento. Nuestro RDOQ ya era ajustado y SSIMULACRA2 no es error cuadrático |
| RDO psicovisual (penalizar la diferencia de energía AC, como el `psy-rd` de x264) | de +2 % (débil) a +32 % (fuerte) en calidad baja | **Descartado**: SSIMULACRA2 mide estructura, y forzar textura la empeora |

**Croma 4:4:4 adaptativa.** Las fotos de un usuario con detalle fino de color (fachadas, tejidos,
vegetación) se quedaban en SSIMULACRA2 ~80 con 4:2:0, y ahí JPEG XL ganaba. Ahora el codificador
mide cuánto error añadiría el submuestreo y, si es grande frente al paso de cuantización, guarda la
croma entera (sin cambiar el formato: el campo ya existía):

| Cambio | Resultado |
|---|---|
| 4:4:4 cuando el error del submuestreo supera 0,25 × el paso | Fotos del usuario: Tesela pasa a ganar a JPEG XL en 20 de 21 (−11 % de media) |
| En 4:4:4, croma más gruesa (factor 1,2 en vez de 0,8; matriz 4,5 en vez de 3,0), ajustada en DIV2K 851-900 | 29 fotos de ajuste −4,7 %; fotos del usuario −4,3 % (ninguna empeora); Kodak, CLIC y DIV2K-50 −0,2 a −0,3 % |
| Forzar 4:4:4 en animación | de −2 a +12 %. **Descartado** |**Animación.** Tamaño total a igual SSIMULACRA2 medio por fotograma
([`bench/results/animacion`](bench/results/animacion/REPORT.md); `python3 bench/anim_bench.py`):

| Animación | GIF | WebP animado | Tesela | AVIF animado |
|---|---|---|---|---|
| Terminal, 90 fotogramas de 640×400, a calidad 90 | 242 KB | no llega (máx. 76) | **6,9 KB** | 14,2 KB |
| Interfaz, 40 de 480×320, a calidad 90 | 68 KB | no llega (máx. 84) | **3,6 KB** | 5,4 KB |
| Paneo sobre una foto, 30 de 400×300, a calidad 80 | 1 968 KB | 454 KB | **16,0 KB** | 16,8 KB |
| Zoom sobre una foto, 30 de 400×300, a calidad 70 | 1 917 KB | 279 KB | **8,7 KB** | 9,8 KB |
| El mismo zoom durante 90 fotogramas, a calidad 75 | 5 515 KB | 910 KB | 22,2 KB | **18,2 KB** |
| Objetos que se mueven (y uno gira) sobre un fondo quieto, 40 de 400×300, a calidad 75 | 2 618 KB | 338 KB | 40,3 KB | **37,2 KB** |

AVIF animado va con croma completa (4:4:4), que en estas animaciones le ahorra hasta un 28 %. Tesela
ocupa entre 15 y 100 veces menos que GIF y WebP animado. Frente a AVIF gana en la terminal (−51 %),
en la interfaz (−34 %), en el zoom (−12 %; −4 % a calidad 78) y en el paneo (−5 % a calidad 80,
−11 % a 78; a 82 empatan). Pierde en el zoom largo de 90 fotogramas (AVIF ocupa un 18 % menos) y
con objetos que se mueven y giran (un 8 % menos): AVIF codifica fotogramas futuros ocultos, predice
en las dos direcciones y busca movimiento por bloques con más precisión. Forzar 4:4:4 en Tesela no
ayuda en estas animaciones (de −2 a +12 %).

Así se llegó ahí (tamaño a igual calidad; terminal / interfaz / paneo / zoom):

| Cambio | Resultado |
|---|---|
| Solo el rectángulo que cambia, cada fotograma como imagen fija | 19,2 / 21,9 / 310 / 56 KB (q85) |
| Predicción desde el fotograma anterior (vectores enteros, hash y búsqueda por pasos) | 13,7 / 7,2 / 38 KB (q85) |
| Vectores de cuarto de píxel (filtro de 8 coeficientes de HEVC) y predictor con los vecinos | zoom: 56 → 30 KB (q85) |
| La referencia son los planos decodificados, no el RGB reconvertido en cada fotograma | la calidad ya no cae de 80 a 73 a lo largo del paneo |
| El codificador ignora el relleno hasta el superbloque (~12 % de cada fotograma) | fotogramas predichos: −33 % |
| Movimiento global afín por fotograma, a 1/16 de píxel por bloque de 4 × 4 | zoom: predicción +5,4 dB; paneo: copia exacta |
| Sin restauración en los fotogramas predichos; tabla de fotogramas compacta (LEB128, cabeceras abreviadas) | −25 % por fotograma; −38 bytes por fotograma |
| Referencia larga (fotograma dorado): el movimiento global predice del fotograma clave, sin encadenar interpolaciones | zoom: 16,2 → 10,9 KB (en 29 fotogramas sin residuo, 30 → 39 dB) |
| La referencia larga renovada se codifica a la calidad pedida si la anterior duró ≥ 8 fotogramas | zoom de 90 fotogramas: el último pasa de 56,6 a 66,2 de SSIMULACRA2, mismo tamaño |
| Filtro Lanczos-4 para el movimiento global (en lugar del seno con ventana coseno²) | neutro: con la referencia larga ya no se encadenan interpolaciones. **Descartado** |
| Mapa de cuantización adaptativa deducido de la referencia (el decodificador lo calcula; antes eran 55 de ~95 bytes de un fotograma que solo copia) | −13 a −22 % |
| Movimiento global en la tabla de fotogramas, predicho por extrapolación (0-6 bytes en vez de 15) | −1 a −2 % |
| Media de dos predicciones pasadas (anterior y referencia larga) | +0,1 dB en simulación. **Descartado** |
| Rectángulo de cada fotograma con 32 píxeles de margen, alineado a 16 | arregla rayas que se acumulaban donde un objeto descubre el fondo: objetos 48 → 71 de SSIMULACRA2; terminal +7 a +10 de calidad (el texto nuevo caía en el borde de una imagen aparte) |
| Croma: un bit para heredar el vector de la luma de su posición | −2 a −3 % con movimiento local |
| Sin movimiento de cámara, copia sin mover de la referencia larga (que solo se renueva con un cambio de escena) | el fondo que vuelve a verse al apartarse un objeto se copia: objetos −6 % y +2,3 de calidad |
| Calidad por celda según lo bien que predice el fotograma anterior (AQ temporal) | peor que el −15 global en las tres animaciones de foto. **Descartado** |
| Los fotogramas predichos empiezan con los contextos del anterior (como los CDF de AV1), no con las probabilidades iniciales | −7 a −14 %; el zoom largo pasa de un 30 % a un 9 % (frente a AVIF 4:2:0) por detrás de AVIF |
| Total frente a la versión anterior, a igual calidad | terminal ≥ −55 %, interfaz −48 %, paneo −51 %, zoom −68 % |

Velocidad: codificar un fotograma predicho costaba 2,6 veces una imagen fija del mismo tamaño. La
interpolación se rehízo sin recortes por píxel ni búferes nuevos por bloque (idéntica bit a bit) y,
si copiar del fotograma anterior ya es exacto, no se prueba la predicción intra: animaciones ~2 veces
más rápidas por un ~1 % de tamaño.

Dos errores que destaparon los tests: el predictor de vectores usaba el vecino superior derecho,
que el codificador veía pero el decodificador aún no; y al convertir la referencia de RGB a YCbCr
en cada fotograma la imagen se degradaba poco a poco (en el paneo, de 80 a 73 de SSIMULACRA2).
Desde entonces dos tests exigen que el decodificador reproduzca byte a byte lo que el codificador
cree haber producido, uno con desplazamiento y otro con zoom.

**Modos rápidos.** Los niveles de esfuerzo bajos recortan la búsqueda del codificador: sin
particiones rectangulares (3 y menos), sin refinado de los modos angulares y con parada temprana
de la división (2 y menos), y sin bloques de 4×4 (1). Kodak-8, BD-rate frente a WebP y tiempo por
imagen en el mismo banco (4 trabajos a la vez):

| Modo | SSIMULACRA2 | PSNR-Y | Tiempo | Foto de 12 MP, 4 núcleos |
|---|---|---|---|---|
| `-e 4` (por defecto) | −29,9 % | −21,4 % | 2,04 s | 9,2 s |
| `-e 3` | −28,1 % | −19,2 % | 0,76 s | |
| `-e 2` | −25,7 % | −16,4 % | 0,55 s | 2,8 s |
| `-e 1` | −24,0 % | −11,2 % | 0,40 s | 2,2 s |
| AVIF (aom, speed 4, 4:2:0), como referencia | −24,1 % | −17,3 % | 1,93 s | |

Con `-e 3` Tesela comprime más que AVIF y codifica 2,5 veces más rápido; con `-e 1`, empata con
AVIF en SSIMULACRA2 y es 5 veces más rápido.

**Codificador multihilo.** Dentro de cada tesela, los superbloques de 64×64 se deciden en
*frente de onda*: el (x, y) puede empezar en cuanto están el (x−1, y) y el (x+1, y−1). Para que
el resultado no dependa del número de hilos, cada superbloque solo ve lo que es seguro que ya está
decidido, y estima el coste con las probabilidades de su propia fila (cada fila arranca con las de
la anterior tras su segundo superbloque, como el WPP de HEVC). La escritura sigue siendo
secuencial y con los contextos reales, así que el formato y el decodificador no cambian.

| Paso | kodim05, 4 núcleos | Notas |
|---|---|---|
| Un hilo | 3,19 s | Base |
| Contextos de la fila y−2 | 2,26 s | Solo 2 superbloques a la vez: cada fila esperaba a que se escribiera entera la de dos más arriba |
| Contextos por fila (WPP) y reparto por diagonales | 1,19 s | Los hilos dejaron de esperarse entre sí en la misma fila |
| Cada hilo copia solo la ventana que lee | 1,04 s | Antes clonaba el plano entero por superbloque (15 MB en una foto de 12 MP) |

El coste en compresión es mínimo (−29,8 % frente a −29,9 % del codificador secuencial).

**Hilos en el navegador.** El módulo con hilos se compila para `wasm32-wasip1-threads`: cada hilo
de Rust corre en un Web Worker con su propia instancia y la misma memoria compartida. En el
comparador, kodim23 pasa de 2,4 a 1,3 s con 4 núcleos (compartidos con los otros formatos, que
se codifican a la vez); en Node, con la máquina libre, de 2,2 a 0,93 s. Por el camino aparecieron
tres problemas:

| Problema | Síntoma | Solución |
|---|---|---|
| `malloc` de wasi-libc con un único cerrojo | Con más de 2 hilos iba **más lento** | Caché de bloques pequeños por hilo (`src/talloc.rs`) |
| La biblioteca no tiene `_start`, y nadie inicializaba la estructura del hilo principal de wasi-libc | Se bloqueaba al crear el primer hilo | La página llama a `__wasi_init_tp` y a los constructores |
| En V8, si un hilo hace crecer la memoria compartida, los demás pueden seguir viendo el tamaño anterior | «memory access out of bounds» aleatorio, 1 de cada 2 veces | Antes de crear los hilos de cada plano se reserva la memoria que necesitarán, y así no crece mientras trabajan |

**Teselas de 2048: probadas y no adoptadas por defecto.** Con el frente de onda ya no hace falta
trocear la imagen para codificar en paralelo, y una tesela grande comprime algo mejor: −1,4 % en
las 12 fotos de 2,7 MP. Pero el decodificador solo paraleliza por teselas, y esas fotos pasan de
4 teselas a 1: decodificar tarda 2,3 veces más (0,10 → 0,23 s). Se quedan en 1024, y quien
prefiera el 1,4 % puede usar `--tile 2048`.

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
| `-e 1..9` | Esfuerzo de búsqueda (por defecto 4). Los niveles 1–3 son modos rápidos: ver la tabla de abajo. |
| `--tune balanced\|perceptual\|psnr` | Perfil de ajuste. |
| `--tile N` | Tamaño de las teselas en píxeles (múltiplo de 64). Por defecto, 1024; con 2048 ocupa ~1,4 % menos en fotos grandes, pero decodifica más despacio. |
| `--threads N` | Hilos del codificador (0 = todos). El resultado es idéntico con cualquier número. |
| `--v1` | Escribe la versión 1 (congelada) en lugar de la 2. |
| `--420` / `--444` | Fuerza el submuestreo de croma. |
| `--no-restore` | Desactiva el filtro de restauración. |
| `--lossless` | Compresión sin pérdida. |
| `--preview` | Añade una vista previa progresiva (~300 bytes). |
| `--sin-metadatos` | No guarda el perfil de color ni el EXIF de la imagen original. |
| `--screen` / `--no-screen` | Fuerza o desactiva las herramientas de contenido sintético. |
| `--aq`, `--cq`, `--qm`, `--qmc`, `--lambda` | Ajustes finos. |

## En la web

### Opción 1: HTML normal (recomendada)

```html
<script src="/tesela-auto.js"></script>   <!-- en el <head> -->
<img src="foto.tsl" alt="Descripción">
```

**Instalación.** Copia `tesela-auto.js`, `tesela-sw.js`, `tesela.wasm`, `tesela.js`,
`tesela-core.js` y `tesela-worker.js` a la **raíz** de tu web.

**Cómo funciona.** `tesela-sw.js` es un Service Worker que entrega cada `.tsl` al navegador ya
convertido a PNG. Por eso funcionan `<img>`, `srcset`, `<picture>`, los fondos CSS y "Guardar
imagen como" sin cambiar nada más.

**Detalles:**

- **Primera visita.** Aún no hay Service Worker activo; las imágenes `.tsl` se recargan solas en
  cuanto se activa (~0,3 s).
- **Requisitos.** Hace falta HTTPS (o `localhost`), y el Service Worker solo controla las páginas
  de su carpeta y subcarpetas: por eso va en la raíz.
- **Sin Service Worker.** En navegadores sin Service Worker, las `<img>` se decodifican en la
  página; los fondos CSS, en cambio, no se convierten.
- **Demo.** [`web/demo/img.html`](web/demo/img.html).

### Opción 2: elemento propio, con vista previa progresiva y respaldo

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

- **Tamaño del módulo.** `web/tesela.wasm` pesa 515 KB e incluye el codificador, el
  decodificador y las métricas.
- **Paralelismo.** La librería reparte las teselas entre varios Web Workers.
- **Recompilar.** `web/build.sh` regenera el módulo (necesitas
  `rustup target add wasm32-unknown-unknown`).

Para probarlo en local:

```sh
python3 web/serve.py
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

Tesela codifica con **varios hilos** en el comparador (`web/tesela-mt.wasm`), con el mismo
resultado byte a byte que con uno. Para eso el navegador exige que la página esté aislada entre
orígenes: `web/serve.py` envía las cabeceras necesarias, y donde el servidor no las envía (GitHub
Pages) las añade un service worker (`compare/coi-sw.js`), que recarga la página una vez. Si nada de
eso es posible, se codifica con un hilo y la página lo indica.

**Fotos grandes.** El comparador estima la memoria de cada trabajo (códec y métricas) y solo lanza
en paralelo los que caben en la del equipo; con fotos grandes van de uno en uno. Si aun así la foto
no cabe, la reduce lo justo y lo indica (JPEG XL es el más exigente: sin él caben fotos más
grandes). Con 8 GB o más, una foto de 12 MP entra a tamaño completo. Para medir fotos enormes sin
límites está la línea de comandos (`tesela roundtrip`) y el banco de pruebas.

**SSIMULACRA2 por filas.** La implementación de referencia (crate `ssimulacra2`) guarda unas 13
imágenes completas en coma flotante: ~165 bytes por píxel, casi 2 GB con 12 MP, en cada worker.
`src/ssim2.rs` hace las mismas operaciones en el mismo orden pero fila a fila (anillos de 12 filas
para el gaussiano recursivo) y solo guarda las escalas reducidas: **22 bytes por píxel** y el mismo
resultado bit a bit (lo comprueban los tests, con imágenes sintéticas y con fotos reales). En
nativo va igual de rápido o algo más.

## Banco de pruebas

```sh
pip install -r bench/requirements.txt   # Pillow (WebP y AVIF), imagecodecs (libjxl 0.12), numpy, scipy, matplotlib
bench/fetch_kodak.sh
python3 bench/bench.py                  # Kodak, todos los códecs
bench/fetch_div2k.sh && python3 bench/bench.py --images bench/data/div2k --out bench/results/fotos-grandes
cargo doc --no-deps && node bench/gen_synthetic.js bench/data   # capturas e ilustraciones
python3 bench/bench.py --images bench/data/screen --out bench/results/capturas
python3 bench/bench.py --codecs tesela --reuse --tesela-args "--aq 1.5"   # iterar solo Tesela
```

Para reentrenar las probabilidades iniciales (con imágenes que no sean las de evaluación):

```sh
./target/release/tesela train src/init_probs.rs entrenamiento/*.png
```

## Limitaciones

- **Soporte nativo.** Ningún navegador ni sistema operativo lo soporta de forma nativa: hace falta
  `tesela.js` y WebAssembly (630 KB; 730 KB más la versión con hilos, solo para codificar). Conviene servir un `fallback` en JPEG.
- **Fotos grandes.** Frente a AVIF con croma completa (4:4:4), en las 50 fotos de 2,7 MP de DIV2K
  la diferencia es de solo 0,5 puntos: prácticamente un empate. En CLIC y Kodak Tesela gana por
  6 y 4 puntos.
- **Sin pérdida en fotos.** Ocupa más que WebP y JPEG XL.
- **Formato de los datos.** Solo admite 8 bits por canal, y el canal alfa siempre es sin pérdida
  (un alfa con degradados suaves cuesta bastante). No tiene HDR. En la web, el perfil de color se
  respeta con `tesela-auto.js`, pero `<tesela-img>` pinta en sRGB.
- **Animación con movimiento natural.** Tesela gana a AVIF animado (4:4:4) en cuatro de las seis
  animaciones del banco, con mucha ventaja en contenido de pantalla, pero en un zoom de 90
  fotogramas AVIF ocupa un 18 % menos y con objetos que se mueven, un 8 % menos (fotogramas
  futuros ocultos y predicción bidireccional).
- **Velocidad de codificación.** Codificar sigue siendo más lento que AVIF: ~1 s por foto de
  0,4 MP con 4 núcleos y ~10 s por foto de 12 MP. En el navegador va con hilos solo si la página
  está aislada entre orígenes (ver el comparador); la librería `tesela.js` aún codifica con uno.
- **Licencias de los datos.** Las imágenes de prueba (Kodak, DIV2K) y de entrenamiento (BSD100) no
  se redistribuyen, salvo `web/compare/sample.png` (kodim23). Las capturas e ilustraciones se
  generan con `bench/gen_synthetic.js`.
