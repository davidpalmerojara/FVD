# Tesela para escritorio

Aplicación pequeña para **ver y convertir imágenes `.tsl`** sin instalar Rust ni abrir el
navegador. Usa el códec del repositorio en Rust nativo (el mismo que `tesela encode`/`decode`)
y [Tauri 2](https://tauri.app): la interfaz es HTML/CSS/JS sin compilar y se dibuja con el
WebView del sistema (WebView2 en Windows, WebKit en macOS, WebKitGTK en Linux), así que no
lleva un navegador dentro como Electron.

| | Tamaño (0.1.0, medido en Linux x86-64) |
|---|---|
| Ejecutable | 8 MB |
| Paquete `.deb` | 3,1 MB |
| `.AppImage` | 78 MB (incluye WebKitGTK y GTK para funcionar en cualquier distribución) |

## Descarga

En [Releases](https://github.com/davidpalmerojara/tesela/releases):

- **Windows 10/11**: `Tesela_X.Y.Z_x64-setup.exe` (o el `.msi`). Usa WebView2, que ya viene
  con Windows 10/11; si faltara, el instalador lo descarga.
- **macOS 10.15 o posterior** (Apple Silicon e Intel): `Tesela_X.Y.Z_universal.dmg`. Arrastra
  Tesela a Aplicaciones.
- **Linux**: `Tesela_X.Y.Z_amd64.deb` para Debian/Ubuntu (`sudo apt install ./Tesela_*.deb`;
  asocia los `.tsl`) o `Tesela_X.Y.Z_amd64.AppImage` (`chmod +x` y ejecutar; algunas
  distribuciones necesitan `libfuse2`).

`SHA256SUMS.txt` tiene la suma de cada archivo: `sha256sum -c SHA256SUMS.txt --ignore-missing`
(en PowerShell, `Get-FileHash archivo`).

### Aplicación sin firmar: qué verás

Los instaladores **no están firmados** con un certificado de desarrollador, así que el sistema
avisa la primera vez:

- **Windows (SmartScreen)**: «Windows protegió su PC». Pulsa **Más información → Ejecutar de
  todas formas**. El aviso desaparece cuando el archivo gana reputación o se firma.
- **macOS (Gatekeeper)**: «No se puede abrir porque no se puede verificar el desarrollador».
  Haz **clic derecho (o Control+clic) sobre Tesela → Abrir → Abrir**. En macOS 15 o posterior,
  ábrela una vez, ve a **Configuración del Sistema → Privacidad y seguridad** y pulsa **Abrir
  igualmente**. Alternativa en Terminal:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Tesela.app
  ```

  La app lleva una firma *ad hoc* (sin identidad), necesaria para que el binario arranque en
  Apple Silicon, pero no está notarizada.
- **Linux**: no hay aviso; el `.deb` y el `.AppImage` no se firman.

**Qué costaría firmar** (precios orientativos de 2025-2026):

- **macOS**: Apple Developer Program, 99 $/año, permite firmar con «Developer ID» y notarizar;
  con eso Gatekeeper no avisa.
- **Windows**: un certificado de firma de código OV de una autoridad (Sectigo, DigiCert…),
  unos 200-500 €/año y ahora siempre en token físico o HSM en la nube; uno EV, más caro, da
  reputación inmediata en SmartScreen. Más barato: Azure Trusted Signing (unos 10 $/mes), con
  requisitos de verificación de identidad y disponibilidad por país.

El flujo `.github/workflows/release.yml` tiene comentados los pasos y secretos que harían falta
para firmar en ambos sistemas.

## Uso

Tiene dos pestañas: **Visor** y **Convertir** (se cambia con el ratón o con ← → sobre las
pestañas). Todo funciona sin conexión.

### Visor

Abre un `.tsl` de cualquiera de estas formas:

- **Doble clic** en el archivo (el instalador asocia `.tsl` con Tesela) o «Abrir con».
- Desde la línea de órdenes: `tesela-desktop foto.tsl` (en macOS, `open -a Tesela foto.tsl`).
- **Abrir…** (Ctrl+O; ⌘O en macOS).
- **Arrastrando** el archivo a la ventana.

Muestra dimensiones, tamaño del archivo, bits por píxel, número de fotogramas, si tiene
transparencia o es sin pérdida, y el tiempo de decodificación. Las animaciones se reproducen con
la duración de cada fotograma y el número de repeticiones del archivo.

| Tecla | Acción |
|---|---|
| ← / → | `.tsl` anterior / siguiente de la misma carpeta (circular) |
| 0 o F | Ajustar a la ventana |
| 1 | Tamaño real (un píxel de la imagen por píxel de la pantalla) |
| + / − o rueda del ratón | Acercar / alejar (la rueda, hacia el puntero) |
| Arrastrar, o Mayús + flechas | Desplazar la imagen ampliada |
| Doble clic | Alternar entre ajustar y tamaño real |
| Espacio | Reproducir / pausar la animación |
| , / . | Fotograma anterior / siguiente (en pausa) |
| Ctrl+S | **Guardar como PNG** (con el perfil de color y el EXIF del archivo; en una animación, el fotograma visible) |

### Convertir

1. Arrastra imágenes a la ventana o pulsa la zona punteada: **PNG, JPEG, GIF y WebP**,
   también animados (GIF, APNG y WebP animado se guardan como animación `.tsl`).
2. Elige la **calidad** (85 por defecto) o **sin pérdida**.
3. Elige el destino: **junto al original** o **en otra carpeta**.
4. Pulsa **Convertir**. Cada imagen muestra `tamaño original → tamaño .tsl` y el porcentaje
   ahorrado, con botones para verla en el visor o mostrarla en la carpeta.

La codificación se hace fuera del hilo de la interfaz, así que la ventana sigue respondiendo.
Nunca se sobrescribe un archivo: si `foto.tsl` existe, se guarda `foto (2).tsl`.
El resultado es idéntico byte a byte al de `tesela encode foto.png foto.tsl -q 85` (misma
orientación EXIF, mismos metadatos).

## Limitaciones

- No hay gestión de color en pantalla: el perfil ICC no se aplica al mostrar (sí se conserva al
  convertir y al guardar como PNG).
- Una animación que ocupe más de 512 MB descomprimida (fotogramas × píxeles) muestra solo el
  primer fotograma.
- Cada archivo abierto desde el sistema con la aplicación ya abierta abre otra ventana (en macOS
  se reutiliza la misma).
- El `.AppImage` no registra la asociación de archivos; el `.deb`, el `.msi`/`.exe` y el `.dmg`, sí.

## Compilar desde el código

Es un crate aparte (`desktop/`, con su propio `Cargo.lock` y `target/`), así que las
compilaciones del códec y de WebAssembly no dependen de Tauri.

Requisitos: [Rust](https://rustup.rs) estable y, según el sistema,

- **Linux** (Debian/Ubuntu): `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev
  libayatana-appindicator3-dev`.
- **macOS**: Xcode Command Line Tools (`xcode-select --install`).
- **Windows**: Visual Studio Build Tools (C++); WebView2 ya está en Windows 10/11.

```sh
cargo install tauri-cli --version "^2" --locked   # o: npm install -g @tauri-apps/cli@2
cd desktop
cargo tauri dev                 # ejecutar en modo desarrollo
cargo tauri build               # instaladores en target/release/bundle/
cargo tauri build --bundles deb # solo uno
```

No hay paso de compilación del frontend: `ui/` (HTML, CSS, JS y las fuentes Manrope y DM Mono
con sus licencias OFL) se incrusta tal cual en el ejecutable.

Estructura:

- `src/main.rs`: comandos que llama la interfaz (abrir, fotogramas, guardar PNG, convertir,
  diálogos) y la apertura de archivos desde el sistema.
- `src/load.rs`: lectura de PNG/JPEG/GIF/WebP, copiada de `src/main.rs` de la línea de órdenes
  para codificar exactamente igual; si cambia allí, hay que cambiarla aquí.
- `ui/`: la interfaz.
- `icons/icon.svg`: fuente del icono. Los demás tamaños se generan con
  `cargo tauri icon icons/icon.svg` (después se borran `android/`, `ios/`, `Square*` y
  `StoreLogo.png`, que no se usan).
- `linux/`: plantilla `.desktop` (con `%F` para recibir los archivos) y el tipo MIME
  `image/x-tesela` para el `.deb`.

## Publicar una versión

1. Sube la versión en `Cargo.toml`, `desktop/Cargo.toml` y `desktop/tauri.conf.json` (las tres
   iguales; el flujo lo comprueba).
2. `git tag v0.1.0 && git push origin v0.1.0` (o *Actions → Release → Run workflow*).
3. El flujo `release.yml` compila la línea de órdenes para Linux, Windows y macOS (Intel y
   Apple Silicon), los instaladores de la aplicación y `SHA256SUMS.txt`, y lo deja todo en un
   **borrador** de Release. Revísalo y pulsa *Publish release*.
