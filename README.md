# Playzi

Aplicacion de escritorio nativa en **Rust** y **Tauri v2** para explorar, reproducir y descargar contenido de YouTube y YouTube Music. Interfaz en espanol, diseno ligero e independiente del navegador.

## Descarga (Windows)

Descarga el instalador para Windows x64 desde la seccion de Releases:

- **Todas las releases**: https://github.com/Shionebash/Playzi/releases

---

## Caracteristicas

- **Aplicacion de escritorio nativa (Rust + Tauri v2)**: ejecutable independiente sin necesidad de servidor web local ni entorno de Python.
- **YouTube y YouTube Music**: busqueda, exploracion de canales, recomendaciones personalizadas y navegacion por musica.
- **Descargas concurrentes**: descarga de video y audio (`opus`, `mp3`, `m4a`) gestionada en Rust mediante `yt-dlp` y `ffmpeg`.
- **Reproductor integrado y reproductores externos**: reproduccion directa dentro de la app con proxy de streaming, o envio directo a **MPV** / **VLC**.
- **Modo Party (Watch Party)**: salas sincronizadas en tiempo real sobre WebSocket con cola compartida, roles y politicas de control.
- **Controles multimedia nativos del SO**: integracion con teclas multimedia y controles del sistema operativo mediante `souvlaki`.
- **Autenticacion con Google/YouTube desde tu navegador predeterminado**: abre YouTube Music en tu navegador del sistema (Zen, Firefox, Chrome, Edge, Brave, Opera, Vivaldi) y sincroniza la sesion automaticamente.
- **Biblioteca local**: listas de reproduccion, colecciones, historial de reproduccion y cache de miniaturas.
- **Sincronizacion automatica**: actualizacion periodica en segundo plano de suscripciones y secciones.
- **Cola de reproduccion avanzada**: shuffle, repeat (ninguno / uno / todos), radio/autoplay al finalizar y atajos de teclado (`Space` play/pausa, `←/→` seek, `N/P` siguiente/anterior, `S` shuffle, `R` repeat).

---

## Requisitos

### Para ejecutar la app instalada

El instalador de Windows (`.exe` NSIS) verifica e instala automaticamente los requisitos obligatorios (`yt-dlp` y `ffmpeg`) junto con la aplicacion, y te permite elegir durante la instalacion si deseas instalar **MPV** y/o **VLC** como reproductores externos opcionales.

| Herramienta | Requerido | Notas |
|-------------|-----------|-------|
| Windows 10/11 (x64) | Si | Incluye WebView2 para la ventana nativa |
| `yt-dlp` | Si | Se instala automaticamente con el instalador (resolucion de streams y descargas) |
| `ffmpeg` | Si | Se instala automaticamente con el instalador (conversion y mezcla de audio/video) |
| `mpv` / `vlc` | Opcional | Seleccionable durante la instalacion mediante `winget` (reproductor externo) |

### Para compilar desde el codigo fuente

| Herramienta | Notas |
|-------------|-------|
| Rust (`cargo`, edition 2024) | [rustup.rs](https://rustup.rs/) |
| Node.js (para Tauri CLI) | Usado para empaquetar instaladores con `@tauri-apps/cli` |
| Dependencias de Tauri en Linux | `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev` |

---

## Desarrollo y compilacion

```bash
# 1. Clonar el repositorio
git clone https://github.com/Shionebash/Playzi.git
cd Playzi

# 2. Ejecutar tests del workspace en Rust
cargo test --workspace

# 3. Ejecutar la app de escritorio en modo desarrollo
cargo run -p playzi-desktop
```

### Generar el instalador `.exe` (Windows NSIS)

```powershell
cd crates\playzi-desktop
npx @tauri-apps/cli@2 build --bundles nsis
```

El instalador generado quedara en (reemplazando `<version>` por la version actual del proyecto, por ejemplo `1.0.5`):

```text
target/release/bundle/nsis/Playzi_<version>_x64-setup.exe
```

Para generar los paquetes nativos de tu plataforma actual:

```bash
cd crates/playzi-desktop
npx @tauri-apps/cli@2 build
```

---

## Configuracion

Playzi carga su configuracion desde un archivo `.env` en el directorio raiz de datos (o mediante variables de entorno). Tambien puede editarse desde la propia interfaz de la aplicacion.

```env
MEDIA_ROOT=./media        # Carpeta donde se guardan las descargas
DATA_DIR=./data           # Carpeta de estado, cache y cookies

DEFAULT_PLAYER=mpv        # Reproductor externo por defecto: mpv | vlc
MPV_PATH=mpv              # Ruta o ejecutable de mpv
VLC_PATH=vlc              # Ruta o ejecutable de vlc
FFMPEG_PATH=ffmpeg        # Ruta o ejecutable de ffmpeg

AUDIO_FORMAT=opus         # Formato de audio: opus | mp3 | m4a
YTDLP_SEARCH_LIMIT=12     # Resultados por busqueda (1-100)
PLAYBACK_QUALITY=best     # Calidad: best | 1080p | 720p | 480p | 360p

# Opcional: cookies para contenido con sesion
COOKIES_BROWSER=          # Navegador para extraer cookies (chrome, firefox, etc.)
COOKIES_FILE=             # Ruta a un cookies.txt en formato Netscape
```

> Consulta `.env.example` para referencia completa.

---

## Arquitectura del proyecto

El proyecto esta organizado como un **Cargo Workspace** en Rust:

```text
Playzi/
├── Cargo.toml                  # Workspace principal de Rust
├── crates/
│   ├── playzi-core/            # Logica de negocio y persistencia en Rust
│   │   └── src/
│   │       ├── autosync.rs     # Sincronizacion automatica en segundo plano
│   │       ├── collections.rs  # Colecciones locales
│   │       ├── config.rs       # Lectura/escritura de configuracion (.env)
│   │       ├── cookies.rs      # Gestion de cookies Netscape y SAPISIDHASH
│   │       ├── downloads.rs    # Gestor de cola de descargas concurrentes
│   │       ├── history.rs      # Historial de reproduccion
│   │       ├── party.rs        # Salas Watch Party por WebSocket
│   │       ├── player.rs       # Resolucion de streams, proxy y control MPV/VLC
│   │       ├── playlists.rs    # Listas de reproduccion
│   │       ├── state.rs        # Estado persistente de la aplicacion
│   │       ├── thumbs.rs       # Cache de miniaturas
│   │       ├── youtube.rs      # Integracion con YouTube
│   │       └── ytmusic.rs      # Integracion con YouTube Music
│   └── playzi-desktop/         # App de escritorio Tauri v2
│       ├── tauri.conf.json     # Configuracion de ventana y bundle NSIS
│       ├── nsis/               # Hooks NSIS e instalador de requisitos (yt-dlp, ffmpeg, MPV/VLC)
│       ├── build.rs            # Empaquetado de assets de static/ en dist/
│       └── src/
│           ├── main.rs         # Punto de entrada Windows/Desktop
│           ├── lib.rs          # Inicializacion de Tauri, eventos y protocolos
│           ├── commands.rs     # Comandos IPC y ventana de login WebView
│           ├── media_controls.rs # Controles multimedia del SO (souvlaki)
│           └── runtime.rs      # Inicializacion del runtime de playzi-core
├── static/                     # Frontend embebido (HTML/CSS/JS)
├── backend/                    # Backend web legado en Python/FastAPI (referencia)
├── scripts/                    # Scripts legados para el servidor Python
└── RUST_MIGRATION_ROADMAP.md   # Hoja de ruta de la migracion a Rust
```

---

## Licencia

Distribuido bajo licencia **MIT**.
