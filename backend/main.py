from __future__ import annotations

import logging
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor as _ThreadPoolExecutor
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Literal
from urllib.parse import quote, urlparse

import httpx
from fastapi import FastAPI, HTTPException
from fastapi import Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, Response, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field
from starlette.background import BackgroundTask

from . import config
from .autosync import get_sync_status, refresh_all, refresh_section, start_autosync
from .channels import add_channel, delete_channel, list_channels, list_feed, refresh_all_channels, refresh_channel
from .collections import create_collection, delete_collection, list_collections
from .history import delete_history_event
from .playlists import add_item as playlist_add_item
from .playlists import create_playlist, delete_playlist, import_playlist, list_playlists, remove_item as playlist_remove_item, rename_playlist, reorder_items as reorder_playlist_items, sync_playlist
from .downloads import cancel_download, delete_download, list_download_status, list_downloads, list_history, list_library, start_download
from .state import read_state_key
from .player import _resolve_vlc_stream, playlist_entries, play, resolve_stream_info
from .recommendations import list_recommendations
from .tasks import get_task, list_tasks, start_task
from .web_player import audio_info
from .web_player import manifest as player_manifest
from .web_player import player_info, resolve_playlist
from .ytmusic import (
    fetch_playlist_songs, list_music_library, list_music_playlists, list_music_recommendations,
    refresh_music_library, search_music,
)
from .youtube import fetch_radio_items, search

logger = logging.getLogger(__name__)

_THUMBNAIL_HOSTS = frozenset({
    "i.ytimg.com",
    "yt3.ggpht.com",
    "i9.ytimg.com",
    "lh3.googleusercontent.com",
    "yt3.googleusercontent.com",
    "www.gstatic.com",
    "music.youtube.com",
})
_THUMBNAIL_HOST_SUFFIXES = (
    ".ytimg.com",
    ".googleusercontent.com",
    ".ggpht.com",
    ".gstatic.com",
)
_THUMBNAIL_HEADERS = {
    "User-Agent": (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
        "(KHTML, like Gecko) Chrome/125.0 Safari/537.36"
    ),
    "Referer": "https://www.youtube.com/",
    "Accept": "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
}
_THUMB_CACHE: dict[str, tuple[float, bytes, str]] = {}
_THUMB_CACHE_TTL = 24 * 60 * 60
_THUMB_CACHE_MAX = 250

_HTTP_CLIENT = httpx.Client(
    timeout=None,
    follow_redirects=True,
    limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
)
_SEARCH_POOL = _ThreadPoolExecutor(max_workers=2, thread_name_prefix="playzi-search")


def _thumbnail_host_allowed(hostname: str | None) -> bool:
    host = (hostname or "").lower()
    return host in _THUMBNAIL_HOSTS or any(host.endswith(suffix) for suffix in _THUMBNAIL_HOST_SUFFIXES)

config.ensure_dirs()


@asynccontextmanager
async def _lifespan(app):
    start_autosync()
    yield


app = FastAPI(title="Playzi", version="0.1.0", lifespan=_lifespan)
app.add_middleware(
    CORSMiddleware,
    allow_origins=[
        f"http://{config.APP_HOST}:{config.APP_PORT}",
        f"http://localhost:{config.APP_PORT}",
        f"http://127.0.0.1:{config.APP_PORT}",
    ],
    allow_methods=["*"],
    allow_headers=["*"],
)

ROOT = Path(__file__).resolve().parents[1]
STATIC_DIR = ROOT / "static"
app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")
app.mount("/media", StaticFiles(directory=config.MEDIA_ROOT), name="media")


class DownloadRequest(BaseModel):
    url: str
    title: str | None = None
    channel: str | None = None
    thumbnail: str | None = None
    kind: Literal["video", "audio"] = "video"
    quality: str | None = "1080p"
    collection: str | None = ""


class PlayRequest(BaseModel):
    target: str = Field(min_length=1)
    player: Literal["mpv", "vlc"] | None = None
    title: str | None = None
    thumbnail: str | None = None
    channel: str | None = None


class ConfigUpdate(BaseModel):
    mediaRoot: str | None = None
    defaultPlayer: Literal["mpv", "vlc"] | None = None
    mpvPath: str | None = None
    vlcPath: str | None = None
    ffmpegPath: str | None = None
    audioFormat: Literal["opus", "mp3", "m4a"] | None = None
    searchLimit: int | None = None
    playbackQuality: Literal["best", "2160p", "1440p", "1080p", "720p", "480p", "360p"] | None = None
    cookiesBrowser: str | None = None
    cookiesFile: str | None = None


class ChannelRequest(BaseModel):
    url: str = Field(min_length=1)
    name: str | None = None


class CollectionRequest(BaseModel):
    name: str = Field(min_length=1)


class PlaylistRequest(BaseModel):
    name: str = Field(min_length=1)


class PlaylistItemRequest(BaseModel):
    url: str
    title: str | None = None
    thumbnail: str | None = None
    channel: str | None = None
    durationText: str | None = None


class PlaylistItemRemoveRequest(BaseModel):
    url: str


class PlaylistImportRequest(BaseModel):
    url: str = Field(min_length=1)
    mode: Literal["copy", "linked"] = "copy"


class PlaylistRenameRequest(BaseModel):
    name: str = Field(min_length=1)


class PlaylistReorderRequest(BaseModel):
    urls: list[str]


class TaskStartResponse(BaseModel):
    task: dict


@app.get("/")
def index():
    return FileResponse(STATIC_DIR / "index.html")


@app.get("/api/config")
def api_config():
    return config.public_config()


def _run_youtube_login() -> dict:
    """Open a visible Chromium for the user to log into YouTube, then refresh cookies."""
    from .managed_browser import open_login_browser
    from .ytmusic import _get_validated_client

    open_login_browser()  # blocks until the user closes the window
    _client, state = _get_validated_client()
    return {"authState": state}


def _bootstrap_payload() -> dict:
    recommendations = list_recommendations()
    music_recommendations = list_music_recommendations()
    music_playlists = list_music_playlists()
    return {
        "config": config.public_config(),
        "downloads": {"items": list_downloads()},
        "downloadStatus": {"items": list_download_status()},
        "library": {"items": list_library()},
        "history": {"items": list_history()},
        "channels": {"items": list_channels()},
        "feed": {"items": list_feed()},
        "collections": {"items": list_collections()},
        "playlists": {"items": list_playlists()},
        "recommendations": recommendations,
        "musicRecommendations": music_recommendations,
        "musicPlaylists": music_playlists,
        "musicLibrary": list_music_library(),
        "channelsLastSync": read_state_key("channelsLastSync"),
        "musicAuthState": read_state_key("musicAuthState"),
        "syncStatus": get_sync_status(),
    }


@app.get("/api/bootstrap")
def api_bootstrap():
    return _bootstrap_payload()


@app.get("/api/state/summary")
def api_state_summary():
    return {
        "downloadStatus": {"items": list_download_status()},
        "downloads": {"items": list_downloads()},
        "syncStatus": get_sync_status(),
    }


@app.post("/api/config")
def api_config_update(payload: ConfigUpdate):
    try:
        return config.update_config(payload.model_dump(exclude_none=True))
    except Exception as exc:
        logger.exception("Error al actualizar configuracion")
        raise HTTPException(status_code=400, detail="Error al guardar configuracion") from exc


@app.get("/api/search")
def api_search(q: str, scope: str | None = None, search_filter: str | None = None, limit: int | None = None):
    try:
        selected = (scope or "all").lower()
        music_limit = limit or 20
        if selected == "music":
            return {"items": search_music(q, limit=music_limit, search_filter=search_filter)}
        if selected == "youtube":
            return {"items": search(q, limit=limit)}
        _music_limit = max(6, config.YTDLP_SEARCH_LIMIT // 2)
        yt_fut = _SEARCH_POOL.submit(search, q)
        mus_fut = _SEARCH_POOL.submit(lambda: search_music(q, limit=_music_limit))
        yt_items = yt_fut.result(timeout=60)
        try:
            music_items = mus_fut.result(timeout=60)
        except Exception:
            music_items = []
        return {"items": [*yt_items, *music_items]}
    except Exception as exc:
        logger.exception("Error en busqueda: %s", q)
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.post("/api/downloads")
def api_downloads(payload: DownloadRequest):
    return start_download(payload.model_dump())


@app.get("/api/downloads")
def api_downloads_list():
    return {"items": list_downloads()}


@app.get("/api/downloads/status")
def api_downloads_status():
    return {"items": list_download_status()}


@app.delete("/api/downloads/{download_id}")
def api_downloads_delete(download_id: str):
    delete_download(download_id)
    return {"ok": True}


@app.post("/api/downloads/{download_id}/cancel")
def api_downloads_cancel(download_id: str):
    cancel_download(download_id)
    return {"ok": True}


@app.get("/api/library")
def api_library():
    return {"items": list_library()}


@app.get("/api/history")
def api_history():
    return {"items": list_history()}


@app.delete("/api/history/{event_id}")
def api_history_delete(event_id: str):
    delete_history_event(event_id)
    return {"ok": True}


@app.get("/api/channels")
def api_channels():
    return {"items": list_channels()}


@app.post("/api/channels")
def api_channels_add(payload: ChannelRequest):
    try:
        return add_channel(payload.url, payload.name)
    except Exception as exc:
        logger.exception("Error al agregar canal: %s", payload.url)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.delete("/api/channels/{channel_id}")
def api_channels_delete(channel_id: str):
    delete_channel(channel_id)
    return {"ok": True}


@app.post("/api/channels/{channel_id}/refresh")
def api_channel_refresh(channel_id: str):
    try:
        return {"task": start_task("refresh-channel", lambda: refresh_channel(channel_id))}
    except Exception as exc:
        logger.exception("Error al refrescar canal: %s", channel_id)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.post("/api/channels/refresh")
def api_channels_refresh():
    return {"task": start_task("refresh-channels", refresh_all_channels)}


@app.get("/api/feed")
def api_feed():
    return {"items": list_feed()}


@app.get("/api/collections")
def api_collections():
    return {"items": list_collections()}


@app.post("/api/collections")
def api_collections_create(payload: CollectionRequest):
    try:
        return create_collection(payload.name)
    except Exception as exc:
        logger.exception("Error al crear coleccion: %s", payload.name)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.delete("/api/collections/{collection_id}")
def api_collections_delete(collection_id: str):
    delete_collection(collection_id)
    return {"ok": True}


@app.get("/api/playlists")
def api_playlists():
    return {"items": list_playlists()}


@app.post("/api/playlists")
def api_playlists_create(payload: PlaylistRequest):
    try:
        return create_playlist(payload.name)
    except Exception as exc:
        logger.exception("Error al crear playlist: %s", payload.name)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.post("/api/playlists/import")
def api_playlists_import(payload: PlaylistImportRequest):
    try:
        return import_playlist(payload.url, payload.mode)
    except Exception as exc:
        logger.exception("Error al importar playlist: %s", payload.url)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.delete("/api/playlists/{playlist_id}")
def api_playlists_delete(playlist_id: str):
    delete_playlist(playlist_id)
    return {"ok": True}


@app.post("/api/playlists/{playlist_id}/sync")
def api_playlists_sync(playlist_id: str):
    try:
        return sync_playlist(playlist_id)
    except Exception as exc:
        logger.exception("Error al sincronizar playlist: %s", playlist_id)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.patch("/api/playlists/{playlist_id}")
def api_playlists_rename(playlist_id: str, payload: PlaylistRenameRequest):
    try:
        return rename_playlist(playlist_id, payload.name)
    except Exception as exc:
        logger.exception("Error al renombrar playlist: %s", playlist_id)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.post("/api/playlists/{playlist_id}/items/reorder")
def api_playlists_reorder(playlist_id: str, payload: PlaylistReorderRequest):
    try:
        return reorder_playlist_items(playlist_id, payload.urls)
    except Exception as exc:
        logger.exception("Error al reordenar playlist: %s", playlist_id)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.post("/api/playlists/{playlist_id}/items")
def api_playlists_add_item(playlist_id: str, payload: PlaylistItemRequest):
    try:
        return playlist_add_item(playlist_id, payload.model_dump(exclude_none=True))
    except Exception as exc:
        logger.exception("Error al agregar item a playlist: %s", playlist_id)
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@app.delete("/api/playlists/{playlist_id}/items")
def api_playlists_remove_item(playlist_id: str, url: str):
    playlist_remove_item(playlist_id, url)
    return {"ok": True}


@app.post("/api/playlists/{playlist_id}/items/remove")
def api_playlists_remove_item_post(playlist_id: str, payload: PlaylistItemRemoveRequest):
    playlist_remove_item(playlist_id, payload.url)
    return {"ok": True}


@app.get("/api/thumbnail")
def api_thumbnail(url: str):
    if url.startswith("//"):
        url = f"https:{url}"
    parsed = urlparse(url)
    if parsed.scheme not in {"http", "https"}:
        raise HTTPException(status_code=400, detail="URL de thumbnail invalida")
    if not _thumbnail_host_allowed(parsed.hostname):
        raise HTTPException(status_code=400, detail="Dominio de thumbnail no permitido")
    cached = _THUMB_CACHE.get(url)
    if cached and time.monotonic() - cached[0] < _THUMB_CACHE_TTL:
        return Response(
            content=cached[1],
            media_type=cached[2],
            headers={"Cache-Control": "public, max-age=86400"},
        )
    try:
        response = httpx.get(url, headers=_THUMBNAIL_HEADERS, timeout=12, follow_redirects=True)
        response.raise_for_status()
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"No se pudo cargar thumbnail: {exc}") from exc
    content_type = response.headers.get("content-type", "image/jpeg")
    if not content_type.startswith("image/"):
        content_type = "image/jpeg"
    if len(_THUMB_CACHE) >= _THUMB_CACHE_MAX:
        _THUMB_CACHE.pop(next(iter(_THUMB_CACHE)))
    _THUMB_CACHE[url] = (time.monotonic(), response.content, content_type)
    return Response(
        content=response.content,
        media_type=content_type,
        headers={"Cache-Control": "public, max-age=86400"},
    )


@app.get("/api/stream")
def api_stream(url: str, request: Request, quality: str | None = None):
    selected_quality = (quality or config.PLAYBACK_QUALITY or "best").lower()
    height = int("".join(ch for ch in selected_quality if ch.isdigit()) or "0")
    if selected_quality == "best" or height >= 720:
        try:
            return _ffmpeg_stream(url, quality)
        except Exception:
            logger.exception("Error al remuxear stream, usando directo: %s", url)
    return _direct_stream(url, request, quality)


def _direct_stream(url: str, request: Request, quality: str | None = None):
    try:
        stream = _resolve_vlc_stream(url, quality)
    except Exception as exc:
        logger.exception("Error al resolver stream: %s", url)
        raise HTTPException(status_code=502, detail=f"No se pudo resolver stream: {exc}") from exc

    range_header = request.headers.get("range")
    headers = dict(stream.get("headers") or {})
    headers.setdefault("User-Agent", stream["userAgent"])
    headers.setdefault("Referer", stream["referer"])
    if range_header:
        headers["Range"] = range_header

    outbound = _HTTP_CLIENT.build_request("GET", stream["url"], headers=headers)
    response = _HTTP_CLIENT.send(outbound, stream=True)
    if response.status_code >= 400:
        response.close()
        raise HTTPException(status_code=502, detail=f"Stream remoto respondio {response.status_code}")

    response_headers = {"Accept-Ranges": "bytes"}
    for key in ("content-length", "content-range", "content-type"):
        if response.headers.get(key):
            response_headers[key.title()] = response.headers[key]

    def close_stream() -> None:
        response.close()

    return StreamingResponse(
        response.iter_bytes(1024 * 256),
        media_type=response.headers.get("content-type", "video/mp4"),
        headers=response_headers,
        status_code=response.status_code,
        background=BackgroundTask(close_stream),
    )


def _ffmpeg_stream(url: str, quality: str | None = None):
    try:
        info = resolve_stream_info(url, quality)
    except Exception as exc:
        logger.exception("Error al resolver stream ffmpeg: %s", url)
        raise HTTPException(status_code=502, detail=f"No se pudo resolver stream: {exc}") from exc

    requested = info.get("requested_formats") or []
    if len(requested) < 2:
        raise RuntimeError("yt-dlp no devolvio video y audio separados")

    video = requested[0]
    audio = requested[1]
    video_headers = video.get("http_headers") or {}
    audio_headers = audio.get("http_headers") or {}
    common_headers = {**audio_headers, **video_headers}
    header_text = "".join(f"{key}: {value}\r\n" for key, value in common_headers.items())
    ffmpeg = config.FFMPEG_PATH or "ffmpeg"
    args = [
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-headers",
        header_text,
        "-i",
        video["url"],
        "-headers",
        header_text,
        "-i",
        audio["url"],
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        "-c",
        "copy",
        "-f",
        "matroska",
        "pipe:1",
    ]
    proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def close_proc() -> None:
        if proc.poll() is None:
            proc.terminate()

    return StreamingResponse(
        proc.stdout,
        media_type="video/x-matroska",
        headers={"Cache-Control": "no-store"},
        background=BackgroundTask(close_proc),
    )


@app.get("/api/playlist.m3u")
def api_playlist_m3u(url: str, quality: str | None = None):
    try:
        entries = playlist_entries(url)
    except Exception as exc:
        logger.exception("Error al resolver playlist: %s", url)
        raise HTTPException(status_code=502, detail=f"No se pudo resolver playlist: {exc}") from exc
    lines = ["#EXTM3U"]
    for entry in entries:
        lines.append(f"#EXTINF:-1,{entry['title']}")
        stream_url = (
            f"http://{config.APP_HOST}:{config.APP_PORT}/api/stream"
            f"?url={quote(entry['url'], safe='')}&quality={quote(quality or config.PLAYBACK_QUALITY or 'best', safe='')}"
        )
        lines.append(stream_url)
    return Response("\n".join(lines) + "\n", media_type="audio/x-mpegurl")


@app.get("/api/radio")
def api_radio(url: str):
    try:
        items = fetch_radio_items(url)
        return {"items": items}
    except Exception as exc:
        logger.exception("Error al obtener radio mix")
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.get("/api/recommendations")
def api_recommendations():
    return list_recommendations()


@app.post("/api/recommendations/refresh")
def api_recommendations_refresh():
    try:
        return {"task": start_task("refresh-recommendations", lambda: refresh_section("recommendations"))}
    except Exception as exc:
        logger.exception("Error al obtener recomendaciones")
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.get("/api/music/recommendations")
def api_music_recom():
    return list_music_recommendations()


@app.post("/api/music/recommendations/refresh")
def api_music_recom_refresh():
    try:
        return {"task": start_task("refresh-music-recommendations", lambda: refresh_section("musicRecommendations"))}
    except Exception as exc:
        logger.exception("Error al obtener recomendaciones de YT Music")
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.get("/api/music/playlists")
def api_music_playlists():
    return list_music_playlists()


@app.post("/api/music/playlists/refresh")
def api_music_playlists_refresh():
    try:
        return {"task": start_task("refresh-music-playlists", lambda: refresh_section("musicPlaylists"))}
    except Exception as exc:
        logger.exception("Error al obtener playlists de YT Music")
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.get("/api/music/library")
def api_music_library():
    return list_music_library()


@app.post("/api/music/library/refresh")
def api_music_library_refresh():
    return {"task": start_task("refresh-music-library", refresh_music_library)}


@app.post("/api/music/login")
def api_music_login():
    return {"task": start_task("login-youtube", _run_youtube_login)}


@app.get("/api/sync/status")
def api_sync_status():
    return get_sync_status()


@app.post("/api/sync/refresh-all")
def api_sync_refresh_all():
    try:
        return {"task": start_task("refresh-all", refresh_all)}
    except Exception as exc:
        logger.exception("Error al sincronizar todo")
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.get("/api/tasks")
def api_tasks():
    return {"items": list_tasks()}


@app.get("/api/tasks/{task_id}")
def api_task(task_id: str):
    task = get_task(task_id)
    if not task:
        raise HTTPException(status_code=404, detail="Tarea no encontrada")
    return task


class PlaylistSongsRequest(BaseModel):
    url: str = Field(min_length=1)


class PlayerPlaylistRequest(BaseModel):
    url: str = Field(min_length=1)
    quality: str | None = None


@app.get("/api/player/info")
def api_player_info(url: str, quality: str | None = None):
    try:
        return player_info(url, quality)
    except Exception as exc:
        logger.exception("Error al resolver player info: %s", url)
        raise HTTPException(status_code=502, detail=str(exc)) from exc


@app.get("/api/player/manifest.mpd")
def api_player_manifest(url: str, quality: str | None = None):
    try:
        return Response(player_manifest(url, quality), media_type="application/dash+xml")
    except Exception as exc:
        logger.exception("Error al generar manifest: %s", url)
        raise HTTPException(status_code=502, detail=str(exc)) from exc


@app.get("/api/player/proxy")
def api_player_proxy(src: str, request: Request):
    parsed = urlparse(src)
    if parsed.scheme not in {"http", "https"}:
        raise HTTPException(status_code=400, detail="URL invalida")
    if not parsed.hostname or not parsed.hostname.endswith("googlevideo.com"):
        raise HTTPException(status_code=400, detail="Host de media no permitido")

    headers = {
        "User-Agent": request.headers.get("user-agent")
        or "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/143 Safari/537.36",
        "Referer": "https://www.youtube.com/",
        "Origin": "https://www.youtube.com",
    }
    if request.headers.get("range"):
        headers["Range"] = request.headers["range"]
    response = _HTTP_CLIENT.send(_HTTP_CLIENT.build_request("GET", src, headers=headers), stream=True)
    if response.status_code >= 400:
        response.close()
        raise HTTPException(status_code=502, detail=f"Media remota respondio {response.status_code}")
    response_headers = {"Accept-Ranges": "bytes", "Cache-Control": "no-store"}
    for key in ("content-length", "content-range", "content-type"):
        if response.headers.get(key):
            response_headers[key.title()] = response.headers[key]

    def close_proxy() -> None:
        response.close()

    return StreamingResponse(
        response.iter_bytes(1024 * 256),
        status_code=response.status_code,
        media_type=response.headers.get("content-type", "application/octet-stream"),
        headers=response_headers,
        background=BackgroundTask(close_proxy),
    )


@app.get("/api/player/audio")
def api_player_audio(url: str):
    try:
        return audio_info(url)
    except Exception as exc:
        logger.exception("Error al resolver audio: %s", url)
        raise HTTPException(status_code=502, detail=str(exc)) from exc


@app.post("/api/player/playlist")
def api_player_playlist(payload: PlayerPlaylistRequest):
    try:
        return resolve_playlist(payload.url, payload.quality)
    except Exception as exc:
        logger.exception("Error al resolver cola: %s", payload.url)
        raise HTTPException(status_code=502, detail=str(exc)) from exc


@app.post("/api/music/playlist-songs")
def api_music_playlist_songs(payload: PlaylistSongsRequest):
    try:
        return {"items": fetch_playlist_songs(payload.url)}
    except Exception as exc:
        logger.exception("Error al obtener canciones de playlist: %s", payload.url)
        raise HTTPException(status_code=500, detail=str(exc)) from exc


@app.post("/api/play")
def api_play(payload: PlayRequest):
    return play(payload.target, payload.player, payload.title, payload.thumbnail, payload.channel)
