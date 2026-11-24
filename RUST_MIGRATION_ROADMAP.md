# Ruta de Migración de Playzi a Rust (App de Escritorio Independiente)

Objetivo: transformar **Playzi** de un servidor local Python + pestaña de navegador (~350–600 MB RAM) en una **aplicación de escritorio independiente en Rust** (`.exe` en Windows / binario nativo en Linux), reutilizando al máximo lo que ya funciona hoy.

---

## Principios de la migración

1. **Sin pérdida de datos**: compatibilidad 100% con `data/state.json`, `.env`, `media/` y `data/private/playzi_browser_cookies.txt`.
2. **Núcleo desacoplado de la UI**: usar un **Cargo Workspace** (`crates/playzi-core` + `crates/playzi-desktop`) para que toda la lógica en Rust sirva tanto en Tauri v2 como en una futura UI 100% nativa (`iced`/`egui`).
3. **Pragmatismo con YouTube**: usar clientes nativos (`rustypipe` / InnerTube con `reqwest`) para búsquedas, canales y YouTube Music, y mantener `yt-dlp` + `ffmpeg` como binarios externos/sidecar para descargas y fallback de streams protegidos.
4. **Cero Python y cero Playwright en producción**: eliminar el servidor FastAPI/Uvicorn y el Chromium duplicado de Playwright (~400 MB).

---

## Arquitectura objetivo (Cargo Workspace)

```text
playzi/
├── Cargo.toml                  # Workspace root
├── crates/
│   ├── playzi-core/            # Lógica pura en Rust (compartible entre UIs)
│   │   ├── config.rs           # Lectura/escritura atómica de .env (RwLock)
│   │   ├── state.rs            # Persistencia compatible con data/state.json
│   │   ├── cookies.rs          # Parser Netscape para data/private/
│   │   ├── youtube.rs          # Búsquedas, canales, radio (rustypipe + fallback)
│   │   ├── ytmusic.rs          # Cliente InnerTube con reqwest
│   │   ├── downloads.rs        # Cola acotada (Semaphore) + yt-dlp / ffmpeg
│   │   ├── player.rs           # Resolución de streams + control libmpv / mpv
│   │   ├── autosync.rs         # Scheduler Tokio en segundo plano
│   │   └── thumbs.rs           # Pool HTTP único + caché en disco (data/thumbs/)
│   └── playzi-desktop/         # App de escritorio independiente (Tauri v2)
└── static/                     # Frontend actual reutilizado y optimizado
```

---

## Fases de implementación

### Fase 0 — Estabilización y preparación sobre el proyecto actual (1–2 días)
Objetivo: corregir bugs reales y dejar el frontend/backend listos para convivir y migrar sin deuda técnica.
- [x] Regenerar `.venv` limpio con Python 3.15 y verificar suite de tests (`7/7 OK`).
- [x] Corregir carga faltante de `static/vendor/video-js.min.css` y eliminar `shaka-player.compiled.min.js` (646 KB sin uso).
- [x] Optimizar CSS: quitar `backdrop-filter: blur(4px)` de las cards, reemplazar `transition: all`, agregar `content-visibility: auto` y `@media (prefers-reduced-motion: reduce)`.
- [x] Corregir leaks y polling en `static/app.js`: TTL/limpieza en `_prefetchedUrls` y pausar polling cuando `document.hidden`.
- [x] Corregir bugs de backend: imports dinámicos de `config` (`state.py`, `youtube.py`), locks en cachés (`main.py`, `web_player.py`), pool HTTP en `/api/thumbnail`, cierre de `proc.stderr` en FFmpeg, validación estricta en `/api/player/proxy`, cookies en `web_player.py` y semáforo de concurrencia en `downloads.py`.

### Fase 1 — `playzi-core`: Configuración, Estado y CRUD local (3–4 días)
Migrar módulos sin red ni subprocesos:
- [x] `backend/config.py` -> `crates/playzi-core/src/config.rs` (con `RwLock` y escritura atómica de `.env`)
- [x] `backend/state.py` -> `crates/playzi-core/src/state.rs` (escritura atómica `.tmp`, respaldo `.json.bak`, `#[serde(flatten)]` sin pérdida de claves)
- [x] `backend/collections.py`, `backend/history.py`, `backend/playlists.py` -> módulos CRUD puros con `serde`
- [x] Tests de compatibilidad leyendo y escribiendo un `data/state.json` real existente (`cargo test --workspace`: 5/5 OK).

### Fase 2 — `playzi-core`: YouTube, YouTube Music, Cookies y Descargas (1 semana)
Migrar lógica de red y tareas en segundo plano:
- [x] `backend/youtube.py`, `backend/channels.py`, `backend/channel_browse.py` -> `crates/playzi-core/src/youtube.rs` (cliente InnerTube con `reqwest`, caché TTL, búsquedas, metadata, radio mix y navegación/feeds de canales).
- [x] `backend/ytmusic.py` + `backend/_cookies.py` -> `crates/playzi-core/src/ytmusic.rs` + `crates/playzi-core/src/cookies.rs` (parser Netscape, caché privado, firma `SAPISIDHASH` y cliente InnerTube `WEB_REMIX`).
- [x] `backend/downloads.py` -> `crates/playzi-core/src/downloads.rs` (cola acotada con `tokio::sync::Semaphore`, eventos de progreso en tiempo real vía `broadcast`, cancelación y ejecución de `yt-dlp` / `ffmpeg`).
- [x] Caché de miniaturas en disco (`data/thumbs/`) -> `crates/playzi-core/src/thumbs.rs` (pool HTTP único, allowlist de hosts, escritura atómica y poda por límite).
- [x] `backend/autosync.py` -> `crates/playzi-core/src/autosync.rs` (scheduler Tokio periódico con bloqueo no solapado, backoff de reintentos y eventos `SyncEvent` vía `broadcast`).

### Fase 3 — App de escritorio independiente con Tauri v2 (4–5 días)
- [x] Inicializar `crates/playzi-desktop` con **Tauri v2** (`tauri.conf.json`, `capabilities/default.json`, `build.rs` sincronizando `static/` y generación de íconos PNG/ICO).
- [x] Exponer `playzi-core` mediante `#[tauri::command]` (`api_request`, `get_bootstrap`, `get_state_summary`, `open_auth_window`, `save_browser_cookies`), protocolo `playzi-thumb://` y eventos nativos (`download-progress`, `sync-status`, `auth-status`).
- [x] Adaptar `api.get/post/patch/delete` en `static/app.js` para usar `window.__TAURI__.core.invoke` en modo escritorio (eliminando el servidor HTTP local y el polling mediante `window.__TAURI__.event.listen`).
- [x] Reemplazar Playwright por una ventana secundaria nativa del WebView (`auth-google`) para iniciar sesión en Google y guardar cookies Netscape directamente (`save_browser_cookies`).
- [x] **Hito**: compilar ejecutable release independiente `.exe` (`target/release/playzi-desktop.exe`, ~16.0 MB) y validar build limpio sin errores ni warnings.

### Fase 4 — Reproductor nativo acelerado por hardware (`libmpv` / `mpv`)
- [x] `crates/playzi-core/src/player.rs`: resolución de streams y playlists vía `yt-dlp --dump-single-json` con caché TTL + expiración y cookies Netscape; controlador nativo `mpv` con `--hwdec=auto-safe`, servidor JSON IPC (`--input-ipc-server`), soporte `--wid`, auto-detección de rutas estándar en Windows (`C:\Program Files\MPV Player\mpv.exe`), validación estricta de `MEDIA_ROOT`, eventos `PlayerEvent` y detección de capacidades (`mpv`/`vlc`/`yt-dlp`).
- [x] Integración en `crates/playzi-desktop`: `AppRuntime.player`, puente de eventos nativos `"player-state"`, comandos `player_status`/`player_control`/`get_main_window_hwnd`, rutas `/api/play` y `/api/player/*`, y controles multimedia del sistema operativo con `souvlaki` (`crates/playzi-desktop/src/media_controls.rs`: SMTC en Windows / MPRIS en Linux).
- [x] Frontend `static/app.js`: escucha de `"player-state"`, detección de `capabilities.mpvAvailable` para reproducción directa en `mpv` (GPU) y barra de controles nativos IPC (`togglePause`, `seek`, `setMute`, `stop`).
- [x] Instalar `mpv` (`winget install shinchiro.mpv`, v0.42.0) y verificar reproducción real acelerada por GPU (`d3d11va` + `vo/gpu-next` + `wasapi`), pipe IPC en Windows (`\\.\pipe\playzi-mpv-*`) y embebido con `--wid`.

### Fase 5 (Opcional) — UI 100% Nativa en Rust (sin WebView)
- Reemplazar la capa visual de Tauri por `iced` o `egui` manteniendo intacto `crates/playzi-core`.
- Objetivo final: **~45–80 MB de RAM total** con video 1080p reproduciendo en GPU.

---

## Estado del entorno actual

- **Rust**: `rustc 1.99.1` / `cargo 1.99.1` (instalado)
- **FFmpeg**: `9.0.2` (instalado)
- **Python (soporte actual)**: `3.15.2` con `.venv` limpio y tests pasando
- **MPV**: `v0.42.0` (`shinchiro.mpv` en `C:\Program Files\MPV Player\mpv.exe`, `d3d11va` / `vulkan` / `gpu-next`)
