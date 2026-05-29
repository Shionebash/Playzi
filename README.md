# Playzi

Servidor web local para descargar y reproducir contenido de YouTube y YouTube Music. Interfaz en español, funciona en Windows y Linux.

## Características

- Buscar y descargar videos/audio de YouTube
- Explorar y reproducir YouTube Music
- Gestión de listas de reproducción y colecciones
- Suscripciones a canales con sincronización automática
- Reproductor web integrado (Shaka Player / Video.js)
- Reproducción directa en MPV o VLC
- Autenticación con cuenta de Google via navegador gestionado (Playwright)
- Historial de reproducción y recomendaciones

## Requisitos

| Herramienta | Windows | Linux |
|-------------|---------|-------|
| Python 3.11+ | [python.org](https://www.python.org/downloads/) | `sudo apt install python3 python3-venv` |
| ffmpeg | [ffmpeg.org](https://ffmpeg.org/download.html) | `sudo apt install ffmpeg` |
| MPV (opcional) | [mpv.io](https://mpv.io/installation/) | `sudo apt install mpv` |
| VLC (opcional) | [videolan.org](https://www.videolan.org/) | `sudo apt install vlc` |
| Node.js (opcional) | [nodejs.org](https://nodejs.org/) | `sudo apt install nodejs` |

> Node.js solo se usa si yt-dlp requiere ejecución de JavaScript para ciertos videos.

---

## Instalación y ejecución

### Windows

```powershell
# 1. Clonar repositorio
git clone https://github.com/Shionebash/Playzi.git
cd Playzi

# 2. Setup (crea venv, instala dependencias, genera .env)
.\scripts\windows\setup.ps1

# 3. Arrancar servidor
.\scripts\windows\run.ps1
```

Abrir en el navegador: **http://127.0.0.1:8765**

Para detener: `.\scripts\windows\stop.ps1`

---

### Linux

```bash
# 1. Clonar repositorio
git clone https://github.com/Shionebash/Playzi.git
cd Playzi

# 2. Setup (crea venv, instala dependencias, genera .env)
bash scripts/linux/setup.sh

# 3. Arrancar servidor
bash scripts/linux/run.sh
```

Abrir en el navegador: **http://127.0.0.1:8765**

Para detener: `bash scripts/linux/stop.sh`

---

## Configuración

`setup.ps1` / `setup.sh` genera automáticamente un archivo `.env` con la configuración detectada. Puedes editarlo manualmente:

```env
APP_HOST=127.0.0.1       # Interfaz de red del servidor
APP_PORT=8765            # Puerto

MEDIA_ROOT=/ruta/media   # Dónde se guardan los archivos descargados
DATA_DIR=/ruta/data      # Dónde se guarda el estado y caché

DEFAULT_PLAYER=mpv       # Reproductor por defecto: mpv | vlc
MPV_PATH=mpv             # Ruta o nombre del ejecutable mpv
VLC_PATH=vlc             # Ruta o nombre del ejecutable vlc
FFMPEG_PATH=ffmpeg       # Ruta o nombre del ejecutable ffmpeg

AUDIO_FORMAT=opus        # Formato de audio: opus | mp3 | m4a
YTDLP_SEARCH_LIMIT=12    # Resultados por búsqueda
PLAYBACK_QUALITY=best    # Calidad: best | 1080p | 720p | 480p | 360p

# Opcional - cookies para contenido con login
COOKIES_BROWSER=         # Navegador para extraer cookies: chrome | firefox | etc.
COOKIES_FILE=            # Ruta a un cookies.txt en formato Netscape
```

> Ver `.env.example` para referencia.

---

## Autenticación con YouTube

Para acceder a contenido que requiere login (YouTube Music, recomendaciones personalizadas):

### Windows
```powershell
.\scripts\windows\login-youtube.ps1
```

### Linux
```bash
bash scripts/linux/login-youtube.sh
```

Se abre un navegador Chromium gestionado. Inicia sesión normalmente en YouTube. Las cookies quedan guardadas en `data/private/`.

---

## Actualizar yt-dlp

Si los downloads fallan, actualizar yt-dlp suele resolverlo:

### Windows
```powershell
.\scripts\windows\update-ytdlp.ps1
```

### Linux
```bash
bash scripts/linux/update-ytdlp.sh
```

---

## Estructura del proyecto

```
Playzi/
├── backend/           # Servidor FastAPI (Python)
│   ├── main.py        # Rutas y app principal
│   ├── config.py      # Configuración desde .env
│   ├── downloads.py   # Cola de descargas
│   ├── player.py      # Integración MPV/VLC
│   ├── youtube.py     # Búsqueda y metadatos
│   ├── ytmusic.py     # YouTube Music API
│   ├── playlists.py   # Listas de reproducción
│   ├── channels.py    # Suscripciones a canales
│   └── ...
├── static/            # Frontend web (HTML/CSS/JS)
├── scripts/
│   ├── windows/       # Scripts PowerShell
│   └── linux/         # Scripts Bash
├── .env.example       # Plantilla de configuración
└── requirements.txt   # Dependencias Python
```

---

## Dependencias Python

```
fastapi
uvicorn[standard]
yt-dlp
ytmusicapi
playwright
python-dotenv
pydantic
httpx
Pillow
```

Instaladas automáticamente por los scripts de setup en el entorno virtual `.venv/`.
