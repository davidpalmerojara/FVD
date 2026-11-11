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

## 4. Probabilidades iniciales

Los modelos se inicializan con `init_probs_v2::TABLES[sintético][banda]`, en el mismo orden de
visita que en la v1. Los modelos nuevos (`rect` y `rect_dir`) van **al final** de ese orden, así
que el prefijo coincide con la lista de modelos de la v1.

## 5. Pendiente antes de congelar

- Vectores de conformidad v2 (incluido uno con metadatos y vista previa).
- Fijar en este texto la parte angular y CCLM con fórmulas completas, como en la v1.
- Decidir si se añaden más herramientas a la v2 antes de congelarla.
