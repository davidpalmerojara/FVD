# Especificación del formato Tesela, versión 2 (borrador)

Estado: **en desarrollo**. La v2 puede cambiar todavía; los archivos v2 que se escriban hoy quizá
no se abran con la versión final. La v1 sigue congelada y cualquier decodificador v2 la abre igual
([`especificacion-v1.md`](especificacion-v1.md)).

Este documento solo describe **las diferencias con la v1**. Todo lo que no se menciona aquí es
igual que en la v1. Mientras sea un borrador, la implementación de referencia (`src/`) es
normativa en caso de duda.

## 1. Cabecera

El byte de versión vale **2**. El resto de la cabecera no cambia, salvo un bit nuevo en `tools`:

| Bit | Nombre | Significado |
|---|---|---|
| 32 | `META` | Hay una sección de metadatos tras la cabecera (sección 1.1). |
| 64 | `ANIM` | Animación: lista de fotogramas en lugar de la tabla de teselas (sección 1.2). |
| 128 | `INTER` | Solo en el archivo de un fotograma (no el primero): predicción desde el fotograma anterior (sección 1.3). |

Un decodificador v1 rechaza un archivo con un bit de `tools` que no conoce.

### 1.1 Sección de metadatos

Si `tools & META`, justo después de la cabecera (antes de la vista previa, si la hay) viene:

```
u32  L                     longitud del resto de la sección
L bytes de fragmentos:
  4 bytes  etiqueta (ASCII)
  u32      n
  n bytes  datos
```

| Etiqueta | Contenido |
|---|---|
| `ICCP` | Perfil de color ICC, tal cual. Los valores RGB decodificados están en ese espacio de color; sin perfil, en sRGB. |
| `EXIF` | Bloque EXIF en formato TIFF (como el segmento APP1 de JPEG, sin el prefijo `Exif\0\0`). |

Las etiquetas desconocidas se ignoran, así que se pueden añadir otras sin cambiar de versión. Los
metadatos no afectan a la decodificación de los píxeles. La orientación EXIF no se aplica al
decodificar: el codificador de referencia la aplica a los píxeles y deja la etiqueta en 1.

### 1.2 Animación

Si `tools & ANIM`, el ancho y el alto de la cabecera son los del lienzo, el archivo no lleva vista
previa (`PREVIEW` debe ser 0) y, tras la cabecera y los metadatos, viene:

```
u16  repeticiones          0 = sin fin
u16  reservado             0
u32  n                     número de fotogramas (≥ 1)
n veces (v = entero LEB128: 7 bits por byte, primero los bajos; el bit alto indica que sigue;
         como mucho 5 bytes y 2³² − 1):
  v    duración (ms)
  v    x, v y, v ancho, v alto     rectángulo dentro del lienzo
  u8   forma: bit 0 = abreviada; bit 1 = referencia larga (ver 1.3); el resto, 0
  forma 0:
    v      longitud
    datos: un archivo .tsl completo del rectángulo (sin ANIM)
  forma 1 (abreviada, bit 0; no en el primer fotograma):
    u8     tools
    v      longitud de cada tesela (tantas como teselas tenga el rectángulo)
    datos de las teselas, seguidos
```

En la forma abreviada, la cabecera del fotograma es la del fotograma anterior (completa o
reconstruida) con el ancho, el alto y `tools` de la entrada; `tools` no puede tener `PREVIEW`,
`META` ni `ANIM`, y la cabecera resultante debe ser válida como cualquier otra. Equivale al
archivo formado por esa cabecera, la tabla de longitudes en `u32` y los datos.

El lienzo empieza vacío y cada fotograma sustituye su rectángulo (RGBA, también el alfa); el
resultado tras cada uno es la imagen que se muestra durante su duración. El primer fotograma cubre
todo el lienzo. Un decodificador que solo muestre imágenes fijas usa el primer fotograma.

El codificador de referencia guarda solo el rectángulo que cambia respecto al fotograma anterior y
une los fotogramas idénticos sumando sus duraciones.

### 1.3 Predicción entre fotogramas

Un fotograma cuyo archivo tiene `INTER` puede predecir sus bloques del fotograma anterior. Si el
archivo tiene croma 4:2:0, `x` e `y` deben ser pares.

**Referencia.** El decodificador de una animación guarda, además del lienzo RGBA, unos **planos de
referencia** del tamaño del lienzo (tres planos de 8 bits, con la croma a la mitad en 4:2:0) y su
espacio de color: (sin pérdida, 4:2:0). Al empezar, no hay planos. Antes de cada fotograma:

1. Si no hay planos o su espacio de color no es el del archivo del fotograma, se calculan a partir
   del lienzo (RGB, antes de este fotograma) con la transformación de color del archivo (YCbCr con o
   sin submuestreo, o G, B, R sin pérdida, como en la v1), redondeando cada muestra a 8 bits
   (`round` y `clamp(0, 255)`).
2. Se decodifica el fotograma; si tiene `INTER`, predice de esos planos.
3. Los planos finales del fotograma (tras el relleno de la sección 4, la restauración y, si lo hay,
   el desbloqueo; antes de convertirlos a RGB) se copian en los de referencia en
   `(x / d, y / d)`, con d = 2 en la croma submuestreada y 1 en otro caso, recortando a la parte
   visible de cada plano (en 4:2:0, `⌈w / 2⌉ × ⌈h / 2⌉` en la croma). Si el archivo tiene croma 4:2:0
   y `x` o `y` es impar, en lugar de copiarlos se descartan los planos de referencia.

4. Si el fotograma es el primero o su forma tiene el bit 1, los planos de referencia (tras el paso
   3) se copian también como **referencia larga**, con su espacio de color. Si en ese momento no hay
   planos de referencia, la referencia larga queda vacía.

Así, una zona que no cambia o que solo se desplaza se copia exacta, sin pasar por RGB en cada
fotograma. Para una tesela que empieza en (tx, ty) dentro del fotograma, el origen en el plano de
referencia es `((x + tx) / d, (y + ty) / d)`.

**Movimiento global.** Al principio de los datos de cada tesela de un archivo con `INTER`, antes
del primer plano, va un bit sin contexto. Si vale 1, sigue otro bit sin contexto, `larga`, y seis
enteros con signo (a, b, c, d, e, f),
cada uno así: 6 bits sin contexto con n, el número de bits de la magnitud (0 = valor 0); si n > 0,
los n − 1 bits bajos de la magnitud (el alto es 1) y un bit de signo (1 = negativo). Se recortan a
±2²⁰ (a, b, d, e) y a ±2³⁰ (c, f). El vector de luma en 1/16 de píxel en el punto (X, Y) del
lienzo es `((a·X + b·Y + c + 2¹³) >> 14, (d·X + e·Y + f + 2¹³) >> 14)` (en 64 bits; cada
componente se recorta a ±2²²).

**Sintaxis.** Al principio de la cabecera de cada hoja (antes de la clase especial de la v1) va el
bit `inter[(pc·5 + sz)·3 + c]`, donde c es cuántos vecinos (izquierdo y superior) tienen la marca
202 o 203. Si vale 1 y la tesela lleva movimiento global, sigue el bit
`warp[(pc·5 + sz)·3 + c']`, con c' el número de esos vecinos con la marca 203; si vale 1, la hoja
usa el movimiento global (marca 203) y no lleva vector. Si no, sigue un vector de movimiento con la
misma sintaxis que el vector de la copia intra (sección 6.5 de la v1), con modelos propios (`mv`),
pero codificado respecto a un **predictor**: la mediana, componente a componente, de los vectores
de los vecinos izquierdo (x0 − 1, y0), superior (x0, y0 − 1) y superior izquierdo (x0 − 1, y0 − 1)
que sean hojas con la marca 202 o 203. Los que no lo sean toman el valor del último vector del plano
(que empieza en (0, 0) en cada plano de cada tesela); si solo uno lo es, el predictor es ese; si
ninguno, el último vector. Tras decodificar la hoja, el último vector pasa a ser el suyo (las hojas
con movimiento global no lo cambian). La marca de la hoja en el mapa es 202, y la cabecera sigue
con `ts` como cualquier otra hoja.

Si `larga` vale 1 y hay referencia larga en el espacio de color del archivo del fotograma, las
hojas con movimiento global predicen de sus planos (con el mismo origen); si no, de los planos de
referencia del fotograma anterior. Las hojas con vector propio predicen siempre de estos.

Una hoja con movimiento global se predice por bloques de 4 × 4: el bloque que empieza en (x, y)
del plano (coordenadas del lienzo en ese plano) usa el vector de luma en el punto
`(x·d + 2d, y·d + 2d)`; en la croma submuestreada, cada componente v pasa a `(v + 1) >> 1`. La
predicción es como la de abajo pero con vectores en 1/16 de píxel (`mv >> 4` y `mv & 15`) y la
tabla `G` de 16 filtros:

```
G[0]  = [ 0, 0,   0, 64,  0,   0, 0,  0]     G[8]  = [-1, 4, -10, 39, 39, -10, 4, -1]
G[1]  = [ 0, 1,  -3, 63,  4,  -1, 0,  0]     G[9]  = [-1, 3,  -9, 34, 45, -11, 4, -1]
G[2]  = [-1, 2,  -6, 63,  8,  -3, 1,  0]     G[10] = [ 0, 3,  -8, 29, 48, -11, 4, -1]
G[3]  = [-1, 3,  -8, 60, 13,  -4, 1,  0]     G[11] = [ 0, 2,  -7, 23, 53, -10, 4, -1]
G[4]  = [-1, 3,  -9, 57, 18,  -6, 2,  0]     G[12] = [ 0, 2,  -6, 18, 57,  -9, 3, -1]
G[5]  = [-1, 4, -10, 53, 23,  -7, 2,  0]     G[13] = [ 0, 1,  -4, 13, 60,  -8, 3, -1]
G[6]  = [-1, 4, -11, 48, 29,  -8, 3,  0]     G[14] = [ 0, 1,  -3,  8, 63,  -6, 2, -1]
G[7]  = [-1, 4, -11, 45, 34,  -9, 3, -1]     G[15] = [ 0, 0,  -1,  4, 63,  -3, 1,  0]
```

En el mapa, cada bloque de 4 × 4 guarda su vector redondeado a cuartos de píxel
(`(v + 2) >> 2`), que es el que ven los vecinos al calcular el predictor.

**Predicción.** Los vectores están en **cuartos de píxel**. Con la parte entera
`(ix, iy) = (mvx >> 2, mvy >> 2)` (desplazamiento aritmético) y la fracción `(fx, fy) = (mvx & 3,
mvy & 3)`, sea `R(x, y)` la referencia con las coordenadas recortadas al plano (se repite el borde)
y `F` los filtros:

```
F[0] = [ 0, 0,   0, 64,  0,   0, 0,  0]
F[1] = [-1, 4, -10, 58, 17,  -5, 1,  0]
F[2] = [-1, 4, -11, 40, 40, -11, 4, -1]
F[3] = [ 0, 1,  -5, 17, 58, -10, 4, -1]
```

Si `fx = fy = 0`, la muestra (x, y) es `R(ox + x0 + x + ix, oy + y0 + y + iy)`. Si no, en enteros:
`t(x, r) = Σk F[fx][k] · R(ox + x0 + x + ix + k − 3, oy + y0 + r + iy − 3)` para
r = 0 … h + 6, y la muestra es `clamp((Σk F[fy][k] · t(x, y + k) + 2048) >> 12, 0, 255)`. El residuo
y la reconstrucción son los de la v1.

Los modelos `inter`, `mv` y `warp` van al final del orden de visita, detrás de los de la v2, en ese orden.

## 2. Bloques rectangulares

La v2 añade hojas de tamaño **w × h** con w ≠ h, en proporción 2:1 o 1:2. Por eso los
tamaños de la v1 (N ∈ {4, 8, 16, 32, 64}) pasan a ser lados: w, h ∈ {4, 8, 16, 32, 64}.

### 2.1 Sintaxis del árbol

En la v1, un nodo de lado N > 4 con `split` = 0 es una hoja N × N. En la v2, después de
`split` = 0 viene:

1. Bit `rect[pc·4 + size_index(N) − 1]`. Si vale 0, el nodo es una hoja N × N como en la v1.
2. Si vale 1, bit `rect_dir[pc·4 + size_index(N) − 1]`:
   - **1 = horizontal**: dos hojas N × N/2, en (x0, y0) y en (x0, y0 + N/2), en ese orden.
   - **0 = vertical**: dos hojas N/2 × N, en (x0, y0) y en (x0 + N/2, y0), en ese orden.

Las mitades son hojas: no se vuelven a dividir. La sintaxis existe para N = 8, 16, 32 y 64.
El codificador de referencia no usa rectángulos en N = 64, pero el decodificador debe aceptarlos.

### 2.2 Cabecera de la hoja y coeficientes

- **Contextos.** Donde la v1 usa `sz = size_index(N)`, la v2 usa `size_index(max(w, h))`.
- **Transform skip.** La condición N ≤ 16 pasa a ser max(w, h) ≤ 16.
- **Paleta.** Los índices se recorren en un bloque de w × h, con el mismo orden y los mismos
  vecinos que en la v1.
- **Copia intra (IBC).** El bloque de referencia es de w × h y debe estar entero disponible.
- **Barrido.** Diagonal sobre w × h: para d = 0 … w + h − 2, y para y = 0 … h − 1 con
  0 ≤ d − y < w, la posición (x = d − y, y).
- **Clases de frecuencia y magnitud.** Como en la v1; la plantilla de vecinos se recorta al
  rectángulo w × h.

### 2.3 Mapa del plano

Al terminar una hoja w × h se marcan sus unidades de 4 × 4 igual que en la v1. El «lado menor del
bloque» que guarda el mapa (y que usa el contexto de `split`) es min(w, h).

## 3. Reconstrucción de una hoja rectangular

En un bloque cuadrado, todo es idéntico bit a bit a la v1.

### 3.1 Cuantización

- **Celdas AQ.** Las que cubre el rectángulo: de `⌊x0·f/32⌋` a `⌈(x0+w)·f/32⌉ − 1` en x, y de
  `⌊y0·f/32⌋` a `⌈(y0+h)·f/32⌉ − 1` en y.
- **Matriz.** `w(x, y) = 1 + s · ((x/w)² + (y/h)²)`.

### 3.2 Residuo

- **Solo DC.** Si solo el coeficiente (0, 0) es no nulo, el residuo es constante,
  `c(0,0) / √(w·h)`.
- **Transformada.** DCT-II inversa ortonormal separable: primero las h filas de longitud w y
  luego las w columnas de longitud h, con las mismas DCT 1-D de la v1 (sección 9).

### 3.3 Predicción intra

- **Referencias.** h + w muestras a la izquierda (de abajo arriba) y w + h arriba (de izquierda a
  derecha), con las mismas reglas de disponibilidad y relleno que en la v1.
- **Filtrado de referencias.** Se decide con el lado equivalente `2^((log2 w + log2 h) / 2)`
  (división entera) en lugar de N.
- **Planar.** Con T y L como en la v1, lw = log2 w y lh = log2 h:
  `pv = ((h−1−y)·T[x] + (y+1)·L[h]) << lw`, `ph = ((w−1−x)·L[y] + (x+1)·T[w]) << lh` y
  `pred = (pv + ph + w·h) >> (lw + lh + 1)`. En un cuadrado es la fórmula de la v1.
- **DC.** `(Σ T[0..w) + Σ L[0..h) + (w+h)/2) / (w+h)` (división entera).
- **Angular.** Como en la v1, con el eje principal de longitud w (modos verticales) o h
  (horizontales) y el eje secundario de la otra longitud.
- **CCLM.** Los parámetros se deducen con las muestras vecinas de arriba (w) y de la izquierda
  (h) en lugar de N y N.

## 4. Relleno de los planos

Los planos se codifican con su tamaño redondeado a superbloques. En la v2, tras decodificar los
tres planos de una tesela y antes de la restauración, lo que queda fuera de la parte visible de
cada plano (`w × h` en luma; en 4:2:0, `⌈w / 2⌉ × ⌈h / 2⌉` en croma) se sustituye repitiendo el
borde: primero, en cada fila visible, las columnas a la derecha toman el valor de la última columna
visible; después, las filas por debajo copian la última fila visible. Así el codificador puede
dejar cualquier cosa en esa zona (no se ve) sin que afecte a la restauración ni a la conversión de
color.

## 5. Probabilidades iniciales

Los modelos se inicializan con `init_probs_v2::TABLES[sintético][banda]`, en el mismo orden de
visita que en la v1. Los modelos nuevos (`rect` y `rect_dir`) van **al final** de ese orden, así
que el prefijo coincide con la lista de modelos de la v1.

## 6. Pendiente antes de congelar

- Vectores de conformidad v2 (incluido uno con metadatos y vista previa).
- Fijar en este texto la parte angular y CCLM con fórmulas completas, como en la v1.
- Decidir si se añaden más herramientas a la v2 antes de congelarla.
