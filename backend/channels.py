from __future__ import annotations

import concurrent.futures
import time
import uuid
from typing import Any

import yt_dlp

from .state import read_state, update_state
from .youtube import _entry_to_item


def list_channels() -> list[dict[str, Any]]:
    return read_state()["channels"]


def list_feed() -> list[dict[str, Any]]:
    return read_state()["feed"]


def add_channel(url: str, name: str | None = None) -> dict[str, Any]:
    normalized = _videos_url(url)
    existing = next((c for c in read_state()["channels"] if c["url"] == normalized), None)
    if existing:
        raise ValueError(f"Canal ya agregado: {existing['name']}")
    channel = {
        "id": str(uuid.uuid4()),
        "name": name or url,
        "url": normalized,
        "thumbnail": "",
        "lastSync": "",
        "error": "",
    }

    def mutate(data):
        data["channels"].insert(0, channel)

    update_state(mutate)
    return refresh_channel(channel["id"])


def refresh_channel(channel_id: str) -> dict[str, Any]:
    state = read_state()
    channel = next((c for c in state["channels"] if c["id"] == channel_id), None)
    if not channel:
        raise ValueError("Canal no encontrado")
    try:
        info = _extract_channel(channel["url"])
        entries = info.get("entries") or []
        channel_name = info.get("title") or channel["name"]
        feed_items = []
        for entry in entries:
            if not entry:
                continue
            item = _entry_to_item(entry)
            if not item.get("url"):
                continue
            item.update(
                {
                    "feedId": f"{channel_id}:{item.get('id') or item.get('url')}",
                    "sourceChannelId": channel_id,
                    "sourceChannelName": channel_name,
                    "seenAt": time.strftime("%Y-%m-%d %H:%M:%S"),
                }
            )
            feed_items.append(item)
        now = time.strftime("%Y-%m-%d %H:%M:%S")

        def mutate(data):
            for saved in data["channels"]:
                if saved["id"] == channel_id:
                    saved.update({"name": channel_name, "lastSync": now, "error": ""})
                    break
            existing = {item.get("feedId"): item for item in data["feed"]}
            for item in feed_items:
                existing[item["feedId"]] = item
            data["feed"] = list(existing.values())[:200]

        update_state(mutate)
    except Exception as exc:
        def mark_error(data):
            for saved in data["channels"]:
                if saved["id"] == channel_id:
                    saved["error"] = str(exc)
                    break

        update_state(mark_error)
        raise
    return next(c for c in read_state()["channels"] if c["id"] == channel_id)


def refresh_all_channels() -> list[dict[str, Any]]:
    channels = list_channels()
    if not channels:
        return []
    with concurrent.futures.ThreadPoolExecutor(max_workers=4, thread_name_prefix="playzi-ch") as executor:
        futures = {executor.submit(refresh_channel, c["id"]): c["id"] for c in channels}
        refreshed = []
        for future in concurrent.futures.as_completed(futures):
            channel_id = futures[future]
            try:
                refreshed.append(future.result())
            except Exception:
                refreshed.append(next(c for c in read_state()["channels"] if c["id"] == channel_id))
    return refreshed


def delete_channel(channel_id: str) -> None:
    def mutate(data):
        data["channels"] = [c for c in data["channels"] if c["id"] != channel_id]
        data["feed"] = [f for f in data["feed"] if f.get("sourceChannelId") != channel_id]

    update_state(mutate)


def _extract_channel(url: str) -> dict[str, Any]:
    opts = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": "in_playlist",
        "playlistend": 24,
        "ignoreerrors": True,
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        return ydl.extract_info(url, download=False) or {}


def _videos_url(url: str) -> str:
    clean = url.strip()
    if not clean.startswith("http"):
        clean = "https://www.youtube.com/@" + clean.lstrip("@")
    if "youtube.com" in clean and not clean.rstrip("/").endswith(("/videos", "/streams", "/shorts")):
        clean = clean.rstrip("/") + "/videos"
    return clean
