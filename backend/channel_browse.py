from __future__ import annotations

import logging
import re
import time
from typing import Any

import yt_dlp

from .state import read_state
from .youtube import _entry_to_item
from .ytdlp_opts import javascript_runtime_opts

logger = logging.getLogger(__name__)

TABS = ("videos", "shorts", "streams", "playlists", "releases")

_BROWSE_CACHE: dict[str, tuple[float, dict[str, Any]]] = {}
_CACHE_TTL = 600
_CACHE_MAX = 80

_TAB_SUFFIX_RE = re.compile(r"/(videos|shorts|streams|playlists|releases|featured|community|about)/?$", re.I)


def resolve_channel_base(url: str) -> str:
    """Normalize any channel reference (@handle, UC id, /user/, full URL) to a
    base channel URL without tab suffix."""
    clean = (url or "").strip().rstrip("/")
    if not clean:
        raise ValueError("URL de canal vacia")
    if not clean.startswith("http"):
        if clean.startswith("UC") and len(clean) == 24:
            clean = f"https://www.youtube.com/channel/{clean}"
        else:
            clean = "https://www.youtube.com/@" + clean.lstrip("@")
    clean = _TAB_SUFFIX_RE.sub("", clean).rstrip("/")
    return clean


def browse_channel(url: str, tab: str = "videos", page: int = 1, page_size: int = 30) -> dict[str, Any]:
    tab = (tab or "videos").lower()
    if tab not in TABS:
        raise ValueError(f"Tab no soportada: {tab}")
    page = max(1, int(page))
    page_size = min(60, max(5, int(page_size)))
    base = resolve_channel_base(url)

    cache_key = f"{base}|{tab}|{page}|{page_size}"
    cached = _cache_get(cache_key)
    if cached is not None:
        return cached

    start = (page - 1) * page_size + 1
    end = page * page_size + 1  # one extra to derive hasMore
    opts = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "lazy_playlist": True,
        "ignoreerrors": True,
        "playlist_items": f"{start}:{end}",
        **javascript_runtime_opts(),
    }

    tab_error = ""
    info: dict[str, Any] = {}
    try:
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info(f"{base}/{tab}", download=False) or {}
    except yt_dlp.utils.DownloadError as exc:
        # Canal sin esa pestana (p.ej. sin shorts) — respuesta vacia, no 500
        logger.info("Tab %s no disponible para %s: %s", tab, base, exc)
        tab_error = "Este canal no tiene contenido en esta pestaña."

    raw_entries = [e for e in (info.get("entries") or []) if e]
    has_more = len(raw_entries) > page_size
    entries = raw_entries[:page_size]

    if tab == "playlists":
        items = [_playlist_entry_to_item(e) for e in entries]
    else:
        items = []
        for entry in entries:
            item = _entry_to_item(entry)
            if item.get("url"):
                if tab == "shorts":
                    item["resultType"] = "short"
                    if item.get("id") and "/shorts/" not in str(item["url"]).lower():
                        item["url"] = f"https://www.youtube.com/shorts/{item['id']}"
                    if not item.get("durationText") or item.get("durationText") == "--:--":
                        item["durationText"] = "Short"
                items.append(item)

    channel_id = info.get("channel_id") or info.get("id") or ""
    name = info.get("channel") or info.get("uploader") or _strip_tab_title(info.get("title") or "")
    handle = info.get("uploader_id") or ""
    avatar = _channel_avatar(info)
    canonical = f"https://www.youtube.com/channel/{channel_id}" if str(channel_id).startswith("UC") else base

    followed = _find_followed(base, canonical, handle)
    result = {
        "name": name or "",
        "url": canonical,
        "channelId": channel_id,
        "handle": handle if str(handle).startswith("@") else "",
        "avatar": avatar,
        "subscriberCount": info.get("channel_follower_count"),
        "tab": tab,
        "page": page,
        "pageSize": page_size,
        "hasMore": has_more,
        "items": items,
        "tabError": tab_error,
        "alreadyFollowed": bool(followed),
        "followedChannelId": followed["id"] if followed else None,
    }
    _cache_set(cache_key, result)
    if canonical != base:
        # mismo resultado accesible por la URL canonica (paginas 2+ usan esa key)
        _cache_set(f"{canonical}|{tab}|{page}|{page_size}", result)
    return result


def _playlist_entry_to_item(entry: dict[str, Any]) -> dict[str, Any]:
    item = _entry_to_item(entry)
    item["resultType"] = "playlist"
    count = entry.get("playlist_count") or entry.get("video_count")
    if count:
        item["durationText"] = f"{count} videos"
    return item


def _strip_tab_title(title: str) -> str:
    return re.sub(r"\s+-\s+(Videos|Shorts|Live|Playlists|Releases)$", "", title, flags=re.I)


def _channel_avatar(info: dict[str, Any]) -> str:
    best = ""
    for thumb in info.get("thumbnails") or []:
        url = thumb.get("url") or ""
        if not url:
            continue
        if thumb.get("id") == "avatar_uncropped" or "yt3." in url:
            best = url
        elif not best:
            best = url
    return best


def _find_followed(*candidates: str) -> dict[str, Any] | None:
    channels = read_state()["channels"]
    bases = set()
    for cand in candidates:
        if not cand:
            continue
        bases.add(cand.rstrip("/").lower())
        if cand.startswith("@"):
            bases.add(f"https://www.youtube.com/{cand}".lower())
    for channel in channels:
        stored = re.sub(r"/(videos|streams|shorts)/?$", "", channel.get("url") or "", flags=re.I).rstrip("/").lower()
        if stored in bases:
            return channel
    return None


def _cache_get(key: str) -> dict[str, Any] | None:
    cached = _BROWSE_CACHE.get(key)
    if not cached:
        return None
    ts, value = cached
    if time.monotonic() - ts > _CACHE_TTL:
        _BROWSE_CACHE.pop(key, None)
        return None
    return value


def _cache_set(key: str, value: dict[str, Any]) -> None:
    if len(_BROWSE_CACHE) >= _CACHE_MAX:
        oldest = min(_BROWSE_CACHE.items(), key=lambda kv: kv[1][0])[0]
        _BROWSE_CACHE.pop(oldest, None)
    _BROWSE_CACHE[key] = (time.monotonic(), value)
