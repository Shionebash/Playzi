from __future__ import annotations

import time
import uuid
from urllib.parse import parse_qs, urlparse
from typing import Any

import yt_dlp

from .history import add_history_event
from .state import flush_state, read_state, update_state
from .youtube import _entry_to_item
from .ytdlp_opts import javascript_runtime_opts


def list_playlists() -> list[dict[str, Any]]:
    return [_normalize_playlist(pl) for pl in read_state().get("playlists", [])]


def create_playlist(name: str) -> dict[str, Any]:
    clean = name.strip()
    if not clean:
        raise ValueError("Nombre de playlist vacio")
    playlist: dict[str, Any] = {
        "id": str(uuid.uuid4()),
        "name": clean,
        "source": "playzi",
        "externalUrl": "",
        "externalId": "",
        "importMode": "local",
        "lastSync": "",
        "itemCount": 0,
        "thumbnail": "",
        "items": [],
        "createdAt": time.strftime("%Y-%m-%d %H:%M:%S"),
    }

    def mutate(data: dict[str, Any]) -> None:
        data.setdefault("playlists", []).append(playlist)

    update_state(mutate)
    flush_state()
    return playlist


def delete_playlist(playlist_id: str) -> None:
    def mutate(data: dict[str, Any]) -> None:
        data["playlists"] = [p for p in data.get("playlists", []) if p["id"] != playlist_id]

    update_state(mutate)
    flush_state()


def add_item(playlist_id: str, item: dict[str, Any]) -> dict[str, Any]:
    def mutate(data: dict[str, Any]) -> None:
        for pl in data.get("playlists", []):
            if pl["id"] == playlist_id:
                if not any(i.get("url") == item.get("url") for i in pl["items"]):
                    pl["items"].append(item)
                _refresh_playlist_meta(pl)

    update_state(mutate)
    flush_state()
    return item


def remove_item(playlist_id: str, item_url: str) -> None:
    def mutate(data: dict[str, Any]) -> None:
        for pl in data.get("playlists", []):
            if pl["id"] == playlist_id:
                pl["items"] = [i for i in pl["items"] if i.get("url") != item_url]
                _refresh_playlist_meta(pl)

    update_state(mutate)
    flush_state()


def import_playlist(url: str, mode: str = "copy") -> dict[str, Any]:
    if mode not in {"copy", "linked"}:
        raise ValueError("Modo de importacion invalido")
    resolved = _resolve_external_playlist(url)
    now = time.strftime("%Y-%m-%d %H:%M:%S")
    playlist = {
        "id": str(uuid.uuid4()),
        "name": resolved["title"],
        "source": resolved["source"],
        "externalUrl": url,
        "externalId": resolved.get("externalId", ""),
        "importMode": mode,
        "lastSync": now,
        "itemCount": len(resolved["items"]),
        "thumbnail": resolved.get("thumbnail", ""),
        "items": resolved["items"],
        "createdAt": now,
    }

    def mutate(data: dict[str, Any]) -> None:
        data.setdefault("playlists", []).append(playlist)
        data.setdefault("importedPlaylists", []).insert(0, {
            "id": playlist["id"],
            "name": playlist["name"],
            "source": playlist["source"],
            "externalUrl": playlist["externalUrl"],
            "importMode": playlist["importMode"],
            "lastSync": playlist["lastSync"],
            "itemCount": playlist["itemCount"],
            "thumbnail": playlist["thumbnail"],
        })

    update_state(mutate)
    flush_state()
    add_history_event({
        "action": "import",
        "source": playlist["source"],
        "url": url,
        "title": playlist["name"],
        "thumbnail": playlist["thumbnail"],
        "date": now,
    })
    return playlist


def sync_playlist(playlist_id: str) -> dict[str, Any]:
    current = next((pl for pl in read_state().get("playlists", []) if pl.get("id") == playlist_id), None)
    if not current:
        raise ValueError("Playlist no encontrada")
    if current.get("importMode") != "linked" or not current.get("externalUrl"):
        raise ValueError("Solo las playlists vinculadas se pueden sincronizar")

    resolved = _resolve_external_playlist(current["externalUrl"])
    now = time.strftime("%Y-%m-%d %H:%M:%S")
    synced: dict[str, Any] = {}

    def mutate(data: dict[str, Any]) -> None:
        nonlocal synced
        for pl in data.get("playlists", []):
            if pl.get("id") == playlist_id:
                existing = {item.get("url"): item for item in pl.get("items", []) if item.get("url")}
                for item in resolved["items"]:
                    if item.get("url"):
                        existing[item["url"]] = item
                pl.update({
                    "name": resolved["title"] or pl.get("name"),
                    "source": resolved["source"],
                    "externalId": resolved.get("externalId", pl.get("externalId", "")),
                    "lastSync": now,
                    "thumbnail": resolved.get("thumbnail") or pl.get("thumbnail", ""),
                    "items": list(existing.values()),
                })
                _refresh_playlist_meta(pl)
                synced = _normalize_playlist(pl)
                break
        data["importedPlaylists"] = [
            {**item, "lastSync": now, "itemCount": synced.get("itemCount", item.get("itemCount", 0))}
            if item.get("id") == playlist_id else item
            for item in data.get("importedPlaylists", [])
        ]

    update_state(mutate)
    flush_state()
    add_history_event({
        "action": "sync",
        "source": synced.get("source", "playlist"),
        "url": synced.get("externalUrl", ""),
        "title": synced.get("name", "Playlist sincronizada"),
        "thumbnail": synced.get("thumbnail", ""),
        "date": now,
    })
    return synced


def _normalize_playlist(pl: dict[str, Any]) -> dict[str, Any]:
    normalized = {
        **pl,
        "source": pl.get("source") or "playzi",
        "externalUrl": pl.get("externalUrl") or "",
        "externalId": pl.get("externalId") or "",
        "importMode": pl.get("importMode") or "local",
        "lastSync": pl.get("lastSync") or "",
        "thumbnail": pl.get("thumbnail") or _first_thumb(pl.get("items", [])),
        "items": pl.get("items") or [],
    }
    normalized["itemCount"] = len(normalized["items"])
    return normalized


def _refresh_playlist_meta(pl: dict[str, Any]) -> None:
    pl["itemCount"] = len(pl.get("items", []))
    if not pl.get("thumbnail"):
        pl["thumbnail"] = _first_thumb(pl.get("items", []))


def _first_thumb(items: list[dict[str, Any]]) -> str:
    return next((item.get("thumbnail", "") for item in items if item.get("thumbnail")), "")


def _resolve_external_playlist(url: str) -> dict[str, Any]:
    parsed = urlparse(url)
    query = parse_qs(parsed.query)
    source = "music" if parsed.hostname == "music.youtube.com" else "youtube"
    if source == "music" and query.get("list"):
        try:
            from .ytmusic import music_playlist_queue

            data = music_playlist_queue(query["list"][0], limit=150)
            items = data.get("items") or []
            if items:
                return {
                    "title": data.get("title") or "YouTube Music playlist",
                    "source": source,
                    "externalId": query["list"][0],
                    "thumbnail": _first_thumb(items),
                    "items": items,
                }
        except Exception:
            pass

    opts = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "playlistend": 150,
        "ignoreerrors": True,
        **javascript_runtime_opts(),
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(url, download=False) or {}
    entries = []
    for entry in info.get("entries") or []:
        if not entry:
            continue
        item = _entry_to_item(entry)
        if item.get("url"):
            item["source"] = source
            entries.append(item)
    return {
        "title": info.get("title") or "Playlist importada",
        "source": source,
        "externalId": query.get("list", [""])[0],
        "thumbnail": _first_thumb(entries),
        "items": entries,
    }


def rename_playlist(playlist_id: str, name: str) -> dict[str, Any]:
    clean = name.strip()
    if not clean:
        raise ValueError("Nombre de playlist vacio")
    result: dict[str, Any] = {}

    def mutate(data: dict[str, Any]) -> None:
        nonlocal result
        for pl in data.get("playlists", []):
            if pl["id"] == playlist_id:
                pl["name"] = clean
                result = _normalize_playlist(pl)
        for imp in data.get("importedPlaylists", []):
            if imp.get("id") == playlist_id:
                imp["name"] = clean

    update_state(mutate)
    flush_state()
    return result or {"ok": True}


def reorder_items(playlist_id: str, urls: list[str]) -> dict[str, Any]:
    def mutate(data: dict[str, Any]) -> None:
        pl = next((p for p in data.get("playlists", []) if p["id"] == playlist_id), None)
        if not pl:
            raise ValueError("Playlist no encontrada")
        by_url = {item["url"]: item for item in pl.get("items", []) if item.get("url")}
        url_set = set(urls)
        reordered = [by_url[u] for u in urls if u in by_url]
        leftover = [item for item in pl.get("items", []) if item.get("url") not in url_set]
        pl["items"] = reordered + leftover
        _refresh_playlist_meta(pl)

    update_state(mutate)
    flush_state()
    return {"ok": True}
