from __future__ import annotations

import json
import threading
import time
import atexit
from copy import deepcopy
from pathlib import Path
from typing import Any

from .config import STATE_FILE, ensure_dirs


_LOCK = threading.RLock()
_CACHE: dict[str, Any] | None = None
_CACHE_FILE: Path | None = None
_DIRTY = False
_LAST_WRITE = 0.0
_WRITE_INTERVAL_SECONDS = 1.25
_VOLATILE_TIMER: threading.Timer | None = None
MAX_HISTORY = 300
MAX_FEED = 200
MAX_LIBRARY = 1000
MAX_DOWNLOADS = 250
DEFAULT_STATE: dict[str, Any] = {
    "downloads": [],
    "library": [],
    "history": [],
    "channels": [],
    "feed": [],
    "collections": [],
    "playlists": [],
    "recommendations": [],
    "recommendationsLastSync": "",
    "musicRecommendations": [],
    "musicRecomLastSync": "",
    "musicPlaylists": [],
    "musicPlaylistsLastSync": "",
    "musicLibrary": {
        "songs": [],
        "liked": [],
        "albums": [],
        "artists": [],
        "playlists": [],
        "lastSync": "",
        "error": "",
    },
    "musicHistory": [],
    "importedPlaylists": [],
    "syncStatus": {
        "running": False,
        "section": "",
        "lastAttempt": "",
        "lastSuccess": "",
        "lastError": "",
        "usedStaleCookies": False,
        "warning": "",
        "sections": {},
    },
}


def _clone(value: Any) -> Any:
    return deepcopy(value)


def _state_file_changed() -> bool:
    return _CACHE_FILE != STATE_FILE


def _read_unlocked() -> dict[str, Any]:
    global _CACHE, _CACHE_FILE
    ensure_dirs()
    if _CACHE is not None and not _state_file_changed():
        return _CACHE
    if not STATE_FILE.exists():
        STATE_FILE.write_text(json.dumps(DEFAULT_STATE, indent=2), encoding="utf-8")
        _CACHE = _clone(DEFAULT_STATE)
        _CACHE_FILE = STATE_FILE
        return _CACHE
    try:
        data = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        backup = STATE_FILE.with_suffix(".json.bak")
        STATE_FILE.replace(backup)
        data = _clone(DEFAULT_STATE)
    for key, value in DEFAULT_STATE.items():
        data.setdefault(key, _clone(value))
    _trim_state(data)
    _CACHE = data
    _CACHE_FILE = STATE_FILE
    return data


def _trim_state(data: dict[str, Any]) -> None:
    data["history"] = list(data.get("history", []))[:MAX_HISTORY]
    data["musicHistory"] = list(data.get("musicHistory", []))[:MAX_HISTORY]
    data["feed"] = list(data.get("feed", []))[:MAX_FEED]
    data["library"] = list(data.get("library", []))[:MAX_LIBRARY]
    data["downloads"] = list(data.get("downloads", []))[:MAX_DOWNLOADS]


def _write_unlocked(data: dict[str, Any]) -> None:
    global _DIRTY, _LAST_WRITE
    ensure_dirs()
    _trim_state(data)
    tmp = Path(str(STATE_FILE) + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")
    tmp.replace(STATE_FILE)
    _DIRTY = False
    _LAST_WRITE = time.monotonic()


def _schedule_flush_unlocked() -> None:
    global _VOLATILE_TIMER
    if _VOLATILE_TIMER and _VOLATILE_TIMER.is_alive():
        _VOLATILE_TIMER.cancel()
    _VOLATILE_TIMER = threading.Timer(_WRITE_INTERVAL_SECONDS, flush_state)
    _VOLATILE_TIMER.daemon = True
    _VOLATILE_TIMER.start()


def read_state() -> dict[str, Any]:
    with _LOCK:
        return _clone(_read_unlocked())


def read_state_key(key: str) -> Any:
    """Deep-copy a single top-level key. Cheaper than full read_state() for single-array reads."""
    with _LOCK:
        return _clone(_read_unlocked().get(key, []))


def write_state(data: dict[str, Any]) -> None:
    global _CACHE, _CACHE_FILE
    with _LOCK:
        _CACHE = _clone(data)
        _CACHE_FILE = STATE_FILE
        _write_unlocked(_CACHE)


def update_state(mutator, *, volatile: bool = False):
    global _CACHE, _CACHE_FILE, _DIRTY
    with _LOCK:
        data = _read_unlocked()
        result = mutator(data)
        _CACHE = data
        _CACHE_FILE = STATE_FILE
        if volatile and time.monotonic() - _LAST_WRITE < _WRITE_INTERVAL_SECONDS:
            _DIRTY = True
            _schedule_flush_unlocked()
        else:
            _write_unlocked(data)
        return result


def flush_state() -> None:
    with _LOCK:
        if _DIRTY and _CACHE is not None:
            _write_unlocked(_CACHE)


atexit.register(flush_state)
