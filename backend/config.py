from __future__ import annotations

import os
import threading
from pathlib import Path

from dotenv import load_dotenv


ROOT = Path(__file__).resolve().parents[1]
ENV_FILE = ROOT / ".env"
load_dotenv(ENV_FILE)


def _path_env(name: str, default: str) -> Path:
    value = os.getenv(name, default).strip().strip('"')
    return Path(value).expanduser()


def _str_env(name: str, default: str) -> str:
    return os.getenv(name, default).strip()


APP_HOST = _str_env("APP_HOST", "127.0.0.1")
APP_PORT = int(_str_env("APP_PORT", "8765"))
MEDIA_ROOT = _path_env("MEDIA_ROOT", str(ROOT / "media")).resolve()
DATA_DIR = _path_env("DATA_DIR", str(ROOT / "data")).resolve()
STATE_FILE = _path_env("STATE_FILE", str(DATA_DIR / "state.json")).resolve()
DEFAULT_PLAYER = _str_env("DEFAULT_PLAYER", "mpv").lower()
MPV_PATH = _str_env("MPV_PATH", "mpv")
VLC_PATH = _str_env("VLC_PATH", "vlc")
FFMPEG_PATH = _str_env("FFMPEG_PATH", "ffmpeg")
AUDIO_FORMAT = _str_env("AUDIO_FORMAT", "opus").lower()
YTDLP_SEARCH_LIMIT = int(_str_env("YTDLP_SEARCH_LIMIT", "12"))
PLAYBACK_QUALITY = _str_env("PLAYBACK_QUALITY", "best")
COOKIES_BROWSER = _str_env("COOKIES_BROWSER", "")
COOKIES_FILE = _str_env("COOKIES_FILE", "")

# YouTube Music web client defaults (public values scraped from music.youtube.com)
YTMUSIC_API_KEY = _str_env("YTMUSIC_API_KEY", "AIzaSyBOti4mM-6x9WDnZIjIeyEU21OpBXqWBgw")
YTMUSIC_CLIENT_VERSION = _str_env("YTMUSIC_CLIENT_VERSION", "1.20240520.01.00")


_CONFIG_LOCK = threading.Lock()


def _set_global(name: str, value: str) -> None:
    os.environ[name] = value
    globals()[name] = value


def update_config(values: dict) -> dict:
    global MEDIA_ROOT, DATA_DIR, STATE_FILE, DEFAULT_PLAYER, MPV_PATH, VLC_PATH, FFMPEG_PATH, AUDIO_FORMAT, YTDLP_SEARCH_LIMIT, PLAYBACK_QUALITY
    with _CONFIG_LOCK:
        return _update_config_locked(values)


def _update_config_locked(values: dict) -> dict:
    global MEDIA_ROOT, DATA_DIR, STATE_FILE, DEFAULT_PLAYER, MPV_PATH, VLC_PATH, FFMPEG_PATH, AUDIO_FORMAT, YTDLP_SEARCH_LIMIT, PLAYBACK_QUALITY, COOKIES_BROWSER, COOKIES_FILE

    env_values = _read_env_file()
    old_cookies_browser = COOKIES_BROWSER
    old_cookies_file = COOKIES_FILE
    mapping = {
        "mediaRoot": "MEDIA_ROOT",
        "defaultPlayer": "DEFAULT_PLAYER",
        "mpvPath": "MPV_PATH",
        "vlcPath": "VLC_PATH",
        "ffmpegPath": "FFMPEG_PATH",
        "audioFormat": "AUDIO_FORMAT",
        "searchLimit": "YTDLP_SEARCH_LIMIT",
        "playbackQuality": "PLAYBACK_QUALITY",
        "cookiesBrowser": "COOKIES_BROWSER",
        "cookiesFile": "COOKIES_FILE",
    }
    # Fields where empty string is a valid value (means "disabled/none")
    _clearable = {"COOKIES_BROWSER", "COOKIES_FILE"}
    for public_key, env_key in mapping.items():
        value = values.get(public_key)
        if value is None:
            continue
        stripped = str(value).strip()
        if stripped == "" and env_key not in _clearable:
            continue  # Don't wipe paths/settings accidentally
        env_values[env_key] = stripped
        os.environ[env_key] = stripped

    _write_env_file(env_values)
    MEDIA_ROOT = _path_env("MEDIA_ROOT", str(ROOT / "media")).resolve()
    DATA_DIR = _path_env("DATA_DIR", str(ROOT / "data")).resolve()
    STATE_FILE = _path_env("STATE_FILE", str(DATA_DIR / "state.json")).resolve()
    DEFAULT_PLAYER = _str_env("DEFAULT_PLAYER", "mpv").lower()
    MPV_PATH = _str_env("MPV_PATH", "mpv")
    VLC_PATH = _str_env("VLC_PATH", "vlc")
    FFMPEG_PATH = _str_env("FFMPEG_PATH", "ffmpeg")
    AUDIO_FORMAT = _str_env("AUDIO_FORMAT", "opus").lower()
    YTDLP_SEARCH_LIMIT = int(_str_env("YTDLP_SEARCH_LIMIT", "12"))
    PLAYBACK_QUALITY = _str_env("PLAYBACK_QUALITY", "best")
    COOKIES_BROWSER = _str_env("COOKIES_BROWSER", "")
    COOKIES_FILE = _str_env("COOKIES_FILE", "")
    ensure_dirs()
    if old_cookies_browser != COOKIES_BROWSER or old_cookies_file != COOKIES_FILE:
        cache = DATA_DIR / "private" / "cookies_cache.txt"
        try:
            if cache.exists():
                cache.unlink()
        except OSError:
            pass
    return public_config()



def _read_env_file() -> dict[str, str]:
    values: dict[str, str] = {}
    if ENV_FILE.exists():
        for line in ENV_FILE.read_text(encoding="utf-8-sig").splitlines():
            if not line.strip() or line.lstrip().startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip()
    return values


def _write_env_file(values: dict[str, str]) -> None:
    order = [
        "APP_HOST",
        "APP_PORT",
        "MEDIA_ROOT",
        "DATA_DIR",
        "STATE_FILE",
        "DEFAULT_PLAYER",
        "MPV_PATH",
        "VLC_PATH",
        "FFMPEG_PATH",
        "AUDIO_FORMAT",
        "YTDLP_SEARCH_LIMIT",
        "PLAYBACK_QUALITY",
    ]
    lines = [f"{key}={values[key]}" for key in order if key in values]
    for key, value in values.items():
        if key not in order:
            lines.append(f"{key}={value}")
    ENV_FILE.write_text("\n".join(lines) + "\n", encoding="utf-8")


def ensure_dirs() -> None:
    MEDIA_ROOT.mkdir(parents=True, exist_ok=True)
    DATA_DIR.mkdir(parents=True, exist_ok=True)


def public_config() -> dict:
    return {
        "appHost": APP_HOST,
        "appPort": APP_PORT,
        "mediaRoot": str(MEDIA_ROOT),
        "dataDir": str(DATA_DIR),
        "defaultPlayer": DEFAULT_PLAYER,
        "players": {"mpv": MPV_PATH, "vlc": VLC_PATH},
        "ffmpeg": FFMPEG_PATH,
        "audioFormat": AUDIO_FORMAT,
        "searchLimit": YTDLP_SEARCH_LIMIT,
        "playbackQuality": PLAYBACK_QUALITY,
        "cookiesBrowser": COOKIES_BROWSER,
        "cookiesFile": COOKIES_FILE,
    }
