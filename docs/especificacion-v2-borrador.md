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
  u8   forma: bit 0 = abreviada; bit 1 = renueva la referencia larga; bit 2 = movimiento global;
       bit 3 = respecto a la referencia larga; bit 4 = parámetros iguales a la predicción
       (los bits 3 y 4 solo con el 2; ver 1.3); el resto, 0
  si bit 2 y no bit 4: 6 × v (zigzag)      diferencia de cada parámetro con la predicción
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

**Movimiento global.** Si la forma del fotograma tiene el bit 2, el fotograma lleva un movimiento
global: seis enteros (a, b, c, d, e, f) y si es respecto a la referencia larga (`larga`, bit 3). Se
**predicen** de la tabla de fotogramas: con los dos fotogramas anteriores de la tabla (el más
reciente, P1, y el anterior, P2), cada uno con movimiento global del mismo tipo (`larga` igual) y,
si `larga`, usado con la misma referencia larga (sin renovación entre medias), la predicción es
2·P1 − P2 (recortada a los límites); si solo P1 cumple, P1; si no, ceros. Un fotograma sin
movimiento global vacía la historia. Si el bit 4 está activo, los parámetros son la predicción; si
no, se leen seis diferencias en zigzag (`v` par = v/2, impar = −(v+1)/2) que se suman a ella. Se
recortan a ±2²⁰ (a, b, d, e) y a ±2³⁰ (c, f). El vector de luma en 1/16 de píxel en el punto (X, Y)
del lienzo es `((a·X + b·Y + c + 2¹³) >> 14, (d·X + e·Y + f + 2¹³) >> 14)` (en 64 bits; cada
componente se recorta a ±2²²).

**Contextos.** Las teselas de un fotograma con `INTER` no empiezan con las probabilidades iniciales
de la sección 5: todos sus modelos empiezan en el estado (probabilidades y contadores) en que
quedaron al terminar de decodificar la tesela 0 del fotograma anterior de la tabla (tras su
restauración; el canal alfa tiene sus propios modelos y no cuenta). Así un fotograma pequeño no
paga por volver a aprender las estadísticas.

**Mapa de cuantización adaptativa.** En un archivo con `INTER` y `AQ`, el dq de cada celda no se
codifica tal cual sino como diferencia con un dq **deducido** de la referencia: para la tesela de
w × h, sea R(x, y) la muestra de luma de los planos de referencia del fotograma anterior en
`(ox + min(x, w − 1), oy + min(y, h − 1))` (recortada al plano), con x, y hasta el tamaño rellenado
hasta el superbloque. Por cada bloque de 8 × 8 de una celda de 32 × 32, con s1 = ΣR y s2 = ΣR²,
`v = (64·s2 − s1²) / 4096 + 1` (división entera), `n = ⌊log2 v⌋` y `l = 16n + ((v·16) >> n) − 16`.
La actividad de la celda es la media entera de sus 16 valores l; con m la media entera de las
actividades de todas las celdas de la tesela y d = actividad − m, el dq deducido es
`(d + 8·signo(d)) / 16` (división entera hacia cero) recortado a ±8. El dq de la celda es el
deducido más el valor leído (recortado a ±64, como en la v1).

**Sintaxis.** Al principio de la cabecera de cada hoja (antes de la clase especial de la v1) va el
bit `inter[(pc·5 + sz)·3 + c]`, donde c es cuántos vecinos (izquierdo y superior) tienen la marca
202 o 203. Si vale 1 y el fotograma lleva movimiento global, sigue el bit
`warp[(pc·5 + sz)·3 + c']`, con c' el número de esos vecinos con la marca 203; si vale 1, la hoja
usa el movimiento global (marca 203) y no lleva vector. Si no, en los planos de croma, si la
luma de la posición de la hoja (la unidad de 4 × 4 de luma que contiene el punto
`((x0 + w/2)·d, (y0 + h/2)·d)`, recortado al plano) tiene la marca 202 o 203, va el bit
`inh[sz]`; si vale 1, el vector es el de esa luma (con croma 4:2:0, cada componente v pasa a
`(v + 1) >> 1`) y no se lee nada más. Si no, sigue un vector de movimiento con la
misma sintaxis que el vector de la copia intra (sección 6.5 de la v1), con modelos propios (`mv`),
pero codificado respecto a un **predictor**: la mediana, componente a componente, de los vectores
de los vecinos izquierdo (x0 − 1, y0), superior (x0, y0 − 1) y superior izquierdo (x0 − 1, y0 − 1)
que sean hojas con la marca 202 o 203. Los que no lo sean toman el valor del último vector del plano
(que empieza en (0, 0) en cada plano de cada tesela); si solo uno lo es, el predictor es ese; si
ninguno, el último vector. El **segundo candidato** es el primero, en este orden, de los vectores
de esos tres vecinos (solo los que tengan la marca 202 o 203) y el último vector del plano que sea
distinto del predictor; puede no haberlo. Si el bit «igual al predictor» vale 0 y hay segundo
candidato, sigue el bit `mv_alt[pc]`: si vale 1, el vector es el segundo candidato y no se lee la
diferencia; si vale 0 (o no hay segundo candidato), siguen las dos diferencias respecto al
predictor. (`mv_alt` va tras `inh` en el orden de los modelos y empieza en p = 1/2.) Tras
decodificar la hoja, el último vector pasa a ser el suyo (las hojas
con movimiento global no lo cambian). La marca de la hoja en el mapa es 202, y la cabecera sigue
con `ts` como cualquier otra hoja.

Si `larga` y hay referencia larga en el espacio de color del archivo del fotograma, las
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

Los modelos `inter`, `mv`, `warp` e `inh` van al final del orden de visita, detrás de los de la v2, en ese orden (luego `mv_alt`, y después `smooth[2·5]` y `smooth_k[2][2]` de la sección 3.5).

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

### 2.4 Coeficientes

Cambian tres pasos de la sección 6.3 de la v1. Valen para los dos conjuntos de contextos (DCT y
transform skip), salvo la ocultación de signos, que solo se aplica a los bloques DCT. El modo sin
pérdida y el canal alfa, que empiezan con todos los modelos a ½ en lugar de las tablas, siguen
con la sintaxis de la v1.

1. **Última posición** (sustituye a `eob`). Se codifican las coordenadas (x, y) del último
   coeficiente no nulo en el orden del barrido: primero x, con lado L = w, y luego y, con L = h.
   Para cada coordenada v (0 ≤ v < L):
   - **Grupo.** g(v) = v si v < 4; si no, con k = ⌊log2 v⌋, g(v) = 2k + (bit k − 1 de v). Los
     grupos son 0, 1, 2, 3, 4–5, 6–7, 8–11, 12–15, 16–23, 24–31, 32–47 y 48–63.
   - **Prefijo.** El grupo en unario truncado a G = g(L − 1): para j = 0, 1, … mientras j < G,
     bit `last[(pc·5 + size_index(L))·11 + j]`; el primer 0 termina y g es el número de unos.
   - **Sufijo.** Si g > 3, `g/2 − 1` bits en bypass (el más significativo primero), que se suman
     al primer valor del grupo, `2^(g/2) + (g mod 2)·2^(g/2 − 1)`.

   `eob` es 1 más el índice de (x, y) en el barrido.
2. **Resto.** `m = 3 + Uint(rem_n[(pc·5 + fc)·7 + r])` en lugar de `Uint(rem[pc·5 + fc])`. La
   clase r sale de la suma **sin tope** s de |nivel| en las cinco posiciones de la plantilla de
   `mc` (las mismas, ya decodificadas): 0–2 → 0; 3–5 → 1; 6–9 → 2; 10–15 → 3; 16–25 → 4;
   26–40 → 5; ≥ 41 → 6.
3. **Signos y ocultación del signo (solo DCT).** Los signos ya no siguen a cada nivel: primero se
   decodifican las magnitudes de todo el bloque (significancia, `gt1`, `gt2` y resto, en el mismo
   orden y con los mismos contextos) y después los signos. Las posiciones 0 … eob − 1 del barrido
   se dividen en grupos de 16 (el último puede ser más corto). Se recorren los grupos del último
   al primero y, dentro de cada uno, sus niveles no nulos de mayor a menor índice. Cada signo se
   lee como en la v1 (en (0, 0) con `dc_sign[pc]`, en las demás posiciones en bypass) salvo el
   del **primer** no nulo del grupo (el de menor índice) cuando el índice del último no nulo del
   grupo lo supera en 4 o más: ese signo no se lee y es negativo si la suma de las magnitudes del
   grupo es impar.

   El codificador de referencia cumple la paridad en la cuantización: en cada grupo con el signo
   oculto que no la cumple, sube o baja en 1 el nivel que menos encarece D + λ·R, sin cambiar
   cuál es el primer no nulo del grupo ni el final del bloque (informativo).

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

Las fórmulas de arriba son las de la v1 extendidas a rectángulos. La v2 cambia además la predicción
intra de todos los bloques, cuadrados o no, como se describe en 3.5.

### 3.5 Herramientas intra de la v2

En esta sección, T[i] (i = 0 … w + h − 1) y L[j] (j = 0 … h + w − 1) son las referencias de
arriba y de la izquierda, C la esquina, lw = log2 w y lh = log2 h. «Referencias de la
predicción» son las filtradas con [1, 2, 1] si el modo filtra (3.3) y las originales si no.

**Variantes SMOOTH (modos 36, 37 y 38).** Tras decodificar el modo intra (sección 6.2 de la v1),
si es planar (0) se decodifica el bit `smooth[pc·5 + sz]`. Si vale 1, `smooth_k[pc][0]` elige
SMOOTH (0) o pasa a `smooth_k[pc][1]`, que elige SMOOTH_V (0) o SMOOTH_H (1). En el mapa de modos
la hoja guarda 36, 37 o 38, y la lista MPM de sus vecinos cuenta esas marcas como planar (0). Con
los pesos de AV1 `s_n` para un lado n (n = 4, 8, 16, 32 o 64):

```
s_4  = 255 149 85 64
s_8  = 255 197 146 105 73 50 37 32
s_16 = 255 225 196 170 145 123 102 84 68 54 43 33 26 20 17 16
s_32 = 255 240 225 210 196 182 169 157 145 133 122 111 101 92 83 74
       66 59 52 45 39 34 29 25 21 17 14 12 10 9 8 8
s_64 = 255 248 240 233 225 218 210 203 196 189 182 176 169 163 156 150
       144 138 133 127 121 116 111 106 101 96 91 86 82 77 73 69
       65 61 57 54 50 47 44 41 38 35 32 29 27 25 22 20
       18 16 15 13 12 10 9 8 7 6 6 5 5 4 4 4
```

y sobre las referencias **sin filtrar**, sean `v = s_h[y]·T[x] + (256 − s_h[y])·L[h−1]` y
`hz = s_w[x]·L[y] + (256 − s_w[x])·T[w−1]`. La predicción es `(v + hz + 256) >> 9` (SMOOTH),
`(v + 128) >> 8` (SMOOTH_V) o `(hz + 128) >> 8` (SMOOTH_H). No llevan PDPC.

**Interpolación cúbica.** En los modos angulares con fracción (ángulo distinto de 0 y de ±32)
cuyas referencias no se filtran, la interpolación de 2 coeficientes de la v1 se sustituye por
`clamp((c0·r[i−1] + c1·r[i] + c2·r[i+1] + c3·r[i+2] + 32) >> 6, 0, 255)`, con
(c0, c1, c2, c3) = `FC[f]` (la tabla de VVC):

```
f:  0 [ 0,64, 0, 0]   8 [-4,54,16,-2]  16 [-4,36,36,-4]  24 [-2,16,54,-4]
    1 [-1,63, 2, 0]   9 [-5,53,18,-2]  17 [-4,33,39,-4]  25 [-2,15,55,-4]
    2 [-2,62, 4, 0]  10 [-6,52,20,-2]  18 [-4,30,42,-4]  26 [-2,14,56,-4]
    3 [-2,60, 7,-1]  11 [-6,49,24,-3]  19 [-4,29,44,-5]  27 [-2,12,57,-3]
    4 [-2,58,10,-2]  12 [-6,46,28,-4]  20 [-4,28,46,-6]  28 [-2,10,58,-2]
    5 [-3,57,12,-2]  13 [-5,44,29,-4]  21 [-3,24,49,-6]  29 [-1, 7,60,-2]
    6 [-4,56,14,-2]  14 [-4,42,30,-4]  22 [-2,20,52,-6]  30 [ 0, 4,62,-2]
    7 [-4,55,15,-2]  15 [-4,39,33,-4]  23 [-2,18,53,-5]  31 [ 0, 2,63,-1]
```

Aquí r es la referencia principal de la v1 (r[0] = esquina, r[1 + i] = T[i] en los verticales o
L[i] en los horizontales) e i, f la posición y la fracción con que la v1 interpola
`(32 − f)·r[i] + f·r[i+1]`. Las posiciones que faltan: r[k] = r[w + h] para k > w + h; y, con
ángulo negativo, la extensión por el ángulo inverso de la v1 se calcula siempre, para
k = −1 … ⌊n·ángulo / 32⌋ − 1, con n la longitud del eje secundario (una muestra más que en la
v1), y con el índice de la otra referencia recortado a [0, w + h].

**PDPC.** Tras la predicción, en los modos 0 a 34 (no en SMOOTH ni CCLM), luma y croma, con las
referencias de la predicción (T, L y C) y P la muestra predicha:

- **Planar, DC, horizontal (10) y vertical (26).** Con `s = (lw + lh − 2) >> 2`,
  `wT = 32 >> min(31, (2y) >> s)` y `wL = 32 >> min(31, (2x) >> s)`:
  - planar y DC: `(wL·L[y] + wT·T[x] + (64 − wL − wT)·P + 32) >> 6`;
  - horizontal: `clamp(P + ((wT·(T[x] − C) + 32) >> 6), 0, 255)`;
  - vertical: `clamp(P + ((wL·(L[y] − C) + 32) >> 6), 0, 255)`.
- **Angulares 27 a 34** (verticales hacia arriba a la derecha) **y 2 a 9** (horizontales hacia
  abajo a la izquierda). Con a el ángulo del modo (positivo), `inv = (16384 + a/2) / a`
  (división entera), n = h en los verticales y w en los horizontales, y
  `s = min(2, log2 n − ⌊log2(3·inv − 2)⌋ + 8)`; si s < 0, no se hace nada. En los verticales,
  para cada muestra (x, y): `k = y + (((x + 1)·inv + 256) >> 9)`; si k < w + h,
  `wL = 32 >> min(31, (2x) >> s)` y la muestra pasa a ser `(wL·L[k] + (64 − wL)·P + 32) >> 6`.
  En los horizontales, lo mismo intercambiando x con y y L con T.

Todo en enteros con desplazamiento aritmético.

## 3.4 Mapa de cuantización adaptativa

En la v2, el dq de cada celda AQ (sección de la v1) no se codifica tal cual sino como diferencia
con una predicción: en los fotogramas predichos, el dq deducido de la referencia (sección 1.3); en
el resto, la predicción **espacial** a partir de las celdas ya decodificadas: con L la de la
izquierda y A la de arriba, `⌊(L + A) / 2⌋` si existen las dos, la que exista si solo hay una, y 0
en la primera celda. El dq de la celda es la predicción más el valor leído, recortado a ±64.

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
visita que en la v1. Los modelos nuevos (`rect` y `rect_dir`, y detrás los de las secciones 1.3 y
3.5) van **al final** de ese orden, así que el prefijo coincide con la lista de modelos de la v1.
Los que la tabla no cubre (entre ellos `smooth` y `smooth_k`) empiezan en p = 1/2.

Tras los demás modelos de la v2 van los de los coeficientes (sección 2.4): `last: [2·5·11]` y
`rem_n: Uint[2·5·7]` del conjunto DCT y después los mismos del de transform skip. Si la tabla no
llega a cubrir todos los modelos, `last` empieza a ½ y cada `rem_n[(pc·5 + fc)·7 + r]` con el
estado de `rem[pc·5 + fc]` del mismo conjunto, ya inicializado con la tabla.

## 6. Pendiente antes de congelar

- Vectores de conformidad v2 (incluido uno con metadatos y vista previa).
- Fijar en este texto la parte angular y CCLM con fórmulas completas, como en la v1.
- Decidir si se añaden más herramientas a la v2 antes de congelarla.
