from __future__ import annotations

import logging
import threading
import time
from collections.abc import Callable
from typing import Any

from . import config
from .recommendations import fetch_recommendations
from .state import read_state, update_state
from .ytmusic import fetch_music_playlists, fetch_music_recommendations

logger = logging.getLogger(__name__)

RECOMMENDATIONS_INTERVAL_SECONDS = 6 * 60 * 60
PLAYLISTS_INTERVAL_SECONDS = 12 * 60 * 60
FAILED_RETRY_SECONDS = 15 * 60
_TIME_FORMAT = "%Y-%m-%d %H:%M:%S"
_REFRESH_LOCK = threading.Lock()
_START_LOCK = threading.Lock()
_STARTED = False


SectionRunner = Callable[[], dict[str, Any]]

_SECTIONS: dict[str, dict[str, Any]] = {
    "recommendations": {
        "label": "YouTube Para ti",
        "last_sync_key": "recommendationsLastSync",
        "items_key": "recommendations",
        "interval": RECOMMENDATIONS_INTERVAL_SECONDS,
        "runner": fetch_recommendations,
    },
    "musicRecommendations": {
        "label": "YT Music recomendadas",
        "last_sync_key": "musicRecomLastSync",
        "items_key": "musicRecommendations",
        "interval": RECOMMENDATIONS_INTERVAL_SECONDS,
        "runner": fetch_music_recommendations,
    },
    "musicPlaylists": {
        "label": "YT Music playlists",
        "last_sync_key": "musicPlaylistsLastSync",
        "items_key": "musicPlaylists",
        "interval": PLAYLISTS_INTERVAL_SECONDS,
        "runner": fetch_music_playlists,
    },
}


def get_sync_status() -> dict[str, Any]:
    data = read_state()
    return data.get("syncStatus", {})


def refresh_section(section: str) -> dict[str, Any]:
    if section not in _SECTIONS:
        raise ValueError(f"Seccion de sync desconocida: {section}")
    if not _REFRESH_LOCK.acquire(blocking=False):
        raise RuntimeError("Ya hay una sincronizacion en curso.")
    try:
        return _run_section(section)
    finally:
        _REFRESH_LOCK.release()


def refresh_all() -> dict[str, Any]:
    if not _REFRESH_LOCK.acquire(blocking=False):
        raise RuntimeError("Ya hay una sincronizacion en curso.")
    try:
        results = {}
        for section in _SECTIONS:
            results[section] = _run_section(section)
        return {"ok": True, "results": results, "status": get_sync_status()}
    finally:
        _REFRESH_LOCK.release()


def start_autosync() -> None:
    global _STARTED
    with _START_LOCK:
        if _STARTED:
            return
        _STARTED = True
    thread = threading.Thread(target=_autosync_loop, name="playzi-autosync", daemon=True)
    thread.start()


def _autosync_loop() -> None:
    time.sleep(2)
    while True:
        try:
            if config.COOKIES_BROWSER or config.COOKIES_FILE:
                _refresh_due_sections()
        except Exception:
            logger.exception("Error en auto-sync")
        time.sleep(60)


def _refresh_due_sections() -> None:
    if not _REFRESH_LOCK.acquire(blocking=False):
        return
    try:
        data = read_state()
        for section, meta in _SECTIONS.items():
            if _section_is_due(data, meta):
                _run_section(section)
                data = read_state()
    finally:
        _REFRESH_LOCK.release()


def _section_is_due(data: dict[str, Any], meta: dict[str, Any]) -> bool:
    status = data.get("syncStatus", {})
    section_status = status.get("sections", {}).get(_section_name_for_meta(meta), {})
    if section_status.get("lastError") and _recent(section_status.get("lastAttempt", ""), FAILED_RETRY_SECONDS):
        return False
    items = data.get(meta["items_key"], [])
    last_sync = data.get(meta["last_sync_key"], "")
    if not items or not last_sync:
        return True
    try:
        last_ts = time.mktime(time.strptime(last_sync, _TIME_FORMAT))
    except ValueError:
        return True
    return (time.time() - last_ts) >= int(meta["interval"])


def _section_name_for_meta(target: dict[str, Any]) -> str:
    for name, meta in _SECTIONS.items():
        if meta is target:
            return name
    return ""


def _recent(value: str, seconds: int) -> bool:
    try:
        ts = time.mktime(time.strptime(value, _TIME_FORMAT))
    except (TypeError, ValueError):
        return False
    return (time.time() - ts) < seconds


def _run_section(section: str) -> dict[str, Any]:
    meta = _SECTIONS[section]
    label = meta["label"]
    now = _now()
    _mark_running(section, now)
    try:
        result = meta["runner"]()
    except Exception as exc:
        _mark_finished(section, now, error=str(exc))
        raise
    _mark_finished(
        section,
        now,
        success=result.get("lastSync") or _now(),
        used_stale=bool(result.get("usedStaleCookies")),
        warning=result.get("warning") or "",
    )
    logger.info("Auto-sync terminado: %s", label)
    return result


def _mark_running(section: str, now: str) -> None:
    def mutate(data: dict[str, Any]) -> None:
        status = _ensure_status(data)
        status.update({
            "running": True,
            "section": section,
            "lastAttempt": now,
            "lastError": "",
        })
        sections = status.setdefault("sections", {})
        sections.setdefault(section, {})
        sections[section].update({"running": True, "lastAttempt": now, "lastError": ""})

    update_state(mutate)


def _mark_finished(
    section: str,
    attempt: str,
    *,
    success: str | None = None,
    error: str = "",
    used_stale: bool = False,
    warning: str = "",
) -> None:
    def mutate(data: dict[str, Any]) -> None:
        status = _ensure_status(data)
        status.update({
            "running": False,
            "section": "",
            "lastAttempt": attempt,
            "lastError": error,
            "usedStaleCookies": used_stale,
            "warning": warning,
        })
        if success:
            status["lastSuccess"] = success
        sections = status.setdefault("sections", {})
        sections.setdefault(section, {})
        sections[section].update({
            "running": False,
            "lastAttempt": attempt,
            "lastError": error,
            "usedStaleCookies": used_stale,
            "warning": warning,
        })
        if success:
            sections[section]["lastSuccess"] = success

    update_state(mutate)


def _ensure_status(data: dict[str, Any]) -> dict[str, Any]:
    status = data.setdefault("syncStatus", {})
    status.setdefault("running", False)
    status.setdefault("section", "")
    status.setdefault("lastAttempt", "")
    status.setdefault("lastSuccess", "")
    status.setdefault("lastError", "")
    status.setdefault("usedStaleCookies", False)
    status.setdefault("warning", "")
    status.setdefault("sections", {})
    return status


def _now() -> str:
    return time.strftime(_TIME_FORMAT)
