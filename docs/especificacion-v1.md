# Especificación del formato Tesela, versión 1

Estado: **congelada**. Cualquier cambio que altere la decodificación de un archivo v1 requiere una
versión nueva del formato (byte de versión 2 o superior).

Este documento describe el formato con el detalle necesario para escribir un decodificador
compatible. Hay dos fuentes normativas además de este texto:

- **Implementación de referencia.** Donde la exactitud depende de la aritmética en coma flotante
  (sección 9), es normativo el orden de operaciones de `src/` en este repositorio.
- **Vectores de conformidad.** Los archivos de `tests/vectors/` con sus hashes. Un decodificador
  es conforme si produce exactamente esos píxeles.

## 1. Convenciones

- **Enteros** en *little-endian*. `u8`, `u16`, `u32`: sin signo de 8, 16 y 32 bits.
- **Píxeles**: 8 bits por canal, orden fila (de izquierda a derecha y de arriba abajo).
- **Divisiones enteras**: `a >> b` es un desplazamiento aritmético. `div_euclid` es la división
  con resto no negativo.
- **Coma flotante**: `f32` y `f64` son IEEE-754 binary32 y binary64.
- **Redondeo**: `round(x)` redondea al entero más cercano, y en caso de empate se aleja del cero
  (el comportamiento de `f32::round` en Rust). `clamp(x, a, b)` limita x al intervalo [a, b].
- **Tamaños de bloque**: N ∈ {4, 8, 16, 32, 64}, con `size_index(N)` = 0, 1, 2, 3, 4.

## 2. Contenedor

```
+-------------------+------------------------------+----------------------+-----------------+
| cabecera (23 B)   | [vista previa: u32 + datos]  | tabla de teselas     | cargas útiles   |
+-------------------+------------------------------+----------------------+-----------------+
```

### 2.1 Cabecera

| Desplazamiento | Tipo | Campo | Valores |
|---|---|---|---|
| 0 | 3 × u8 | firma | `"TSL"` (0x54 0x53 0x4C) |
| 3 | u8 | versión | **1** |
| 4 | u32 | ancho | 1 ..= 65536 |
| 8 | u32 | alto | 1 ..= 65536 |
| 12 | u8 | `subsample` | 0 = croma 4:4:4, 1 = croma 4:2:0 |
| 13 | u16 | `qstep` | paso base de cuantización × 64 (≥ 1) |
| 15 | u8 | `chroma_q` | factor de croma × 32 (≥ 1) |
| 16 | u8 | `qm_luma` | pendiente de la matriz de cuantización de luma × 16 |
| 17 | u8 | `qm_chroma` | ídem para croma |
| 18 | u8 | `aq` | 0 o 1: hay cuantización adaptativa |
| 19 | u8 | reservado | **0** |
| 20 | u16 | `tile` | lado de la tesela / 64 (≥ 1) |
| 22 | u8 | `tools` | bits de herramientas (tabla siguiente) |

| Bit de `tools` | Nombre | Significado |
|---|---|---|
| 1 | `SCREEN` | Herramientas de contenido sintético: paleta, IBC y transform skip |
| 2 | `LOSSLESS` | Sin pérdida: planos G, B y R con residuo exacto |
| 4 | `ALPHA` | Hay plano alfa |
| 8 | `PREVIEW` | Hay vista previa tras la cabecera |

Un decodificador v1 **debe rechazar** el archivo en cualquiera de estos casos:
- La versión es distinta de 1 o la firma no es `"TSL"`.
- Hay bits de `tools` desconocidos.
- Algún campo reservado o booleano tiene un valor no permitido.
- `qstep`, `chroma_q` o `tile` valen 0.

Los archivos con `"TSL1"` en texto (4.º byte 0x31) son de desarrollo y no son v1.

### 2.2 Vista previa (opcional)

Si `tools & PREVIEW`, la cabecera va seguida de un `u32` con la longitud L y de L bytes que forman
un **archivo Tesela v1 completo e independiente**. Es una versión reducida de la imagen, y no puede
contener a su vez una vista previa. Un decodificador puede ignorarla.

### 2.3 Teselas

La imagen se divide en teselas de `T = tile × 64` píxeles, en orden raster. La tesela (i, j) cubre
`x ∈ [j·T, min((j+1)·T, ancho))` e `y ∈ [i·T, min((i+1)·T, alto))`.

**Tabla de teselas.** Para cada tesela, en orden, un `u32` con la longitud de su carga útil. Las
cargas útiles siguen en el mismo orden.

**Independencia.** Cada tesela es independiente: tiene su propio flujo aritmético y sus propios
contextos, y su predicción no lee píxeles de otras teselas. Se decodifica como una imagen de
`w × h` (el tamaño de la tesela) y el resultado se copia en su posición.

## 3. Decodificador aritmético

**Inicialización.** `range = 0xFFFF_FFFF`, `code = 0`. Se leen 5 bytes:
`code = (code << 8) | byte`. Leer más allá del final de la carga útil devuelve 0.

**Decodificar un bit** con probabilidad `p1` (probabilidad de 1, en unidades de 2⁻¹⁶):

```
bound = (range >> 16) * p1
si code < bound: range = bound;                      bit = 1
si no:           code -= bound; range -= bound;      bit = 0
mientras range < 2^24: range <<= 8; code = (code << 8) | siguiente_byte
```

Un bit *bypass* usa `p1 = 32768`.

### 3.1 Modelo adaptativo

Cada contexto guarda `fast`, `slow` (probabilidades de 1 en 2⁻¹⁶) y `n` (observaciones).

- `p1 = clamp((fast + slow) >> 1, 48, 65488)`.
- **Actualización con el bit b:**
  - `sf = min(n+1, 4)` y `ss = min(n+1, 7)`.
  - Si b = 1: `fast += (65536 − fast) >> sf` y `slow += (65536 − slow) >> ss`.
  - Si b = 0: `fast −= fast >> sf` y `slow −= slow >> ss`.
  - Después, si `n < 30`, `n += 1`.
- **Estado inicial sin entrenar:** `fast = slow = 32768`, `n = 0`.
- **Estado inicial entrenado** (sección 4): `fast = slow = clamp(t, 48, 65488)`, `n = 3`.

### 3.2 Binarizaciones

- **`Uint`** (Exp-Golomb adaptativo; 24 modelos de prefijo y 24 de mantisa). Para el valor v se
  toma `x = v + 1` con k = posición del bit más alto de x.
  1. **Prefijo:** bits `prefix[i]` (i = 0, 1, …) mientras valgan 1, hasta un máximo de k = 23.
  2. **Mantisa:** si k > 0, el primer bit de mantisa se decodifica con `mant[k]` y los k − 1
     restantes son bypass, del más significativo al menos.
  3. El resultado es x − 1.
- **`Sint`** (modelos `zero`, `sign` y un `Uint`).
  1. Se decodifica `zero`; si es 0, el valor es 0.
  2. Si no, `sign` (1 = negativo) y luego `magnitud = Uint + 1`.
- **`Tree(b)`**: un símbolo de b bits como árbol binario con 2^b modelos indexados por nodo. Se
  empieza en el nodo 1 y en cada paso `nodo = 2·nodo + bit`.

## 4. Contextos e inicialización

Todos los contextos de una tesela se crean al empezarla.

**Cuándo se usan tablas entrenadas.** Si la imagen no es sin pérdida, los modelos de color se
inicializan con la tabla `TABLES[screen][banda]` de `src/init_probs.rs`, que es **normativa**:
- `screen = (tools & SCREEN) ≠ 0`.
- `banda` = 0 si `qstep/64 < 2.5`, 1 si `< 6`, 2 si `< 16`, y 3 en otro caso.

Los valores se asignan recorriendo los modelos en el orden de la sección 4.1. El plano alfa y el
modo sin pérdida usan contextos **sin entrenar**.

### 4.1 Orden de los modelos

Cada `Uint` aporta 24 + 24 modelos, cada `Sint` aporta 2 + 48 y cada `Tree(b)` aporta 2^b. En las
tablas, `pc` es la clase de plano (0 = luma o G o alfa, 1 = croma o B/R) y `sz` es `size_index`.

1. **Coeficientes** (DCT):
   - `cbf[2·5·3]`;
   - `eob: Uint[2·5]`;
   - `sig[2·5·5·6]`, `gt1[300]` y `gt2[300]`;
   - `rem: Uint[2·5]`;
   - `dc_sign[2]`.
2. `split[2·4·4]`.
3. `mpm_flag[2·5]`.
4. `mpm_idx[2][2]`.
5. `rem_mode: Tree(5)[2]`.
6. `cclm[5]`.
7. `dq: Sint`.
8. **Restauración:**
   - `on[2]` y `class_on[2]`;
   - `coef: Sint[2][8]`;
   - `flag[2][3]`.
9. **Coeficientes con transform skip:** la misma estructura que en el punto 1.
10. `ts[2·3]`.
11. `special[2·5·3]`.
12. `pal_or_ibc[2]`.
13. **Paleta:**
    - `count: Tree(3)[2]` y `reuse[2]`;
    - `first: Uint[2]` y `delta: Uint[2]`;
    - `rank[2·6·4]`.
14. **Vectores de bloque:** `same[2]`, `dx: Sint[2]` y `dy: Sint[2]`.

Total: 5257 modelos.

## 5. Estructura de una tesela

Los planos se decodifican en este orden:

1. **Planos de color.**
   - **Con pérdida:** Y (pc 0), Cb (pc 1) y Cr (pc 1). Con `subsample = 1` los planos de croma
     miden `⌈w/2⌉ × ⌈h/2⌉`.
   - **Sin pérdida:** G (pc 0), B (pc 1) y R (pc 1), todos de `w × h`.
2. **Sintaxis de restauración** de los tres planos (sección 8), en el mismo orden.
3. **Plano alfa**, si `tools & ALPHA`: pc 0, contextos nuevos sin entrenar, sin pérdida,
   herramientas sintéticas activas, sin AQ ni CCLM.

**Superbloques.** Cada plano se rellena hasta múltiplos de 64 y se recorre en superbloques de
64×64 en orden raster. En el plano Y, si `aq = 1`, cada superbloque empieza con los `dq` de sus
cuatro celdas de 32×32 (orden raster dentro del superbloque), cada uno codificado como
`Sint(dq)` y recortado a [−64, 64]. Después viene su árbol (sección 6).

**Estado por plano.** El codificador y el decodificador mantienen, desde el inicio de cada plano:
- un **mapa por unidad de 4×4** con disponibilidad, modo (o marca), lado menor del bloque y si
  tiene coeficientes;
- la **última paleta** (inicialmente ninguna);
- el **último vector de bloque** (inicialmente (0, 0)).

## 6. Sintaxis del árbol y de las hojas

### 6.1 Árbol

Un nodo de lado N > 4 empieza con un bit `split`. El contexto es
`split[(pc·4 + size_index(N) − 1)·4 + 2·L + A]`, donde L (o A) vale 1 si el vecino izquierdo (o
superior) está disponible y su lado menor es menor que N. Si `split` = 1, los cuatro hijos de lado
N/2 se recorren en el orden arriba-izquierda, arriba-derecha, abajo-izquierda y abajo-derecha. Un
nodo de lado 4 es siempre hoja.

**Vecinos.** El vecino izquierdo es la unidad de 4×4 que contiene (x0 − 1, y0) y el superior la
que contiene (x0, y0 − 1). Solo cuentan si están dentro del plano y ya decodificados.

### 6.2 Cabecera de la hoja

**Clase especial.** Si `tools & SCREEN`:
1. Se decodifica `special[(pc·5 + sz)·3 + s]`, donde s es el número de vecinos (izquierdo y
   superior) cuya marca es ≥ 200.
2. Si vale 1, se decodifica `pal_or_ibc[pc]`: 1 = paleta (6.4) y 0 = IBC (6.5).

**Modo intra.** Si la hoja no es especial:
1. **CCLM.** Solo en croma: si el bit `cclm[sz]` vale 1, el modo es CCLM (35).
2. **Lista MPM.** Sean L y A los modos de los vecinos, contando como DC (1) los que no existen y
   las marcas ≥ 35.
   - Si L = A y L < 2: MPM = [planar, DC, vertical (26)].
   - Si L = A y L ≥ 2: MPM = [L, 2 + (L+29) mod 32, 2 + (L−1) mod 32].
   - Si L ≠ A: MPM = [L, A, t], donde t es planar si ninguno de los dos es planar; si no, DC si
     ninguno es DC; y si no, vertical.
3. **Elección.**
   - Bit `mpm_flag[pc·5 + sz]`. Si vale 1, `mpm_idx[pc][0]` elige MPM[0] (si vale 0) o pasa a
     `mpm_idx[pc][1]`, que elige MPM[1] (0) o MPM[2] (1).
   - Si `mpm_flag` vale 0, el modo es un `Tree(5)` en `rem_mode[pc]`. Para obtenerlo, se suma 1
     al valor decodificado por cada MPM (en orden ascendente) que sea ≤ que el valor acumulado.

**Transform skip (`ts`).** Si el archivo es sin pérdida, `ts = 1` siempre. Si no, con `SCREEN` y
N ≤ 16 se decodifica `ts[pc·3 + min(sz, 2)]`. En otro caso, `ts = 0`.

**Marca en el mapa.** El modo intra (0..35); 200 para paleta y 201 para IBC.

### 6.3 Coeficientes

**Contextos.** Se usa el conjunto de coeficientes de transform skip si `ts`, o el de DCT en caso
contrario.

**Barrido.** Diagonal: para d = 0, 1, …, y para y = 0..N−1 con 0 ≤ d − y < N, la posición
(x = d − y, y). El índice i es la posición en ese orden.

1. **`cbf`.** Contexto `cbf[(pc·5 + sz)·3 + c]`, donde c es cuántos vecinos (izquierdo y
   superior) tienen coeficientes. Si vale 0, todos los niveles son 0.
2. **`eob`.** `eob = Uint(eob[pc·5 + sz]) + 1`.
3. **Niveles.** Para i desde eob − 1 hasta 0, en la posición (x, y):
   - `fc` = clase de frecuencia según x + y: 0 → 0; 1–2 → 1; 3–5 → 2; 6–10 → 3; ≥ 11 → 4.
   - `mc` = clase de magnitud. Se suma `min(|nivel|, 3)` en (x+1, y), (x, y+1), (x+1, y+1),
     (x+2, y) y (x, y+2) (fuera del bloque cuenta 0), y la suma s se traduce así: 0 → 0; 1 → 1;
     2 → 2; 3–4 → 3; 5–6 → 4; ≥ 7 → 5.
   - Índice de contexto: `k = ((pc·5 + sz)·5 + fc)·6 + mc`.
   - **Significancia.** Si i ≠ eob − 1, bit `sig[k]`; si vale 0, el nivel es 0 y se pasa al
     siguiente. En i = eob − 1 el nivel es no nulo de forma implícita.
   - **Magnitud.** Magnitud m = 1. Si `gt1[k]` vale 1: m = 2, y si además `gt2[k]` vale 1,
     `m = 3 + Uint(rem[pc·5 + fc])`.
   - **Signo.** En (0, 0) con el modelo `dc_sign[pc]`; en las demás posiciones, bypass
     (1 = negativo).

### 6.4 Paleta

**Colores.**
1. **Reutilización.** Si existe una última paleta del plano, se decodifica `reuse[pc]`. Si vale
   1, los colores son los de la última paleta.
2. **Colores nuevos.** Si no:
   - `k = min(Tree(3)(count[pc]) + 2, 8)`.
   - El primer color es `Uint(first[pc])`.
   - Cada color siguiente es `anterior + 1 + Uint(delta[pc])`.
   - Todos los colores se recortan a 255.

La paleta resultante pasa a ser la última del plano.

**Índices.** En orden raster dentro del bloque:
1. **Vecinos.** Sean l, a y al los índices de los vecinos izquierdo, superior y superior-izquierdo
   **dentro del bloque**.
2. **Orden de candidatos.** Se puntúa cada índice: +2 por l, +2 por a y +1 por al. Los k índices
   se ordenan por puntuación descendente y, a igualdad, por índice ascendente.
3. **Clase del patrón.**
   - 0 si no hay l ni a.
   - 1 si solo hay uno de los dos.
   - 2 si l = a = al.
   - 3 si l = a.
   - 4 si l = al.
   - 5 en otro caso.
4. **Rango.** r = 0, 1, …: mientras r + 1 < k, se decodifica `rank[(pc·6 + clase)·4 + min(r, 3)]`;
   si vale 1 se termina, y si vale 0, r += 1.
5. El índice del píxel es `orden[r]`.

**Predicción.** Cada píxel toma el color de su índice.

### 6.5 Copia intra de bloques (IBC)

**Vector.** Se decodifica `same[pc]`. Si vale 1, el vector es igual al último del plano; si no,
`vector = último + (Sint(dx[pc]), Sint(dy[pc]))`. El resultado pasa a ser el último vector.

**Validez.** El vector es válido si:
- la zona de referencia (x0 + dx, y0 + dy, N × N) cae entera dentro del plano relleno;
- el vector no es (0, 0);
- todas las unidades de 4×4 de la zona de referencia están ya decodificadas.

**Predicción.** Se copian los píxeles reconstruidos (sin filtrar) de la zona de referencia. Si el
vector no es válido, la predicción es 128 en todos los píxeles.

## 7. Reconstrucción de una hoja

### 7.1 Paso de cuantización

- `base = qstep / 64` y `cq = chroma_q / 32`.
- **Celda AQ.** Con f = 2 para croma 4:2:0 y f = 1 en otro caso, el bloque cubre las celdas AQ de
  32×32 de luma con índices de `⌊x0·f/32⌋` a `⌈(x0+N)·f/32⌉ − 1` (ídem en y), recortados al mapa.
- `dq = (2·Σdq + cnt) div_euclid (2·cnt)` sobre esas celdas (0 si `aq = 0`).
- `q = base · 2^(dq/6)` (en `f32`, con `powf`), multiplicado por `cq` en croma.
- **Matriz de cuantización.** Para la posición (x, y) de un bloque N×N, con s = `qm_luma/16` o
  `qm_chroma/16`: `w(x, y) = 1 + s · (x² + y²)/N²`, donde (x² + y²)/N² se calcula como
  `(√(x²+y²)/N)²`.
- **Descuantización.**
  - Con DCT: `c = nivel · q · w(x, y)`.
  - Con `ts`: `c = nivel · q`.

En sin pérdida y en el alfa, q = 1.

### 7.2 Residuo

- **Con `ts`:** el residuo es c directamente (dominio de píxel).
- **Con DCT:**
  - Si solo el coeficiente (0, 0) es no nulo, el residuo es constante, `c(0,0) / N`.
  - En otro caso se aplica la DCT-II inversa ortonormal 2-D (sección 9).

### 7.3 Predicción intra

**Referencias.** Para un bloque en (x0, y0) de lado N:
1. Se toman 2N muestras a la izquierda (de abajo arriba, incluida la parte inferior izquierda),
   la esquina y 2N muestras arriba (incluida la parte superior derecha).
2. Las muestras no disponibles se sustituyen como en HEVC: si ninguna está disponible, todas
   valen 128; si no, las anteriores a la primera disponible copian su valor y cada no disponible
   posterior copia la anterior.

**Filtrado** [1, 2, 1] de las referencias. Se aplica si N ≥ 8, el modo no es DC y además:
- el modo es planar, o
- `min(|modo − 26|, |modo − 10|)` es mayor que 7 (N = 8), 1 (N = 16) o 0 (N ≥ 32).

**Modos** (idénticos a HEVC, en enteros):

| Modo | Nombre | Predicción |
|---|---|---|
| 0 | Planar | `((N−1−x)·L[y] + (x+1)·T[N] + (N−1−y)·T[x] + (y+1)·L[N] + N) >> (log2 N + 1)` |
| 1 | DC | `(Σ T[0..N) + Σ L[0..N) + N) >> (log2 N + 1)` |
| 2..34 | Angulares | Tabla de ángulos de HEVC (2–17 horizontales, 18–34 verticales), con extensión de la referencia por el ángulo inverso e interpolación `((32−f)·r[i] + f·r[i+1] + 16) >> 5` |
| 35 | CCLM | Sección 7.4 |

En la tabla, T y L son las referencias superior e izquierda.

### 7.4 CCLM

**Luma de referencia.** Es el plano de luma reconstruido (sin filtrar). En 4:2:0 se usa
`(a+b+c+d+2) >> 2` sobre 2×2. Las coordenadas se recortan al tamaño del plano.

**Parámetros.** Con las N muestras superiores y las N izquierdas que estén disponibles (luma L y
croma C, en `f64`):

```
den = n·ΣLL − (ΣL)²
alpha = clamp((n·ΣLC − ΣL·ΣC) / den, −4, 4)    si den > 1e-6; si no, 0
beta  = (ΣC − alpha·ΣL) / n
```

Sin vecinos, alpha = 0 y beta = 128.

**Predicción.** `round(clamp(alpha·Lref + beta, 0, 255))`.

Si el plano de luma no existe (alfa), el modo 35 se trata como DC.

### 7.5 Suma

`píxel = clamp(round(pred + residuo), 0, 255)`, en `f32`.

## 8. Filtro de restauración

Tras los tres planos de color, y para cada uno, se lee:

1. **Activación.** Bit `on[pc]`. Si vale 0, el plano no se filtra.
2. **Filtros por clase.** Para cada una de las 12 clases, bit `class_on[pc]`. Si vale 1, se leen
   8 coeficientes `Sint(coef[pc][j])`, recortados a [−64, 64].
3. **Superbloques.** Para cada bloque de 64×64 del plano relleno, en orden raster, un bit
   `flag[pc][izq + arriba]` (izq y arriba son las banderas ya leídas de los bloques vecinos).

**Clasificación** de cada bloque de 4×4, sobre el plano sin filtrar y con borde replicado:
- `sh = Σ|2p − izq − der|` y `sv = Σ|2p − arriba − abajo|`.
- `act = (sh + sv) / 16` → nivel de actividad 0 (0–1), 1 (2–4), 2 (5–11) o 3 (≥ 12).
- Dirección: 1 si `sh > 2·sv`, 2 si `sv > 2·sh` y 0 en otro caso.
- Clase = 3·actividad + dirección.

**Filtro**, en los bloques activos cuya clase tiene filtro:
- Desplazamientos: (1,0), (2,0), (3,0), (0,1), (0,2), (0,3), (1,1) y (1,−1).
- `g_k = p(+d_k) + p(−d_k) − 2·p`, leídos del plano sin filtrar con borde replicado.
- `p' = clamp(p + ((Σ f_k·g_k + 64) >> 7), 0, 255)`.

## 9. Transformada inversa (normativa)

La DCT-II ortonormal 2-D se aplica primero por filas y después por columnas; el orden de
operaciones en `f32` es el de `src/dct.rs`:

- **N = 4 y N = 8:** producto por la matriz Mᵀ, con
  `M[k][i] = s_k·cos(π(2i+1)k/2N)`, `s_0 = √(1/N)` y `s_k = √(2/N)`. M se calcula en `f64` y se
  redondea a `f32`.
- **N ≥ 16:** algoritmo recursivo de Lee (DCT-III), con los factores `1/(2·cos((i+½)π/n))`
  calculados en `f64` y redondeados a `f32`.
  - Escalado previo: coeficiente 0 × `√(1/N)` y los demás × `√(2/N)`.
  - En la pasada de filas se saltan las filas cuyos coeficientes son todos 0.

## 10. Color y salida

**Con pérdida.**
- **Croma 4:2:0.** Se interpola a resolución completa con muestras centradas: primero en
  horizontal, `0.75·c[⌊x/2⌋] + 0.25·c[vecino]` (el vecino es la muestra anterior si x es par y la
  siguiente si es impar, recortada al plano), y después igual en vertical.
- **Conversión a RGB** (BT.601, rango completo), con u = Cb − 128 y v = Cr − 128:
  - `R = Y + 1.402·v`
  - `G = Y − 0.344136·u − 0.714136·v`
  - `B = Y + 1.772·u`
  - Cada canal se redondea y se recorta a [0, 255].

**Sin pérdida.** R, G y B se toman directamente de los planos R, G y B.

**Alfa.** El plano alfa es el canal A. Sin `ALPHA`, A = 255.

## 11. Conformidad

`tests/vectors/manifest.txt` lista cada archivo de prueba con su tamaño y el hash FNV-1a de
64 bits de los píxeles RGBA decodificados (offset `0xcbf29ce484222325`, primo `0x100000001b3`).
Un decodificador es conforme con la v1 si reproduce todos los hashes. La implementación de
referencia lo comprueba:
- en nativo, con `cargo test`;
- en WebAssembly, con `node web/test-vectors.mjs`.
