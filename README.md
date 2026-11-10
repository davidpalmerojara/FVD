# FVD: códec de imagen experimental en Rust

FVD es un códec de imagen con pérdida escrito desde cero en Rust, sin
dependencias de códecs externos. Incluye un banco de pruebas reproducible que
lo compara con WebP, AVIF y JPEG XL.

## Resultados (Kodak, 24 imágenes)

El BD-rate frente a WebP indica cuántos bytes ahorra cada códec a igual
calidad. Un valor negativo significa que necesita menos bytes.

| Códec | SSIMULACRA2 | PSNR-Y | SSIM-Y | PSNR RGB | Codificación | Decodificación |
|---|---|---|---|---|---|---|
| **FVD** (perceptual, por defecto) | **−29,1 %** | −2,7 % | −13,9 % | −14,5 % | 1,80 s | 0,054 s |
| **FVD** `--tune psnr` | −3,3 % | **−23,3 %** | −16,4 % | **−22,9 %** | 1,87 s | 0,050 s |
| WebP (libwebp 1.6, `method=6`) | 0 % | 0 % | 0 % | 0 % | 0,12 s | 0,114 s |
| AVIF (libavif 1.4 + aom, `speed=4`) | −24,8 % | −16,4 % | −22,1 % | −17,4 % | 1,85 s | 0,210 s |
| JPEG XL (libjxl 0.7, `effort=7`) | −13,1 % | +2,5 % | −2,2 % | +11,8 % | 0,20 s | 0,147 s |

Lo que muestran estos números:

- **Calidad perceptual.** Con SSIMULACRA2, la métrica que mejor se correlaciona
  con la percepción humana, FVD ocupa un 29 % menos que WebP a igual calidad y
  también supera a AVIF y JPEG XL.
- **PSNR.** Con `--tune psnr`, FVD ocupa un 23 % menos que WebP y supera a AVIF
  en esa métrica.
- **Compromiso entre perfiles.** El perfil perceptual sacrifica PSNR a cambio
  de calidad visual; el perfil PSNR hace lo contrario.
- **Velocidad.** Decodifica de 2 a 4 veces más rápido que los demás. Codifica
  a una velocidad parecida a AVIF `speed=4`.

![curvas](bench/results/curves.png)

El informe completo y los datos de cada punto están en
[`bench/results/`](bench/results/REPORT.md).

## Diseño

| Etapa | Técnica |
|---|---|
| Color | YCbCr BT.601 de rango completo. Croma 4:2:0 (centrado, interpolación bilineal) por debajo de calidad 90 y 4:4:4 por encima. |
| Partición | Superbloques de 64×64 divididos en un quadtree hasta 4×4. La decisión se toma por RDO (`D + λ·R`) con la tasa estimada a partir de los contextos reales. |
| Predicción | Intra planar, DC y 33 direcciones angulares, con filtrado de referencias según tamaño y modo. Para croma también hay **CCLM** (croma a partir de luma), cuyos parámetros se deducen de los vecinos sin transmitirse. |
| Transformada | DCT-II ortonormal separable de 4, 8, 16, 32 y 64. |
| Cuantización | Matriz de cuantización perceptual (`1 + s·r²`), cuantización adaptativa por actividad en celdas de 32×32 y **RDOQ**, que decide cada nivel según su coste en distorsión y bits. |
| Entropía | Range coder binario con modelos de doble velocidad. Los coeficientes se codifican con `cbf`, `eob` y un barrido diagonal inverso con contextos que dependen del tamaño, la frecuencia y la magnitud de los vecinos ya codificados. Los modos se codifican con 3 MPM. |

Código fuente:

```
src/rc.rs       codificador aritmético y binarizaciones
src/dct.rs      DCT y SATD (Hadamard)
src/predict.rs  predicción intra y CCLM
src/coeff.rs    codificación de coeficientes y RDOQ
src/codec.rs    cabecera, sintaxis, codificador (RDO) y decodificador
src/color.rs    conversión de color y submuestreo
src/deblock.rs  filtro de desbloqueo opcional (desactivado por defecto)
src/metrics.rs  PSNR, SSIM y SSIMULACRA2
```

Toda la sintaxis se escribe una sola vez sobre el trait `Coder`, que tiene tres
implementaciones: codificar, decodificar y estimar coste. Así el codificador y
el decodificador no pueden desincronizarse. Los tests verifican que la salida
del decodificador coincide bit a bit con la reconstrucción del codificador.

## Uso

```sh
cargo build --release
./target/release/fvd encode foto.png foto.fvd -q 70
./target/release/fvd decode foto.fvd salida.png
./target/release/fvd compare foto.png salida.png      # métricas en JSON
./target/release/fvd roundtrip foto.png -q 70         # codifica, decodifica, verifica y mide
```

Opciones de `encode`:

| Opción | Efecto |
|---|---|
| `-q 0..100` | Calidad. |
| `-e 1..9` | Esfuerzo de búsqueda de modos. |
| `--tune psnr` | Activa el perfil PSNR. |
| `--420` / `--444` | Fuerza el submuestreo de croma. |
| `--aq`, `--cq`, `--qm`, `--qmc`, `--lambda`, `--deblock` | Ajustes finos. |

## Banco de pruebas

```sh
pip install -r bench/requirements.txt   # Pillow con WebP y AVIF, numpy, scipy, matplotlib
bench/fetch_kodak.sh                    # descarga las 24 imágenes Kodak
python3 bench/bench.py                  # todos los códecs; ffmpeg con libjxl para JPEG XL
python3 bench/bench.py --codecs fvd --reuse --fvd-args "--aq 1.5"   # iterar solo FVD
```

El script hace lo siguiente:

1. Recorre 9 niveles de calidad por códec.
2. Mide todas las salidas con el mismo binario (`fvd compare`).
3. Calcula el BD-rate por imagen con interpolación PCHIP en un rango de calidad
   típico de la web (SSIMULACRA2 entre 30 y 90).
4. Promedia los resultados y genera `REPORT.md`, `summary.json` y `curves.png`.

## Limitaciones y trabajo futuro

- **Uso.** Es un formato experimental: ningún navegador ni sistema operativo
  lo abre. Para usarlo en la web haría falta un decodificador en WASM.
- **Funcionalidad.** Solo admite RGB de 8 bits. No tiene canal alfa, modo sin
  pérdida, metadatos ICC ni EXIF.
- **Determinismo.** La DCT usa coma flotante. Es determinista con el mismo
  binario, pero entre arquitecturas distintas podría haber diferencias de 1 LSB
  que se propaguen. Una DCT entera lo resolvería.
- **Velocidad.** El codificador es monohilo.
- **Mejoras posibles.** Particiones rectangulares, DST para 4×4, inicialización
  entrenada de los contextos y codificación conjunta de Cb/Cr.
- **Alcance de las comparativas.** Los resultados se midieron solo en Kodak,
  con imágenes de 768×512. La versión de JPEG XL (0.7) es antigua; versiones
  recientes de libjxl rinden mejor.
