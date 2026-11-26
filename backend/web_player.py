from __future__ import annotations

import html
import json
import logging
import re
import struct
import threading
import time
from typing import Any
from urllib.parse import parse_qs, quote, urlparse

import httpx
import yt_dlp

from . import config
from ._cookies import get_cookie_opts
from .player import playlist_entries
from .ytmusic import music_playlist_queue
from .youtube import _entry_to_item
from .ytdlp_opts import javascript_runtime_opts

logger = logging.getLogger(__name__)


_LOUDNESS_CACHE: dict[str, float | None] = {}
_LOUDNESS_LOCK = threading.Lock()
_VIDEO_ID_RE = re.compile(r"(?:[?&]v=|youtu\.be/|/shorts/|/embed/)([A-Za-z0-9_-]{11})")
_LOUDNESS_DB_RE = re.compile(r'"loudnessDb"\s*:\s*(-?\d+(?:\.\d+)?)')
_PERCEPTUAL_LOUDNESS_RE = re.compile(r'"perceptualLoudnessDb"\s*:\s*(-?\d+(?:\.\d+)?)')


def _extract_video_id(url: str, info: dict[str, Any] | None = None) -> str | None:
    if info and isinstance(info.get("id"), str) and len(info["id"]) == 11:
        return info["id"]
    m = _VIDEO_ID_RE.search(url or "")
    return m.group(1) if m else None


def _fetch_loudness_db(url: str, info: dict[str, Any] | None = None) -> float | None:
    if info:
        raw_val = info.get("loudnessDb")
        if raw_val is None:
            raw_val = info.get("loudness_db")
        if isinstance(raw_val, (int, float)) and -60.0 <= float(raw_val) <= 30.0:
            return float(raw_val)
    vid = _extract_video_id(url, info)
    if not vid:
        return None
    with _LOUDNESS_LOCK:
        if vid in _LOUDNESS_CACHE:
            return _LOUDNESS_CACHE[vid]
    val: float | None = None
    try:
        resp = httpx.get(
            f"https://www.youtube.com/watch?v={vid}",
            headers={"User-Agent": "Mozilla/5.0"},
            timeout=4.0,
            follow_redirects=True,
        )
        if resp.status_code == 200:
            m = _LOUDNESS_DB_RE.search(resp.text)
            if m:
                parsed = float(m.group(1))
                if -60.0 <= parsed <= 30.0:
                    val = parsed
            else:
                pm = _PERCEPTUAL_LOUDNESS_RE.search(resp.text)
                if pm:
                    parsed = float(pm.group(1)) + 14.0
                    if -60.0 <= parsed <= 30.0:
                        val = parsed
    except Exception:
        val = None
    with _LOUDNESS_LOCK:
        if len(_LOUDNESS_CACHE) >= 512:
            _LOUDNESS_CACHE.clear()
        _LOUDNESS_CACHE[vid] = val
    return val


def player_info(url: str, quality: str | None = None) -> dict[str, Any]:
    info = _extract_info(url)
    video, audio = _select_formats(info, quality)
    item = _entry_to_item(info)
    return {
        "item": item,
        "duration": info.get("duration") or 0,
        "quality": quality or config.PLAYBACK_QUALITY or "best",
        "video": _public_format(video),
        "audio": _public_format(audio),
        "availableQualities": _available_qualities(info),
        "loudnessDb": _fetch_loudness_db(url, info),
        "manifestUrl": f"/api/player/manifest.mpd?url={quote(url, safe='')}&quality={quote(quality or config.PLAYBACK_QUALITY or 'best', safe='')}",
    }


def manifest(url: str, quality: str | None = None, audio_lang: str | None = None) -> str:
    cache_key = f"{url}|{quality or config.PLAYBACK_QUALITY or 'best'}|{audio_lang or ''}"
    cached = _cache_get(_MANIFEST_CACHE, cache_key)
    if cached is not None:
        return cached
    # Con audio_lang, video y audio salen del info multi (mismo cliente, URLs coherentes)
    info = _extract_info_multi(url) if audio_lang else _extract_info(url)
    video, audio = _select_formats(info, quality, audio_lang)
    duration = float(info.get("duration") or video.get("duration") or audio.get("duration") or 0)
    if duration <= 0:
        duration = 36000
    video_mime = _mime(video)
    audio_mime = _mime(audio)
    video_codec = html.escape(video.get("vcodec") or "avc1.640028")
    audio_codec = html.escape(audio.get("acodec") or "mp4a.40.2")
    video_url = html.escape(_proxy_url(video))
    audio_url = html.escape(_proxy_url(audio))
    video_ranges = _segment_ranges(video)
    audio_ranges = _segment_ranges(audio)
    if not video_ranges or not audio_ranges:
        fmt_id = video.get("format_id") or audio.get("format_id") or "?"
        raise RuntimeError(
            f"No se encontraron rangos DASH (init/index) para el formato {fmt_id}. "
            "El video puede estar en un formato no compatible con el reproductor web."
        )
    video_init, video_index = video_ranges
    audio_init, audio_index = audio_ranges
    width = int(video.get("width") or 1280)
    height = int(video.get("height") or 720)
    bandwidth_v = int((video.get("tbr") or 2000) * 1000)
    bandwidth_a = int((audio.get("tbr") or 128) * 1000)
    body = f"""<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT{duration:.3f}S" minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period duration="PT{duration:.3f}S">
    <AdaptationSet id="video" contentType="video" mimeType="{video_mime}" codecs="{video_codec}" width="{width}" height="{height}" frameRate="{int(video.get('fps') or 30)}" startWithSAP="1" subsegmentAlignment="true">
      <Representation id="{html.escape(str(video.get('format_id') or 'video'))}" bandwidth="{bandwidth_v}" width="{width}" height="{height}">
        <BaseURL>{video_url}</BaseURL>
        <SegmentBase indexRange="{html.escape(video_index)}" indexRangeExact="true">
          <Initialization range="{html.escape(video_init)}" />
        </SegmentBase>
      </Representation>
    </AdaptationSet>
    <AdaptationSet id="audio" contentType="audio" mimeType="{audio_mime}" codecs="{audio_codec}" startWithSAP="1" subsegmentAlignment="true">
      <Representation id="{html.escape(str(audio.get('format_id') or 'audio'))}" bandwidth="{bandwidth_a}" audioSamplingRate="{int(audio.get('asr') or 44100)}">
        <AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="{int(audio.get('audio_channels') or 2)}" />
        <BaseURL>{audio_url}</BaseURL>
        <SegmentBase indexRange="{html.escape(audio_index)}" indexRangeExact="true">
          <Initialization range="{html.escape(audio_init)}" />
        </SegmentBase>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"""
    _cache_set(_MANIFEST_CACHE, cache_key, body)
    return body


def resolve_playlist(url: str, quality: str | None = None) -> dict[str, Any]:
    parsed = urlparse(url)
    query = parse_qs(parsed.query)
    playlist_id = (query.get("list") or [""])[0]
    # Mixes/radios (RD*) are "unviewable" for yt-dlp's playlist extractor on any
    # host; the music watch-playlist API resolves them, so route them there too.
    if playlist_id and (parsed.hostname == "music.youtube.com" or playlist_id.startswith("RD")):
        result = music_playlist_queue(playlist_id, limit=100)
        if result.get("items") or parsed.hostname == "music.youtube.com":
            return _with_manifest_urls(result, quality)

    entries = playlist_entries(url, limit=100)
    error = "" if entries else "No se pudieron leer los videos de esta playlist."
    return _with_manifest_urls({"items": entries, "title": "Playlist", "source": "youtube", "requiresLogin": False, "error": error}, quality)


_INFO_CACHE: dict[str, tuple[float, dict]] = {}  # url -> (timestamp, info)
_MANIFEST_CACHE: dict[str, tuple[float, str]] = {}
_SEGMENT_RANGE_CACHE: dict[str, tuple[float, tuple[str, str]]] = {}
_CACHE_TTL = 18000  # 5 hours — googlevideo URLs include `expire` (~6h)
_CACHE_MAX = 150

_INFO_CACHE_FILE = config.DATA_DIR / "player_cache.json"
_INFO_CACHE_LOCK = threading.RLock()
_CACHE_LOCK = threading.RLock()
_INFO_CACHE_LOADED = False
# Single-flight: concurrent extractions for the same URL share one yt-dlp call
# instead of each paying the ~3.5s nsig cost (e.g. /info + /manifest fired together).
_INFLIGHT: dict[str, threading.Event] = {}
_INFLIGHT_LOCK = threading.Lock()
# Drop cached stream URLs this many seconds before their real `expire` to avoid
# serving a URL that dies mid-playback (15 min covers long videos started near expiry).
_EXPIRE_MARGIN = 900


def _min_expire(info: dict[str, Any]) -> int | None:
    """Earliest `expire` epoch across the info's format URLs, or None."""
    expires: list[int] = []
    for fmt in info.get("formats") or []:
        url = fmt.get("url") or ""
        match = re.search(r"[?&/]expire[=/](\d+)", url)
        if match:
            expires.append(int(match.group(1)))
    return min(expires) if expires else None


def _info_expired(info: dict[str, Any]) -> bool:
    expire = _min_expire(info)
    return expire is not None and time.time() > (expire - _EXPIRE_MARGIN)


def _load_info_cache() -> None:
    global _INFO_CACHE_LOADED
    with _INFO_CACHE_LOCK:
        if _INFO_CACHE_LOADED:
            return
        _INFO_CACHE_LOADED = True
        try:
            raw = json.loads(_INFO_CACHE_FILE.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return
        now = time.monotonic()
        for url, info in (raw or {}).items():
            if not isinstance(info, dict) or _info_expired(info):
                continue
            _INFO_CACHE[url] = (now, info)


def _save_info_cache() -> None:
    with _INFO_CACHE_LOCK:
        with _CACHE_LOCK:
            snapshot = list(_INFO_CACHE.items())
        payload = {}
        for url, (_, info) in snapshot:
            try:
                payload[url] = yt_dlp.YoutubeDL.sanitize_info(info)
            except Exception:
                continue
        try:
            config.DATA_DIR.mkdir(parents=True, exist_ok=True)
            tmp = _INFO_CACHE_FILE.with_suffix(".json.tmp")
            tmp.write_text(json.dumps(payload), encoding="utf-8")
            tmp.replace(_INFO_CACHE_FILE)
        except OSError:
            pass


def _fresh_cached(url: str) -> dict[str, Any] | None:
    cached = _cache_get(_INFO_CACHE, url)
    if cached is not None and not _info_expired(cached):
        return cached
    if cached is not None:
        with _CACHE_LOCK:
            _INFO_CACHE.pop(url, None)
    return None


def _extract_info(url: str) -> dict[str, Any]:
    _load_info_cache()
    cached = _fresh_cached(url)
    if cached is not None:
        return cached

    # Single-flight: first caller extracts, others wait and reuse its result.
    with _INFLIGHT_LOCK:
        event = _INFLIGHT.get(url)
        is_owner = event is None
        if is_owner:
            event = threading.Event()
            _INFLIGHT[url] = event

    if not is_owner:
        event.wait(timeout=60)
        cached = _fresh_cached(url)
        if cached is not None:
            return cached
        # Owner failed or timed out — fall through and extract ourselves.

    try:
        opts = {
            "quiet": True,
            "skip_download": True,
            "noplaylist": True,
            **javascript_runtime_opts(),
        }
        if config.COOKIES_BROWSER or config.COOKIES_FILE:
            try:
                cookie_opts = {
                    k: v for k, v in get_cookie_opts().items() if not k.startswith("_")
                }
                opts.update(cookie_opts)
            except Exception:
                pass
        with yt_dlp.YoutubeDL(opts) as ydl:
            result = ydl.extract_info(url, download=False) or {}
        _cache_set(_INFO_CACHE, url, result)
        _save_info_cache()
        return result
    finally:
        if is_owner:
            with _INFLIGHT_LOCK:
                _INFLIGHT.pop(url, None)
            event.set()


# Multi-audio (dubs): el cliente por defecto de yt-dlp 2026 no devuelve pistas
# dobladas; web_embedded si (con runtime JS). Extraccion separada, solo bajo
# demanda, para no tocar la latencia del arranque normal.
_MULTI_INFO_CACHE: dict[str, tuple[float, dict]] = {}


def _multi_opts() -> dict[str, Any]:
    base = javascript_runtime_opts()
    extractor_args = {k: dict(v) for k, v in (base.get("extractor_args") or {}).items()}
    youtube_args = extractor_args.setdefault("youtube", {})
    youtube_args["player_client"] = ["web_embedded"]
    return {
        "quiet": True,
        "skip_download": True,
        "noplaylist": True,
        **base,
        "extractor_args": extractor_args,
    }


def _extract_info_multi(url: str) -> dict[str, Any]:
    cached = _cache_get(_MULTI_INFO_CACHE, url)
    if cached is not None and not _info_expired(cached):
        return cached
    if cached is not None:
        _MULTI_INFO_CACHE.pop(url, None)

    flight_key = f"multi|{url}"
    with _INFLIGHT_LOCK:
        event = _INFLIGHT.get(flight_key)
        is_owner = event is None
        if is_owner:
            event = threading.Event()
            _INFLIGHT[flight_key] = event

    if not is_owner:
        event.wait(timeout=90)
        cached = _cache_get(_MULTI_INFO_CACHE, url)
        if cached is not None and not _info_expired(cached):
            return cached

    try:
        with yt_dlp.YoutubeDL(_multi_opts()) as ydl:
            result = ydl.extract_info(url, download=False) or {}
        _cache_set(_MULTI_INFO_CACHE, url, result)
        return result
    finally:
        if is_owner:
            with _INFLIGHT_LOCK:
                _INFLIGHT.pop(flight_key, None)
            event.set()


_TRACK_NOTE_CLEAN_RE = re.compile(r",?\s*(low|medium|high|DRC)\b|\s*\(default\)|\s*original", re.I)


def audio_tracks(url: str) -> dict[str, Any]:
    """Idiomas de audio disponibles (dubs) para un video."""
    info = _extract_info_multi(url)
    tracks: dict[str, dict[str, Any]] = {}
    for fmt in info.get("formats") or []:
        if not fmt.get("url") or fmt.get("acodec") in (None, "none") or fmt.get("vcodec") not in (None, "none"):
            continue
        if str(fmt.get("format_id") or "").endswith("-drc"):
            continue
        lang = fmt.get("language") or ""
        if not lang or lang in tracks:
            continue
        note = fmt.get("format_note") or ""
        name = _TRACK_NOTE_CLEAN_RE.sub("", note).strip(" ,") or lang
        tracks[lang] = {
            "lang": lang,
            "name": name,
            "default": "original" in note.lower(),
        }
    ordered = sorted(tracks.values(), key=lambda t: (not t["default"], t["name"].lower()))
    return {"tracks": ordered}


def _select_formats(info: dict[str, Any], quality: str | None = None, audio_lang: str | None = None) -> tuple[dict[str, Any], dict[str, Any]]:
    max_height = _quality_height(quality or config.PLAYBACK_QUALITY or "best")
    formats = info.get("formats") or []
    videos = [
        f for f in formats
        if f.get("url") and f.get("vcodec") not in (None, "none") and f.get("acodec") in (None, "none")
        and (not max_height or int(f.get("height") or 0) <= max_height)
        and _is_browser_video_candidate(f)
    ]
    audios = [
        f for f in formats
        if f.get("url") and f.get("acodec") not in (None, "none") and f.get("vcodec") in (None, "none")
        and _is_browser_audio_candidate(f)
    ]
    if audio_lang:
        by_lang = [
            f for f in audios
            if not str(f.get("format_id") or "").endswith("-drc")
            and str(f.get("language") or "").startswith(audio_lang)
        ]
        if by_lang:
            # Manifest DASH necesita cajas ISOBMFF: preferir m4a/mp4 si existen
            mp4_only = [f for f in by_lang if (f.get("ext") or "").lower() in {"m4a", "mp4"}]
            audios = mp4_only or by_lang
    if not videos or not audios:
        requested = info.get("requested_formats") or []
        if len(requested) >= 2:
            return requested[0], requested[1]
        raise RuntimeError("No se encontraron formatos DASH reproducibles para el navegador.")
    videos.sort(key=_video_score, reverse=True)
    audios.sort(key=_audio_score, reverse=True)
    return videos[0], audios[0]


def _available_qualities(info: dict[str, Any]) -> list[str]:
    formats = info.get("formats") or []
    heights = sorted(
        {
            int(f.get("height") or 0)
            for f in formats
            if f.get("url")
            and f.get("vcodec") not in (None, "none")
            and f.get("acodec") in (None, "none")
            and _is_browser_video_candidate(f)
            and int(f.get("height") or 0) >= 144
        },
        reverse=True,
    )
    return [f"{h}p" for h in heights]


def _is_browser_video_candidate(fmt: dict[str, Any]) -> bool:
    # Only MP4 container — VHS requires ISOBMFF SIDX for DASH; WebM uses EBML Cues (incompatible)
    ext = (fmt.get("ext") or "").lower()
    codec = (fmt.get("vcodec") or "").lower()
    return ext == "mp4" and (
        codec.startswith("avc1") or codec.startswith("av01") or codec.startswith("vp09") or codec.startswith("vp9")
    )


def _is_browser_audio_candidate(fmt: dict[str, Any]) -> bool:
    ext = (fmt.get("ext") or "").lower()
    codec = (fmt.get("acodec") or "").lower()
    return ext in {"m4a", "mp4", "webm"} and (
        codec.startswith("mp4a") or codec.startswith("opus") or codec.startswith("vorbis")
    )


def _video_score(fmt: dict[str, Any]) -> tuple:
    codec = (fmt.get("vcodec") or "").lower()
    # Prefer avc1 (widest compat) then av01; height is primary, codec secondary
    codec_bonus = 2 if codec.startswith("avc1") else 1
    return (int(fmt.get("height") or 0), int(fmt.get("fps") or 0), codec_bonus, float(fmt.get("tbr") or 0))


def _audio_score(fmt: dict[str, Any]) -> tuple:
    codec = (fmt.get("acodec") or "").lower()
    ext = (fmt.get("ext") or "").lower()
    codec_bonus = 3 if codec.startswith("mp4a") else 2 if codec.startswith("opus") else 1
    ext_bonus = 1 if ext in {"m4a", "mp4"} else 0
    return (codec_bonus, ext_bonus, float(fmt.get("abr") or fmt.get("tbr") or 0))


def _quality_height(value: str) -> int | None:
    value = (value or "best").lower()
    if value == "best":
        return None
    digits = "".join(ch for ch in value if ch.isdigit())
    return int(digits) if digits else None


def _mime(fmt: dict[str, Any]) -> str:
    ext = (fmt.get("ext") or "").lower()
    if fmt.get("vcodec") not in (None, "none"):
        return "video/webm" if ext == "webm" else "video/mp4"
    return "audio/webm" if ext == "webm" else "audio/mp4"


def _proxy_url(fmt: dict[str, Any]) -> str:
    return f"/api/player/proxy?src={quote(fmt['url'], safe='')}"


def _segment_ranges(fmt: dict[str, Any]) -> tuple[str, str] | None:
    """Return (init_range, index_range) or None if format doesn't have ISOBMFF sidx."""
    cache_key = fmt.get("url", "")
    cached = _cache_get(_SEGMENT_RANGE_CACHE, cache_key)
    if cached is not None:
        return cached if (cached[0] or cached[1]) else None

    # Prefer HTTP header hints (fast path, no extra request)
    init_range = fmt.get("init_range") or {}
    index_range = fmt.get("index_range") or {}
    if init_range.get("end") and index_range.get("start") and index_range.get("end"):
        init_str = f"{init_range.get('start', 0)}-{init_range['end']}"
        idx_str = f"{index_range['start']}-{index_range['end']}"
        result = (init_str, idx_str)
        _cache_set(_SEGMENT_RANGE_CACHE, cache_key, result)
        return result

    # Fall back to byte-range probe
    try:
        headers = dict(fmt.get("http_headers") or {})
        headers["Range"] = "bytes=0-2097151"  # 2 MB — sidx may be past 1 MB
        with httpx.Client(follow_redirects=True, timeout=30) as client:
            response = client.get(fmt["url"], headers=headers)
            response.raise_for_status()
        data = response.content
        boxes = []
        pos = 0
        while pos + 8 <= len(data):
            size = struct.unpack(">I", data[pos:pos + 4])[0]
            box_type = data[pos + 4:pos + 8].decode("latin1")
            header_size = 8
            if size == 1:
                if pos + 16 > len(data):
                    break
                size = struct.unpack(">Q", data[pos + 8:pos + 16])[0]
                header_size = 16
            if size < header_size or size > 100_000_000:
                break
            boxes.append((box_type, pos, pos + size - 1))
            pos += size
            if box_type == "sidx":
                break
        moov = next((box for box in boxes if box[0] == "moov"), None)
        sidx = next((box for box in boxes if box[0] == "sidx"), None)
        if moov and sidx:
            result = (f"0-{moov[2]}", f"{sidx[1]}-{sidx[2]}")
            _cache_set(_SEGMENT_RANGE_CACHE, cache_key, result)
            return result
        logger.warning(
            "Formato %s sin cajas moov/sidx en los primeros 2MB (boxes=%s)",
            fmt.get("format_id"), [b[0] for b in boxes],
        )
    except Exception:
        logger.warning("Fallo el probe de rangos DASH para formato %s", fmt.get("format_id"), exc_info=True)

    # No sidx found — signal that SegmentBase should be omitted
    _cache_set(_SEGMENT_RANGE_CACHE, cache_key, ("", ""))
    return None


def _cache_get(cache: dict[str, tuple[float, Any]], key: str) -> Any | None:
    with _CACHE_LOCK:
        cached = cache.get(key)
        if not cached:
            return None
        ts, value = cached
        if time.monotonic() - ts > _CACHE_TTL:
            cache.pop(key, None)
            return None
        return value


def _cache_set(cache: dict[str, tuple[float, Any]], key: str, value: Any) -> None:
    if not key:
        return
    with _CACHE_LOCK:
        if len(cache) >= _CACHE_MAX:
            oldest = sorted(cache.items(), key=lambda kv: kv[1][0])
            for old_key, _ in oldest[:len(cache) - _CACHE_MAX + 1]:
                cache.pop(old_key, None)
        cache[key] = (time.monotonic(), value)


def audio_info(url: str) -> dict[str, Any]:
    info = _extract_info(url)
    formats = info.get("formats") or []
    audios = [
        f for f in formats
        if f.get("url") and f.get("acodec") not in (None, "none") and f.get("vcodec") in (None, "none")
        and _is_browser_audio_candidate(f)
    ]
    if audios:
        audios.sort(key=_audio_score, reverse=True)
        audio = audios[0]
    else:
        requested = info.get("requested_formats") or []
        if len(requested) >= 2:
            audio = requested[1]
        elif requested:
            audio = requested[0]
        else:
            raise RuntimeError("No se encontro formato de audio reproducible.")
    item = _entry_to_item(info)
    return {
        "item": item,
        "duration": float(info.get("duration") or audio.get("duration") or 0),
        "proxyUrl": _proxy_url(audio),
        "contentType": _mime(audio),
        "loudnessDb": _fetch_loudness_db(url, info),
    }


def _public_format(fmt: dict[str, Any]) -> dict[str, Any]:
    return {
        "formatId": fmt.get("format_id"),
        "ext": fmt.get("ext"),
        "height": fmt.get("height"),
        "width": fmt.get("width"),
        "fps": fmt.get("fps"),
        "vcodec": fmt.get("vcodec"),
        "acodec": fmt.get("acodec"),
        "tbr": fmt.get("tbr"),
    }


def _with_manifest_urls(result: dict[str, Any], quality: str | None = None) -> dict[str, Any]:
    selected_quality = quality or config.PLAYBACK_QUALITY or "best"
    items = []
    for entry in result.get("items") or []:
        url = entry.get("url")
        if not url:
            continue
        items.append({
            **entry,
            "manifestUrl": f"/api/player/manifest.mpd?url={quote(url, safe='')}&quality={quote(selected_quality, safe='')}",
        })
    return {**result, "items": items}
