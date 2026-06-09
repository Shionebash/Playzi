from __future__ import annotations

import re
import time
from typing import Any

import yt_dlp

from .config import YTDLP_SEARCH_LIMIT
from .ytdlp_opts import javascript_runtime_opts


YOUTUBE_RE = re.compile(r"(youtube\.com|youtu\.be|music\.youtube\.com)", re.I)
VIDEO_ID_RE = re.compile(r"(?:v=|youtu\.be/|shorts/)([A-Za-z0-9_-]{11})")
_SEARCH_CACHE: dict[str, tuple[float, list[dict[str, Any]]]] = {}
_METADATA_CACHE: dict[str, tuple[float, dict[str, Any]]] = {}
_CACHE_TTL = 300
_CACHE_MAX = 50


def is_youtube_url(value: str) -> bool:
    return bool(YOUTUBE_RE.search(value or ""))


def _entry_to_item(entry: dict[str, Any]) -> dict[str, Any]:
    video_id = entry.get("id") or entry.get("url") or entry.get("webpage_url")
    webpage_url = entry.get("webpage_url") or entry.get("url")
    if webpage_url and not str(webpage_url).startswith("http"):
        webpage_url = f"https://www.youtube.com/watch?v={webpage_url}"
    duration = entry.get("duration")
    channel_url = _channel_url_for(entry)
    channel = _channel_name_for(entry)
    return {
        "id": str(video_id or ""),
        "url": webpage_url,
        "title": entry.get("title") or "Sin titulo",
        "channel": channel,
        "channelUrl": channel_url,
        "duration": duration,
        "durationText": _format_duration(duration),
        "thumbnail": _thumbnail_for(entry, str(video_id or ""), webpage_url),
        "viewCount": entry.get("view_count"),
        "live": bool(entry.get("is_live")),
    }


def _channel_name_for(entry: dict[str, Any]) -> str:
    return (
        entry.get("uploader")
        or entry.get("channel")
        or entry.get("channel_name")
        or entry.get("author")
        or entry.get("creator")
        or _owner_text(entry)
        or "YouTube"
    )


def _owner_text(entry: dict[str, Any]) -> str:
    owner = entry.get("owner") or {}
    if isinstance(owner, dict):
        return owner.get("name") or owner.get("title") or ""
    return ""


def _channel_url_for(entry: dict[str, Any]) -> str | None:
    if entry.get("channel_url"):
        return entry["channel_url"]
    if entry.get("uploader_url"):
        return entry["uploader_url"]
    if entry.get("author_url"):
        return entry["author_url"]
    channel_id = entry.get("channel_id") or ""
    uploader_id = entry.get("uploader_id") or ""
    if channel_id:
        return f"https://www.youtube.com/channel/{channel_id}"
    if uploader_id.startswith("@"):
        return f"https://www.youtube.com/{uploader_id}"
    if uploader_id:
        return f"https://www.youtube.com/user/{uploader_id}"
    return None


def _thumbnail_for(entry: dict[str, Any], video_id: str, webpage_url: str | None) -> str | None:
    thumbnail = entry.get("thumbnail")
    if thumbnail:
        return thumbnail
    clean_id = _youtube_id(video_id) or _youtube_id(webpage_url or "")
    if clean_id:
        return f"https://i.ytimg.com/vi/{clean_id}/hqdefault.jpg"
    thumbnails = entry.get("thumbnails") or []
    if thumbnails:
        return thumbnails[-1].get("url")
    return None


def _youtube_id(value: str) -> str | None:
    if not value:
        return None
    if re.fullmatch(r"[A-Za-z0-9_-]{11}", value):
        return value
    match = VIDEO_ID_RE.search(value)
    return match.group(1) if match else None


def _format_duration(seconds: Any) -> str:
    if not isinstance(seconds, (int, float)):
        return "--:--"
    seconds = int(seconds)
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    if h:
        return f"{h}:{m:02d}:{s:02d}"
    return f"{m}:{s:02d}"


def search(query: str, limit: int | None = None) -> list[dict[str, Any]]:
    query = query.strip()
    if not query:
        return []
    effective_limit = limit or YTDLP_SEARCH_LIMIT
    cache_key = f"{query}|{effective_limit}"
    cached = _cache_get(_SEARCH_CACHE, cache_key)
    if cached is not None:
        return cached
    target = query if is_youtube_url(query) else f"ytsearch{effective_limit}:{query}"
    opts = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "noplaylist": False,
        "ignoreerrors": True,
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(target, download=False)
    if not info:
        return []
    entries = info.get("entries")
    if entries:
        items = [_entry_to_item(e) for e in entries if e]
    else:
        items = [_entry_to_item(info)]
    _cache_set(_SEARCH_CACHE, cache_key, items)
    return items


def metadata_for_url(url: str) -> dict[str, Any]:
    cached = _cache_get(_METADATA_CACHE, url)
    if cached is not None:
        return cached
    opts = {
        "quiet": True,
        "skip_download": True,
        "noplaylist": True,
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(url, download=False)
    item = _entry_to_item(info)
    _cache_set(_METADATA_CACHE, url, item)
    return item


def fetch_radio_items(url: str) -> list[dict[str, Any]]:
    video_id = _youtube_id(url)
    if not video_id:
        return []
    mix_url = f"https://www.youtube.com/watch?v={video_id}&list=RD{video_id}"
    opts = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "playlistend": 26,
        "ignoreerrors": True,
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(mix_url, download=False) or {}
    entries = info.get("entries") or []
    # Skip first entry (it's the current video)
    return [_entry_to_item(e) for e in entries[1:] if e]


def _cache_get(cache: dict[str, tuple[float, Any]], key: str) -> Any | None:
    cached = cache.get(key)
    if not cached:
        return None
    ts, value = cached
    if time.monotonic() - ts > _CACHE_TTL:
        cache.pop(key, None)
        return None
    return value


def _cache_set(cache: dict[str, tuple[float, Any]], key: str, value: Any) -> None:
    if len(cache) >= _CACHE_MAX:
        cache.pop(next(iter(cache)))
    cache[key] = (time.monotonic(), value)
