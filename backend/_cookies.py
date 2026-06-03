"""Shared private cookie cache resolution for yt-dlp."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from . import config
from .managed_browser import export_cookies

CACHE_MAX_HOURS = 12
_LAST_COOKIE_WARNING = ""


def _cache_path() -> Path:
    return config.DATA_DIR / "private" / "cookies_cache.txt"


def _ensure_private_dir() -> None:
    _cache_path().parent.mkdir(parents=True, exist_ok=True)


def _cache_is_fresh() -> bool:
    p = _cache_path()
    if not p.exists():
        return False
    if not _looks_like_netscape_cookie_file(p):
        try:
            p.unlink()
        except OSError:
            pass
        return False
    age_hours = (time.time() - p.stat().st_mtime) / 3600
    return age_hours < CACHE_MAX_HOURS


def _secure_permissions(path: Path) -> None:
    """Restrict cookie cache to the current user only."""
    if not path.exists():
        return
    if sys.platform == "win32":
        username = os.environ.get("USERNAME", "")
        if not username:
            return
        domain = os.environ.get("USERDOMAIN", "")
        user = f"{domain}\\{username}" if domain else username
        try:
            subprocess.run(
                [
                    "icacls",
                    str(path),
                    "/inheritance:r",
                    "/grant:r",
                    f"{user}:F",
                    "/remove:g",
                    "Users",
                    "Everyone",
                    "Authenticated Users",
                ],
                check=False,
                capture_output=True,
            )
        except OSError:
            pass
    else:
        try:
            path.chmod(0o600)
        except OSError:
            pass


def _cache_from_file(source: Path) -> Path:
    _ensure_private_dir()
    cache = _cache_path()
    source = source.resolve()
    should_copy = not cache.exists()
    if cache.exists():
        try:
            should_copy = source.stat().st_mtime > cache.stat().st_mtime
        except OSError:
            should_copy = True
    if should_copy:
        shutil.copy2(source, cache)
    _secure_permissions(cache)
    return cache


def _cookies_dir() -> Path:
    return config.ROOT / "cookies"


def _find_exported_cookie_file() -> Path | None:
    folder = _cookies_dir()
    if not folder.exists():
        return None
    candidates = [p for p in folder.glob("*.txt") if p.is_file()]
    candidates.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    for candidate in candidates:
        if _looks_like_netscape_cookie_file(candidate):
            return candidate
    return None


def _looks_like_netscape_cookie_file(source: Path) -> bool:
    try:
        with source.open("r", encoding="utf-8-sig", errors="ignore") as handle:
            for _ in range(20):
                line = handle.readline()
                if not line:
                    break
                stripped = line.strip()
                if not stripped:
                    continue
                if stripped.startswith("# Netscape HTTP Cookie File"):
                    return True
                if stripped.startswith("#"):
                    continue
                return stripped.count("\t") >= 6
    except OSError:
        return False
    return False


def get_cookie_opts(force: bool = False) -> dict[str, Any]:
    """Return yt-dlp cookie opts using Playzi's private cookie cache.

    When force=True with the managed 'playzi' profile, the fresh cache is
    bypassed and cookies are re-exported from the persistent browser profile
    (recovers from a stale cache when the profile still holds a live session).
    """
    if not config.COOKIES_BROWSER and not config.COOKIES_FILE:
        raise ValueError(
            "No hay cookies configuradas. "
            "Ve a Configuracion y selecciona tu navegador con sesion de YouTube iniciada."
        )

    if config.COOKIES_FILE:
        source = Path(config.COOKIES_FILE)
        if not source.exists():
            if not config.COOKIES_BROWSER:
                raise ValueError(
                    f"Archivo de cookies no encontrado: {config.COOKIES_FILE}\n"
                    "Verifica que la ruta sea correcta."
                )
        elif _looks_like_netscape_cookie_file(source):
            cache = _cache_from_file(source)
            return {"cookiefile": str(cache)}
        elif not config.COOKIES_BROWSER:
            raise ValueError(
                "El archivo configurado no parece un cookies.txt compatible con yt-dlp.\n"
                "Selecciona un navegador en Configuracion o usa un cookies.txt exportado."
            )

    is_playzi = config.COOKIES_BROWSER.lower() == "playzi"

    # On force with the managed profile, re-export from the browser profile
    # directly (skip stale cache and static exported files).
    if force and is_playzi:
        invalidate_cache()
        source = export_cookies(headless=True)
        cache = _cache_from_file(source)
        return {"cookiefile": str(cache)}

    exported = _find_exported_cookie_file()
    if exported:
        cache = _cache_from_file(exported)
        return {"cookiefile": str(cache)}

    if is_playzi:
        cache = _cache_path()
        if _cache_is_fresh():
            _secure_permissions(cache)
            return {"cookiefile": str(cache)}
        source = export_cookies(headless=True)
        cache = _cache_from_file(source)
        return {"cookiefile": str(cache)}

    _ensure_private_dir()
    cache = _cache_path()
    if _cache_is_fresh():
        _secure_permissions(cache)
        return {"cookiefile": str(cache)}

    return {
        "cookiesfrombrowser": (config.COOKIES_BROWSER,),
        "cookiefile": str(cache),
    }


def _is_browser_locked_error(msg: str) -> bool:
    msg_lower = msg.lower()
    return "could not copy" in msg_lower or "database is locked" in msg_lower


def fallback_cookie_opts(exc: Exception) -> dict[str, Any] | None:
    """Use a stale private cache when browser extraction is blocked."""
    global _LAST_COOKIE_WARNING
    cache = _cache_path()
    exported = _find_exported_cookie_file()
    if exported:
        cached = _cache_from_file(exported)
        _LAST_COOKIE_WARNING = (
            f"No se pudo leer el navegador; se uso {exported.name} desde la carpeta cookies."
        )
        return {"cookiefile": str(cached), "_used_stale_cookies": True}
    if (
        config.COOKIES_BROWSER
        and cache.exists()
        and _looks_like_netscape_cookie_file(cache)
        and _is_browser_locked_error(str(exc))
    ):
        _secure_permissions(cache)
        _LAST_COOKIE_WARNING = (
            "El navegador bloqueo las cookies; se uso el cache privado anterior."
        )
        return {"cookiefile": str(cache), "_used_stale_cookies": True}
    return None


def pop_cookie_warning() -> str:
    global _LAST_COOKIE_WARNING
    warning = _LAST_COOKIE_WARNING
    _LAST_COOKIE_WARNING = ""
    return warning


def handle_cookie_error(exc: Exception) -> None:
    """Re-raise cookie failures as user-friendly ValueErrors."""
    msg = str(exc)
    if "failed to decrypt with dpapi" in msg.lower():
        raise ValueError(
            "Chrome/Edge no permitio descifrar las cookies con DPAPI.\n"
            "Alternativa estable: exporta cookies de YouTube en formato Netscape cookies.txt "
            "y configura COOKIES_FILE con esa ruta, o usa Firefox como navegador de cookies."
        ) from exc
    if _is_browser_locked_error(msg):
        raise ValueError(
            "No se pudo leer el navegador: el archivo de cookies esta bloqueado.\n"
            "Cierra Chrome/Edge completamente y reintenta. Si Playzi ya tenia "
            "un cache privado anterior, lo usara automaticamente."
        ) from exc
    if "CookieLoadError" in msg or "failed to load cookies" in msg:
        raise ValueError(
            "No se pudieron cargar las cookies.\n"
            "Si usas Chrome/Edge puede ser un bloqueo DPAPI. Alternativa estable: usa un "
            "cookies.txt exportado en formato Netscape o configura Firefox como fuente de cookies."
        ) from exc
    if "permission" in msg.lower() or "access" in msg.lower() or "errno 13" in msg.lower():
        raise ValueError(
            "Sin permisos para leer el cache privado de cookies.\n"
            "Cierra Playzi, elimina data/private/cookies_cache.txt y vuelve a abrir la app."
        ) from exc


def secure_cache_after_write() -> None:
    p = _cache_path()
    if p.exists():
        _secure_permissions(p)


def invalidate_cache() -> None:
    p = _cache_path()
    if p.exists():
        p.unlink()
