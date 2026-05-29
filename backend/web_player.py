from __future__ import annotations

import html
import struct
import time
from typing import Any
from urllib.parse import parse_qs, quote, urlparse

import httpx
import yt_dlp

from . import config
from .player import playlist_entries
from .ytmusic import music_playlist_queue
from .youtube import _entry_to_item
from .ytdlp_opts import javascript_runtime_opts


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
        "manifestUrl": f"/api/player/manifest.mpd?url={quote(url, safe='')}&quality={quote(quality or config.PLAYBACK_QUALITY or 'best', safe='')}",
    }


def manifest(url: str, quality: str | None = None) -> str:
    cache_key = f"{url}|{quality or config.PLAYBACK_QUALITY or 'best'}"
    cached = _cache_get(_MANIFEST_CACHE, cache_key)
    if cached is not None:
        return cached
    info = _extract_info(url)
    video, audio = _select_formats(info, quality)
    duration = float(info.get("duration") or video.get("duration") or audio.get("duration") or 0)
    if duration <= 0:
        duration = 36000
    video_mime = _mime(video)
    audio_mime = _mime(audio)
    video_codec = html.escape(video.get("vcodec") or "avc1.640028")
    audio_codec = html.escape(audio.get("acodec") or "mp4a.40.2")
    video_url = html.escape(_proxy_url(video))
    audio_url = html.escape(_proxy_url(audio))
    video_init, video_index = _segment_ranges(video)
    audio_init, audio_index = _segment_ranges(audio)
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
        <SegmentBase indexRange="{video_index}" indexRangeExact="true">
          <Initialization range="{video_init}" />
        </SegmentBase>
      </Representation>
    </AdaptationSet>
    <AdaptationSet id="audio" contentType="audio" mimeType="{audio_mime}" codecs="{audio_codec}" startWithSAP="1" subsegmentAlignment="true">
      <Representation id="{html.escape(str(audio.get('format_id') or 'audio'))}" bandwidth="{bandwidth_a}" audioSamplingRate="{int(audio.get('asr') or 44100)}">
        <AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="{int(audio.get('audio_channels') or 2)}" />
        <BaseURL>{audio_url}</BaseURL>
        <SegmentBase indexRange="{audio_index}" indexRangeExact="true">
          <Initialization range="{audio_init}" />
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
    if parsed.hostname == "music.youtube.com" and query.get("list"):
        result = music_playlist_queue(query["list"][0], limit=100)
        return _with_manifest_urls(result, quality)

    entries = playlist_entries(url, limit=100)
    return _with_manifest_urls({"items": entries, "title": "Playlist", "source": "youtube", "requiresLogin": False, "error": ""}, quality)


_INFO_CACHE: dict[str, tuple[float, dict]] = {}  # url -> (timestamp, info)
_MANIFEST_CACHE: dict[str, tuple[float, str]] = {}
_SEGMENT_RANGE_CACHE: dict[str, tuple[float, tuple[str, str]]] = {}
_CACHE_TTL = 300  # 5 minutes
_CACHE_MAX = 20


def _extract_info(url: str) -> dict[str, Any]:
    now = time.monotonic()
    cached = _cache_get(_INFO_CACHE, url)
    if cached is not None:
        return cached
    opts = {
        "quiet": True,
        "skip_download": True,
        "noplaylist": True,
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        result = ydl.extract_info(url, download=False) or {}
    _cache_set(_INFO_CACHE, url, result)
    return result


def _select_formats(info: dict[str, Any], quality: str | None = None) -> tuple[dict[str, Any], dict[str, Any]]:
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
    if not videos or not audios:
        requested = info.get("requested_formats") or []
        if len(requested) >= 2:
            return requested[0], requested[1]
        raise RuntimeError("No se encontraron formatos DASH reproducibles para el navegador.")
    videos.sort(key=_video_score, reverse=True)
    audios.sort(key=_audio_score, reverse=True)
    return videos[0], audios[0]


def _is_browser_video_candidate(fmt: dict[str, Any]) -> bool:
    ext = (fmt.get("ext") or "").lower()
    codec = (fmt.get("vcodec") or "").lower()
    return ext in {"mp4", "webm"} and (
        codec.startswith("avc1") or codec.startswith("vp9") or codec.startswith("av01")
    )


def _is_browser_audio_candidate(fmt: dict[str, Any]) -> bool:
    ext = (fmt.get("ext") or "").lower()
    codec = (fmt.get("acodec") or "").lower()
    return ext in {"m4a", "mp4", "webm"} and (
        codec.startswith("mp4a") or codec.startswith("opus") or codec.startswith("vorbis")
    )


def _video_score(fmt: dict[str, Any]) -> tuple:
    codec = (fmt.get("vcodec") or "").lower()
    ext = (fmt.get("ext") or "").lower()
    codec_bonus = 3 if codec.startswith("avc1") else 2 if codec.startswith("vp9") else 1
    ext_bonus = 1 if ext == "mp4" else 0
    return (int(fmt.get("height") or 0), int(fmt.get("fps") or 0), codec_bonus, ext_bonus, float(fmt.get("tbr") or 0))


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


def _segment_ranges(fmt: dict[str, Any]) -> tuple[str, str]:
    cache_key = fmt.get("url", "")
    cached = _cache_get(_SEGMENT_RANGE_CACHE, cache_key)
    if cached is not None:
        return cached
    headers = dict(fmt.get("http_headers") or {})
    headers["Range"] = "bytes=0-1048575"
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
        if size < header_size:
            break
        boxes.append((box_type, pos, pos + size - 1))
        pos += size
        if box_type == "sidx":
            break
    moov = next((box for box in boxes if box[0] == "moov"), None)
    sidx = next((box for box in boxes if box[0] == "sidx"), None)
    if not moov or not sidx:
        raise RuntimeError(f"No se pudieron detectar rangos DASH para formato {fmt.get('format_id')}")
    ranges = (f"0-{moov[2]}", f"{sidx[1]}-{sidx[2]}")
    _cache_set(_SEGMENT_RANGE_CACHE, cache_key, ranges)
    return ranges


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
    if not key:
        return
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
