from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, quote, urlparse

import yt_dlp
from fastapi import HTTPException

from . import config
from .history import add_history_event
from .youtube import is_youtube_url
from .ytdlp_opts import javascript_runtime_opts


def play(target: str, player: str | None = None, title: str | None = None, thumbnail: str | None = None, channel: str | None = None) -> dict[str, Any]:
    selected = (player or config.DEFAULT_PLAYER or "mpv").lower()
    exe = config.MPV_PATH if selected == "mpv" else config.VLC_PATH if selected == "vlc" else None
    if not exe:
        raise HTTPException(status_code=400, detail="Reproductor no soportado")
    if not target.startswith("http"):
        path = Path(target).resolve()
        if not path.exists():
            raise HTTPException(status_code=404, detail="Archivo no encontrado")
        try:
            path.relative_to(config.MEDIA_ROOT.resolve())
        except ValueError:
            raise HTTPException(status_code=400, detail="Ruta fuera del directorio de medios")
        target = str(path)
    try:
        launch_target = target
        args = _player_args(exe, selected, target)
        subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except FileNotFoundError as exc:
        raise HTTPException(status_code=500, detail=f"No se encontro {selected}: {exe}") from exc
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc)) from exc

    add_history_event({
        "id": f"play-{time.time()}",
        "action": "play",
        "title": title or target,
        "url": target if str(target).startswith("http") else "",
        "target": launch_target,
        "player": selected,
        "thumbnail": thumbnail or "",
        "channel": channel or "",
    })
    return {"ok": True, "player": selected, "target": target}


def _player_args(exe: str, selected: str, target: str) -> list[str]:
    if selected == "mpv":
        if is_youtube_url(target):
            if sys.platform == "win32":
                ytdlp_path = str(config.ROOT / ".venv" / "Scripts" / "yt-dlp.exe")
            else:
                ytdlp_path = str(config.ROOT / ".venv" / "bin" / "yt-dlp")
            return [
                exe,
                f"--script-opts=ytdl_hook-ytdl_path={ytdlp_path}",
                "--ytdl-format",
                _format_selector(quality=config.PLAYBACK_QUALITY),
                target,
            ]
        return [exe, target]
    if selected == "vlc" and is_youtube_url(target):
        if _is_playlist_url(target):
            playlist = (
                f"http://{config.APP_HOST}:{config.APP_PORT}/api/playlist.m3u"
                f"?url={quote(target, safe='')}&quality={quote(config.PLAYBACK_QUALITY or 'best', safe='')}"
            )
            return [exe, "--network-caching=2500", playlist]
        local_stream = (
            f"http://{config.APP_HOST}:{config.APP_PORT}/api/stream"
            f"?url={quote(target, safe='')}&quality={quote(config.PLAYBACK_QUALITY or 'best', safe='')}"
        )
        return [exe, "--network-caching=2500", local_stream]
    return [exe, target]


def _resolve_vlc_stream(url: str, quality: str | None = None) -> dict[str, Any]:
    opts = {
        "quiet": True,
        "skip_download": True,
        "noplaylist": True,
        "format": _format_selector(vlc=True, quality=quality),
        "format_sort": ["res", "fps", "codec:h264", "ext:mp4:m4a"],
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(url, download=False)
    stream_url = info.get("url")
    if not stream_url:
        raise RuntimeError("yt-dlp no pudo resolver un stream reproducible para VLC")
    http_headers = dict(info.get("http_headers") or {})
    user_agent = http_headers.get(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36",
    )
    referer = http_headers.get("Referer") or "https://www.youtube.com/"
    http_headers.setdefault("User-Agent", user_agent)
    http_headers.setdefault("Referer", referer)
    return {"url": stream_url, "userAgent": user_agent, "referer": referer, "headers": http_headers}


def resolve_stream_info(url: str, quality: str | None = None) -> dict[str, Any]:
    opts = {
        "quiet": True,
        "skip_download": True,
        "noplaylist": True,
        "format": _format_selector(quality=quality),
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        return ydl.extract_info(url, download=False) or {}


def playlist_entries(url: str, limit: int = 100) -> list[dict[str, Any]]:
    parsed = urlparse(url)
    query = parse_qs(parsed.query)
    if parsed.hostname == "music.youtube.com" and query.get("list"):
        music_entries = _music_playlist_entries(query["list"][0], limit)
        if music_entries:
            return music_entries
    opts = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "playlistend": limit,
        "ignoreerrors": True,
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(url, download=False) or {}
    entries = []
    for entry in info.get("entries") or []:
        if not entry:
            continue
        entry_url = entry.get("webpage_url") or entry.get("url")
        if entry_url and not str(entry_url).startswith("http"):
            entry_url = f"https://www.youtube.com/watch?v={entry_url}"
        if entry_url:
            entries.append({"url": entry_url, "title": entry.get("title") or entry_url})
    return entries


def _music_playlist_entries(playlist_id: str, limit: int) -> list[dict[str, Any]]:
    try:
        from .ytmusic import _ytmusic_client

        ytmusic = _ytmusic_client(prefer_auth=True)
        data = ytmusic.get_watch_playlist(playlistId=playlist_id, limit=limit)
        tracks = data.get("tracks") or []
    except Exception:
        return []
    entries = []
    for track in tracks:
        video_id = track.get("videoId")
        if not video_id:
            continue
        entries.append({
            "url": f"https://music.youtube.com/watch?v={video_id}",
            "title": track.get("title") or video_id,
        })
    return entries


def _is_playlist_url(value: str) -> bool:
    parsed = urlparse(value)
    query = parse_qs(parsed.query)
    return bool(query.get("list")) and not query.get("v")


def _format_selector(vlc: bool = False, quality: str | None = None) -> str:
    selected_quality = (quality or config.PLAYBACK_QUALITY or "best").lower()
    if selected_quality == "best":
        return "best[ext=mp4][acodec!=none][vcodec!=none][protocol^=http]/best[protocol^=http]/best" if vlc else "bestvideo+bestaudio/best"
    height = "".join(ch for ch in selected_quality if ch.isdigit())
    if not height:
        return "best[ext=mp4][acodec!=none][vcodec!=none][protocol^=http]/best[protocol^=http]/best" if vlc else "bestvideo+bestaudio/best"
    if vlc:
        return (
            f"best[height<={height}][ext=mp4][acodec!=none][vcodec!=none][protocol^=http]/"
            f"best[height<={height}][acodec!=none][vcodec!=none][protocol^=http]/"
            f"best[ext=mp4][acodec!=none][vcodec!=none][protocol^=http]/"
            "best[protocol^=http]/best"
        )
    return f"bv*[height<={height}]+ba/b[height<={height}]/best"
