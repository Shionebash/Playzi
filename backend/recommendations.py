from __future__ import annotations

import time
from typing import Any

import yt_dlp

from .state import read_state, update_state
from .youtube import _entry_to_item
from ._cookies import fallback_cookie_opts, get_cookie_opts, handle_cookie_error, pop_cookie_warning, secure_cache_after_write
from .ytdlp_opts import javascript_runtime_opts


def fetch_recommendations() -> dict[str, Any]:
    opts: dict[str, Any] = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "playlistend": 40,
        "ignoreerrors": True,
        **javascript_runtime_opts(),
    }
    opts.update(get_cookie_opts())

    used_stale_cookies = False
    try:
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info("https://www.youtube.com", download=False) or {}
    except Exception as exc:
        fallback = fallback_cookie_opts(exc)
        if not fallback:
            handle_cookie_error(exc)
            raise
        used_stale_cookies = bool(fallback.pop("_used_stale_cookies", False))
        retry_opts = {**opts, **fallback}
        retry_opts.pop("cookiesfrombrowser", None)
        try:
            with yt_dlp.YoutubeDL(retry_opts) as ydl:
                info = ydl.extract_info("https://www.youtube.com", download=False) or {}
        except Exception as retry_exc:
            handle_cookie_error(retry_exc)
            raise
    secure_cache_after_write()

    raw_entries = info.get("entries") or []
    flat = _flatten_entries(raw_entries)

    items = []
    seen: set[str] = set()
    for entry in flat:
        if not entry:
            continue
        item = _entry_to_item(entry)
        if not item.get("url"):
            continue
        uid = item["url"]
        if uid in seen:
            continue
        seen.add(uid)
        items.append(item)
    _enrich_missing_channels(items, opts, limit=20)

    now = time.strftime("%Y-%m-%d %H:%M:%S")
    warning = pop_cookie_warning()
    if not items:
        existing = list_recommendations()
        existing_items = existing.get("items", [])
        warning = warning or (
            "YouTube no devolvio recomendaciones personalizadas. "
            "La sesion de cookies puede estar cerrada o YouTube puede estar limitando el feed recomendado."
        )
        return {
            "items": existing_items,
            "lastSync": existing.get("lastSync", ""),
            "usedStaleCookies": used_stale_cookies,
            "warning": warning,
        }

    def mutate(data):
        data["recommendations"] = items
        data["recommendationsLastSync"] = now

    update_state(mutate)
    return {
        "items": items,
        "lastSync": now,
        "usedStaleCookies": used_stale_cookies,
        "warning": warning,
    }


def list_recommendations() -> dict[str, Any]:
    data = read_state()
    return {
        "items": data.get("recommendations", []),
        "lastSync": data.get("recommendationsLastSync", ""),
    }


def _flatten_entries(entries: list) -> list[dict]:
    """YouTube homepage returns nested sections. Flatten one level deep."""
    result = []
    for e in entries:
        if not e:
            continue
        sub = e.get("entries")
        if sub:
            result.extend(item for item in sub if item)
        else:
            result.append(e)
    return result


def _enrich_missing_channels(items: list[dict[str, Any]], base_opts: dict[str, Any], limit: int = 20) -> None:
    targets = [item for item in items if item.get("url") and (not item.get("channelUrl") or item.get("channel") == "YouTube")]
    if not targets:
        return
    opts = {
        **base_opts,
        "extract_flat": False,
        "noplaylist": True,
        "playlistend": None,
    }
    enriched = 0
    with yt_dlp.YoutubeDL(opts) as ydl:
        for item in targets:
            if enriched >= limit:
                break
            try:
                info = ydl.extract_info(item["url"], download=False) or {}
            except Exception:
                continue
            detailed = _entry_to_item(info)
            for key in ("channel", "channelUrl", "thumbnail", "duration", "durationText", "viewCount"):
                if detailed.get(key):
                    item[key] = detailed[key]
            enriched += 1
