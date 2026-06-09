from __future__ import annotations

import re
import hashlib
import time
from pathlib import Path
from typing import Any

import httpx
import yt_dlp
from ytmusicapi import YTMusic
from ytmusicapi.auth.types import AuthType

from . import config
from .state import read_state, update_state
from .youtube import _cache_get, _cache_set, _entry_to_item
from ._cookies import fallback_cookie_opts, get_cookie_opts, handle_cookie_error, pop_cookie_warning, secure_cache_after_write

_MUSIC_SEARCH_CACHE: dict[str, tuple[float, list[dict[str, Any]]]] = {}

_BASE_OPTS: dict[str, Any] = {
    "quiet": True,
    "skip_download": True,
    "extract_flat": "in_playlist",
    "ignoreerrors": True,
    "extractor_args": {"youtubetab": {"skip": ["authcheck"]}},
}

_MUSIC_HEADERS = {
    "Origin": "https://music.youtube.com",
    "Referer": "https://music.youtube.com/",
    "User-Agent": (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
        "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36"
    ),
}
# Fallback values loaded from config (sourced from music.youtube.com page — public web client keys)
_DEFAULT_MUSIC_API_KEY = config.YTMUSIC_API_KEY
_DEFAULT_MUSIC_CLIENT_VERSION = config.YTMUSIC_CLIENT_VERSION


def _opts(extra: dict | None = None) -> dict[str, Any]:
    merged = {**_BASE_OPTS, **(extra or {})}
    merged.update(get_cookie_opts())
    return merged


def _flatten(entries: list) -> list[dict]:
    result = []
    for e in entries:
        if not e:
            continue
        sub = e.get("entries")
        if sub:
            result.extend(x for x in sub if x)
        else:
            result.append(e)
    return result


def _to_items(entries: list) -> list[dict[str, Any]]:
    seen: set[str] = set()
    items = []
    for e in entries:
        if not e:
            continue
        item = _entry_to_item(e)
        if not item.get("url") or item["url"] in seen:
            continue
        seen.add(item["url"])
        items.append(item)
    return items


def _cookie_header_from_file(path: str) -> str:
    cookie_path = Path(path)
    pairs = []
    for line in cookie_path.read_text(encoding="utf-8-sig", errors="ignore").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        parts = stripped.split("\t")
        if len(parts) >= 7:
            domain = parts[0].lstrip(".").lower()
            if domain not in {"youtube.com", "music.youtube.com"} and not domain.endswith(".youtube.com"):
                continue
            pairs.append(f"{parts[5]}={parts[6]}")
    return "; ".join(pairs)


def _cookie_dict_from_file(path: str) -> dict[str, str]:
    cookies = {}
    cookie_path = Path(path)
    for line in cookie_path.read_text(encoding="utf-8-sig", errors="ignore").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        parts = stripped.split("\t")
        if len(parts) >= 7:
            cookies[parts[5]] = parts[6]
    return cookies


def _music_cookie_header() -> str:
    opts = get_cookie_opts()
    cookiefile = opts.get("cookiefile")
    if not cookiefile:
        raise ValueError(
            "YouTube Music necesita un cookies.txt exportado o un cache privado ya generado. "
            f"Guarda un archivo Netscape cookies.txt en {config.DATA_DIR / 'cookies'}."
        )
    return _cookie_header_from_file(str(cookiefile))


def _music_cookiefile() -> str | None:
    opts = get_cookie_opts()
    cookiefile = opts.get("cookiefile")
    if not cookiefile or not Path(str(cookiefile)).exists():
        return None
    return str(cookiefile)


def _ytmusic_auth_headers() -> dict[str, str] | None:
    cookiefile = _music_cookiefile()
    if not cookiefile:
        return None
    cookies = _cookie_dict_from_file(cookiefile)
    sapisid = cookies.get("SAPISID") or cookies.get("__Secure-3PAPISID") or cookies.get("__Secure-1PAPISID")
    if not sapisid:
        return None
    origin = "https://music.youtube.com"
    timestamp = str(int(time.time()))
    digest = hashlib.sha1(f"{timestamp} {sapisid} {origin}".encode("utf-8")).hexdigest()
    # Send all cookies from the file (like yt-dlp); filtering risks dropping
    # session-validation cookies (LOGIN_INFO, SIDCC, etc.)
    cookie_header = "; ".join(f"{name}={value}" for name, value in cookies.items())
    return {
        "Cookie": cookie_header,
        "Authorization": f"SAPISIDHASH {timestamp}_{digest}",
        "x-origin": origin,
        "X-Goog-AuthUser": "0",
    }


def _ytmusic_client(prefer_auth: bool = True) -> YTMusic:
    try:
        auth = _ytmusic_auth_headers() if prefer_auth else None
    except Exception:
        auth = None
    return YTMusic(auth, language="es", location="EC")


def _session_auth_state(client: YTMusic) -> tuple[str, str]:
    """Return ("ok", accountName) if authenticated server-side, else ("expired", "")."""
    if client.auth_type != AuthType.BROWSER:
        return "expired", ""
    try:
        info = client.get_account_info()
        name = (info or {}).get("accountName") or ""
        return ("ok", name) if name else ("expired", "")
    except Exception:
        return "expired", ""


def _set_music_auth_state(state: str, account_name: str = "") -> None:
    now = time.strftime("%Y-%m-%d %H:%M:%S")

    def mutate(data):
        data["musicAuthState"] = state
        data["musicAuthCheckedAt"] = now
        data["musicAccountName"] = account_name if state == "ok" else ""

    update_state(mutate)


def _get_validated_client() -> tuple[YTMusic, str, str]:
    """Build a YTMusic client and verify the session is really logged in.

    If anonymous and using the managed 'playzi' profile, force a cookie re-export
    (recovers when the cache was merely stale). Returns (client, "ok"|"expired", accountName).
    """
    client = _ytmusic_client(prefer_auth=True)
    state, account_name = _session_auth_state(client)
    if state != "ok" and config.COOKIES_BROWSER.lower() == "playzi":
        try:
            get_cookie_opts(force=True)
            client = _ytmusic_client(prefer_auth=True)
            state, account_name = _session_auth_state(client)
        except Exception:
            pass
    _set_music_auth_state(state, account_name)
    return client, state, account_name


def _items_from_ytmusic_home(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    items = []
    seen = set()
    for row in rows:
        for content in row.get("contents", []):
            video_id = content.get("videoId")
            playlist_id = content.get("playlistId")
            browse_id = content.get("browseId")
            uid = video_id or playlist_id or browse_id
            if not uid or uid in seen:
                continue
            seen.add(uid)
            thumbnails = content.get("thumbnails") or []
            thumb = thumbnails[-1].get("url") if thumbnails else ""
            if video_id:
                url = f"https://music.youtube.com/watch?v={video_id}"
            elif playlist_id:
                url = f"https://music.youtube.com/playlist?list={playlist_id}"
            else:
                url = f"https://music.youtube.com/browse/{browse_id}"
            items.append({
                "id": uid,
                "url": url,
                "title": content.get("title") or "YouTube Music",
                "channel": content.get("artists", [{}])[0].get("name") if content.get("artists") else content.get("description") or row.get("title") or "YouTube Music",
                "channelUrl": None,
                "duration": None,
                "durationText": content.get("duration") or "--:--",
                "thumbnail": thumb,
                "viewCount": None,
                "live": False,
            })
            if len(items) >= 40:
                return items
    return items


def _playlists_from_ytmusicapi(playlists: list[dict[str, Any]]) -> list[dict[str, Any]]:
    items = []
    for playlist in playlists:
        playlist_id = playlist.get("playlistId") or playlist.get("browseId")
        if not playlist_id:
            continue
        thumbnails = playlist.get("thumbnails") or []
        items.append({
            "id": playlist_id,
            "title": playlist.get("title") or "Playlist",
            "url": f"https://music.youtube.com/playlist?list={playlist_id}",
            "count": playlist.get("count") or playlist.get("trackCount") or 0,
            "thumbnail": thumbnails[-1].get("url") if thumbnails else "",
        })
    return items


def music_playlist_queue(playlist_id: str, limit: int = 100) -> dict[str, Any]:
    """Resolve a YouTube Music playlist/mix into playable queue items."""
    ytmusic = _ytmusic_client(prefer_auth=True)
    errors: list[str] = []
    title = "Playlist"
    tracks: list[dict[str, Any]] = []

    try:
        data = ytmusic.get_playlist(playlist_id, limit=limit)
        title = data.get("title") or title
        tracks = data.get("tracks") or []
    except Exception as exc:
        errors.append(str(exc))

    if not tracks:
        try:
            data = ytmusic.get_watch_playlist(playlistId=playlist_id, limit=limit)
            title = data.get("title") or title
            tracks = data.get("tracks") or []
        except Exception as exc:
            errors.append(str(exc))

    items = [_track_to_queue_item(track) for track in tracks]
    items = [item for item in items if item.get("url")]
    if items:
        return {"items": items, "title": title, "source": "ytmusic", "requiresLogin": False, "error": ""}

    special = playlist_id in {"LM", "SE"} or playlist_id.startswith("RDTMAK")
    message = (
        "No se pudo leer esta playlist de YouTube Music. "
        "Inicia sesion en el perfil Playzi con scripts\\login-youtube.ps1 y vuelve a intentar."
        if special
        else "No se encontraron canciones reproducibles en esta playlist de YouTube Music."
    )
    return {
        "items": [],
        "title": title,
        "source": "ytmusic",
        "requiresLogin": special,
        "error": message,
        "debug": errors[-2:],
    }


def _track_to_queue_item(track: dict[str, Any]) -> dict[str, Any]:
    video_id = track.get("videoId")
    thumbs = track.get("thumbnails") or track.get("thumbnail") or []
    artists = track.get("artists") or []
    artist_text = ", ".join(a.get("name", "") for a in artists if a.get("name"))
    if not artist_text and track.get("artist"):
        artist_text = str(track["artist"])
    return {
        "id": video_id or "",
        "title": track.get("title") or video_id or "Cancion",
        "url": f"https://music.youtube.com/watch?v={video_id}" if video_id else "",
        "thumbnail": thumbs[-1].get("url") if thumbs else "",
        "channel": artist_text or "YouTube Music",
        "durationText": track.get("duration") or track.get("length") or "--:--",
    }


def _music_context(client_version: str = "1.20240520.01.00") -> dict[str, Any]:
    return {
        "client": {
            "clientName": "WEB_REMIX",
            "clientVersion": client_version,
            "hl": "es",
            "gl": "EC",
        }
    }


def _extract_initial_music_config(html: str) -> tuple[str, str]:
    key_match = re.search(r'"INNERTUBE_API_KEY"\s*:\s*"([^"]+)"', html)
    version_match = re.search(r'"INNERTUBE_CLIENT_VERSION"\s*:\s*"([^"]+)"', html)
    if not key_match:
        return _DEFAULT_MUSIC_API_KEY, _DEFAULT_MUSIC_CLIENT_VERSION
    return key_match.group(1), version_match.group(1) if version_match else _DEFAULT_MUSIC_CLIENT_VERSION


def _music_browse(browse_id: str) -> dict[str, Any]:
    cookie_header = _music_cookie_header()
    headers = {**_MUSIC_HEADERS, "Cookie": cookie_header}
    with httpx.Client(timeout=20, follow_redirects=True, headers=headers) as client:
        html = client.get("https://music.youtube.com/").text
        api_key, client_version = _extract_initial_music_config(html)
        response = client.post(
            f"https://music.youtube.com/youtubei/v1/browse?key={api_key}",
            json={"context": _music_context(client_version), "browseId": browse_id},
        )
        response.raise_for_status()
        return response.json()


def _text_from_runs(node: Any) -> str:
    if not isinstance(node, dict):
        return ""
    if isinstance(node.get("simpleText"), str):
        return node["simpleText"]
    runs = node.get("runs") or []
    return "".join(run.get("text", "") for run in runs if isinstance(run, dict)).strip()


def _walk_renderers(node: Any, renderer_name: str) -> list[dict[str, Any]]:
    found = []
    if isinstance(node, dict):
        renderer = node.get(renderer_name)
        if isinstance(renderer, dict):
            found.append(renderer)
        for value in node.values():
            found.extend(_walk_renderers(value, renderer_name))
    elif isinstance(node, list):
        for value in node:
            found.extend(_walk_renderers(value, renderer_name))
    return found


def _thumb_from_renderer(renderer: dict[str, Any]) -> str:
    thumbnails = []
    for item in _walk_values(renderer, "thumbnails"):
        if isinstance(item, list):
            thumbnails.extend(x for x in item if isinstance(x, dict))
    if not thumbnails:
        return ""
    return thumbnails[-1].get("url", "")


def _walk_values(node: Any, key: str) -> list[Any]:
    found = []
    if isinstance(node, dict):
        if key in node:
            found.append(node[key])
        for value in node.values():
            found.extend(_walk_values(value, key))
    elif isinstance(node, list):
        for value in node:
            found.extend(_walk_values(value, key))
    return found


def _music_items_from_browse(data: dict[str, Any]) -> list[dict[str, Any]]:
    items = []
    seen = set()
    renderers = _walk_renderers(data, "musicResponsiveListItemRenderer")
    renderers.extend(_walk_renderers(data, "musicTwoRowItemRenderer"))
    for renderer in renderers:
        video_id = _first_value(renderer, "videoId")
        if not video_id or video_id in seen:
            continue
        title = _first_flex_text(renderer) or _text_from_runs(renderer.get("title")) or "Sin titulo"
        subtitle = _second_flex_text(renderer) or "YouTube Music"
        seen.add(video_id)
        items.append({
            "id": video_id,
            "url": f"https://music.youtube.com/watch?v={video_id}",
            "title": title,
            "channel": subtitle,
            "channelUrl": None,
            "duration": None,
            "durationText": "--:--",
            "thumbnail": _thumb_from_renderer(renderer) or f"https://i.ytimg.com/vi/{video_id}/hqdefault.jpg",
            "viewCount": None,
            "live": False,
        })
        if len(items) >= 40:
            break
    return items


def _music_playlists_from_browse(data: dict[str, Any]) -> list[dict[str, Any]]:
    playlists = []
    seen = set()
    for renderer in _walk_renderers(data, "musicTwoRowItemRenderer"):
        playlist_id = _first_value(renderer, "playlistId")
        if not playlist_id or playlist_id in seen:
            continue
        seen.add(playlist_id)
        playlists.append({
            "id": playlist_id,
            "title": _text_from_runs(renderer.get("title")) or "Playlist",
            "url": f"https://music.youtube.com/playlist?list={playlist_id}",
            "count": 0,
            "thumbnail": _thumb_from_renderer(renderer),
        })
    return playlists


def _first_value(node: Any, key: str) -> str:
    for value in _walk_values(node, key):
        if isinstance(value, str) and value:
            return value
    return ""


def _first_flex_text(renderer: dict[str, Any]) -> str:
    columns = renderer.get("flexColumns") or []
    if not columns:
        return ""
    return _text_from_runs(columns[0].get("musicResponsiveListItemFlexColumnRenderer", {}).get("text", {}))


def _second_flex_text(renderer: dict[str, Any]) -> str:
    columns = renderer.get("flexColumns") or []
    if len(columns) < 2:
        return ""
    return _text_from_runs(columns[1].get("musicResponsiveListItemFlexColumnRenderer", {}).get("text", {}))


# ── Recommendations ──────────────────────────────────────────────────────────

_SESSION_EXPIRED_WARNING = (
    "Tu sesión de YouTube expiró. Vuelve a iniciar sesión para ver tu contenido personalizado."
)


def fetch_music_recommendations() -> dict[str, Any]:
    client, auth_state, _account = _get_validated_client()
    if auth_state != "ok":
        # Don't overwrite saved content with anonymous/generic data.
        # Advance the timestamp so autosync respects the interval (avoids
        # re-launching the headless browser every cycle).
        now = time.strftime("%Y-%m-%d %H:%M:%S")
        update_state(lambda d: d.update({"musicRecomLastSync": now}))
        data = read_state()
        return {
            "items": data.get("musicRecommendations", []),
            "lastSync": now,
            "authState": auth_state,
            "warning": _SESSION_EXPIRED_WARNING,
        }
    used_stale_cookies = False
    try:
        items = _items_from_ytmusic_home(client.get_home(limit=8))
    except Exception as exc:
        fallback = fallback_cookie_opts(exc)
        if not fallback:
            handle_cookie_error(exc)
            raise
        used_stale_cookies = bool(fallback.pop("_used_stale_cookies", False))
        try:
            items = _items_from_ytmusic_home(_ytmusic_client(prefer_auth=True).get_home(limit=8))
        except Exception as retry_exc:
            handle_cookie_error(retry_exc)
            raise
    secure_cache_after_write()
    now = time.strftime("%Y-%m-%d %H:%M:%S")

    def mutate(data):
        data["musicRecommendations"] = items
        data["musicRecomLastSync"] = now

    update_state(mutate)
    return {
        "items": items,
        "lastSync": now,
        "authState": "ok",
        "usedStaleCookies": used_stale_cookies,
        "warning": pop_cookie_warning(),
    }


def list_music_recommendations() -> dict[str, Any]:
    data = read_state()
    return {"items": data.get("musicRecommendations", []), "lastSync": data.get("musicRecomLastSync", "")}


# ── Playlists ─────────────────────────────────────────────────────────────────

def fetch_music_playlists() -> dict[str, Any]:
    client, auth_state, _account = _get_validated_client()
    if auth_state != "ok":
        now = time.strftime("%Y-%m-%d %H:%M:%S")
        update_state(lambda d: d.update({"musicPlaylistsLastSync": now}))
        data = read_state()
        return {
            "items": data.get("musicPlaylists", []),
            "lastSync": now,
            "authState": auth_state,
            "warning": _SESSION_EXPIRED_WARNING,
        }
    used_stale_cookies = False
    try:
        playlists = _playlists_from_ytmusicapi(client.get_library_playlists(limit=50))
    except Exception as exc:
        fallback = fallback_cookie_opts(exc)
        if not fallback:
            handle_cookie_error(exc)
            raise
        used_stale_cookies = bool(fallback.pop("_used_stale_cookies", False))
        try:
            playlists = _playlists_from_ytmusicapi(_ytmusic_client(prefer_auth=True).get_library_playlists(limit=50))
        except Exception as retry_exc:
            handle_cookie_error(retry_exc)
            raise
    secure_cache_after_write()

    now = time.strftime("%Y-%m-%d %H:%M:%S")

    def mutate(data):
        data["musicPlaylists"] = playlists
        data["musicPlaylistsLastSync"] = now

    update_state(mutate)
    return {
        "items": playlists,
        "lastSync": now,
        "authState": "ok",
        "usedStaleCookies": used_stale_cookies,
        "warning": pop_cookie_warning(),
    }


def list_music_playlists() -> dict[str, Any]:
    data = read_state()
    return {"items": data.get("musicPlaylists", []), "lastSync": data.get("musicPlaylistsLastSync", "")}


def list_music_library() -> dict[str, Any]:
    data = read_state().get("musicLibrary", {})
    return {
        "songs": data.get("songs", []),
        "liked": data.get("liked", []),
        "albums": data.get("albums", []),
        "artists": data.get("artists", []),
        "playlists": data.get("playlists", []),
        "lastSync": data.get("lastSync", ""),
        "error": data.get("error", ""),
        "history": read_state().get("musicHistory", []),
    }


def refresh_music_library() -> dict[str, Any]:
    client, auth_state, _account = _get_validated_client()
    if auth_state != "ok":
        data = read_state().get("musicLibrary", {})
        return {**data, "authState": auth_state, "error": _SESSION_EXPIRED_WARNING}
    now = time.strftime("%Y-%m-%d %H:%M:%S")
    result: dict[str, Any] = {
        "songs": [],
        "liked": [],
        "albums": [],
        "artists": [],
        "playlists": [],
        "history": [],
        "lastSync": now,
        "error": "",
    }
    errors = []
    for key, loader in {
        "songs": lambda: [_track_to_queue_item(x) for x in client.get_library_songs(limit=100)],
        "liked": lambda: [_track_to_queue_item(x) for x in (client.get_liked_songs(limit=100).get("tracks") or [])],
        "albums": lambda: _music_library_groups(client.get_library_albums(limit=50), "album"),
        "artists": lambda: _music_library_groups(client.get_library_artists(limit=50), "artist"),
        "playlists": lambda: _playlists_from_ytmusicapi(client.get_library_playlists(limit=50)),
        "history": lambda: [_track_to_queue_item(x) for x in client.get_history()],
    }.items():
        try:
            result[key] = loader()
        except Exception as exc:
            errors.append(f"{key}: {exc}")
    result["error"] = " | ".join(errors[:3])

    def mutate(data: dict[str, Any]) -> None:
        data["musicLibrary"] = {k: v for k, v in result.items() if k != "history"}
        data["musicHistory"] = result["history"]

    update_state(mutate)
    return result


def search_music(query: str, limit: int = 20, search_filter: str | None = None) -> list[dict[str, Any]]:
    query = query.strip()
    if not query:
        return []
    cache_key = f"{query}|{search_filter or ''}|{limit}"
    cached = _cache_get(_MUSIC_SEARCH_CACHE, cache_key)
    if cached is not None:
        return cached
    client = _ytmusic_client(prefer_auth=True)
    results = client.search(query, filter=search_filter, limit=limit)
    items = []
    for item in results:
        video_id = item.get("videoId")
        browse_id = item.get("browseId")
        result_type = _music_result_type(item)
        playlist_id = item.get("playlistId") or (browse_id if result_type == "playlist" else None)
        thumbnails = item.get("thumbnails") or []
        if video_id:
            items.append({
                "id": video_id,
                "url": f"https://music.youtube.com/watch?v={video_id}",
                "title": item.get("title") or video_id,
                "channel": _artists_text(item) or item.get("artist") or item.get("category") or "YouTube Music",
                "durationText": item.get("duration") or "--:--",
                "thumbnail": thumbnails[-1].get("url") if thumbnails else "",
                "source": "music",
                "resultType": "track" if result_type in {"song", "track"} else "video",
            })
        elif playlist_id:
            items.append({
                "id": playlist_id,
                "url": f"https://music.youtube.com/playlist?list={playlist_id}",
                "title": item.get("title") or "Playlist",
                "channel": item.get("author") or item.get("category") or "YouTube Music",
                "durationText": "Playlist",
                "thumbnail": thumbnails[-1].get("url") if thumbnails else "",
                "source": "music",
                "resultType": "playlist",
            })
        elif browse_id and result_type == "artist":
            items.append({
                "id": browse_id,
                "url": f"https://music.youtube.com/browse/{browse_id}",
                "title": item.get("title") or item.get("artist") or "Artista",
                "channel": item.get("subscribers") or item.get("category") or "Artista",
                "durationText": "Artista",
                "thumbnail": thumbnails[-1].get("url") if thumbnails else "",
                "source": "music",
                "resultType": "artist",
            })
        elif browse_id and result_type == "album":
            items.append({
                "id": browse_id,
                "url": f"https://music.youtube.com/browse/{browse_id}",
                "title": item.get("title") or "Album",
                "channel": _artists_text(item) or item.get("artist") or item.get("category") or "YouTube Music",
                "durationText": "Album",
                "thumbnail": thumbnails[-1].get("url") if thumbnails else "",
                "source": "music",
                "resultType": "album",
            })
    _cache_set(_MUSIC_SEARCH_CACHE, cache_key, items)
    return items


def fetch_playlist_songs(playlist_url: str) -> list[dict[str, Any]]:
    try:
        with yt_dlp.YoutubeDL(_opts({"playlistend": 150})) as ydl:
            info = ydl.extract_info(playlist_url, download=False) or {}
    except Exception as exc:
        fallback = fallback_cookie_opts(exc)
        if not fallback:
            handle_cookie_error(exc)
            raise
        fallback.pop("_used_stale_cookies", None)
        try:
            with yt_dlp.YoutubeDL({**_BASE_OPTS, "playlistend": 150, **fallback}) as ydl:
                info = ydl.extract_info(playlist_url, download=False) or {}
        except Exception as retry_exc:
            handle_cookie_error(retry_exc)
            raise
    secure_cache_after_write()
    return _to_items(info.get("entries") or [])


def _music_library_groups(items: list[dict[str, Any]], kind: str) -> list[dict[str, Any]]:
    result = []
    for item in items:
        thumbnails = item.get("thumbnails") or []
        browse_id = item.get("browseId") or item.get("playlistId") or item.get("channelId") or ""
        result.append({
            "id": browse_id or item.get("title", ""),
            "title": item.get("title") or kind.title(),
            "channel": item.get("artist") or item.get("artists", [{}])[0].get("name", "") if item.get("artists") else "",
            "thumbnail": thumbnails[-1].get("url") if thumbnails else "",
            "source": "music",
            "kind": kind,
            "url": f"https://music.youtube.com/browse/{browse_id}" if browse_id else "",
        })
    return result


def _artists_text(item: dict[str, Any]) -> str:
    artists = item.get("artists") or []
    return ", ".join(a.get("name", "") for a in artists if a.get("name"))


def _music_result_type(item: dict[str, Any]) -> str:
    raw = str(item.get("resultType") or item.get("category") or "").strip().lower()
    normalized = {
        "songs": "song",
        "canciones": "song",
        "song": "song",
        "tracks": "track",
        "track": "track",
        "videos": "video",
        "video": "video",
        "artists": "artist",
        "artistas": "artist",
        "artist": "artist",
        "albums": "album",
        "albumes": "album",
        "álbumes": "album",
        "album": "album",
        "playlists": "playlist",
        "listas": "playlist",
        "playlist": "playlist",
    }.get(raw)
    if normalized:
        return normalized
    if item.get("videoId"):
        return "song" if item.get("duration") else "video"
    if item.get("playlistId"):
        return "playlist"
    if item.get("browseId"):
        return "artist"
    return ""
