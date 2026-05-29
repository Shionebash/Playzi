from __future__ import annotations

import re
import time
import uuid
from typing import Any

from .state import read_state, update_state

_YT_VIDEO_ID_RE = re.compile(r"(?:v=|youtu\.be/|shorts/)([A-Za-z0-9_-]{11})")


def _derive_thumbnail(url: str) -> str:
    """Derive hqdefault thumbnail from a YouTube/YT Music URL when thumbnail is missing."""
    match = _YT_VIDEO_ID_RE.search(url or "")
    return f"https://i.ytimg.com/vi/{match.group(1)}/hqdefault.jpg" if match else ""


def normalize_history_item(item: dict[str, Any]) -> dict[str, Any]:
    action = item.get("action") or item.get("kind") or "evento"
    url = item.get("url") or item.get("target") or ""
    source = item.get("source") or _source_for_url(url)
    date = item.get("date") or item.get("createdAt") or item.get("updatedAt") or ""
    return {
        "id": item.get("id") or f"hist-{uuid.uuid4()}",
        "action": action,
        "source": source,
        "url": url if str(url).startswith("http") else item.get("url", ""),
        "target": item.get("target") or url,
        "title": item.get("title") or item.get("target") or item.get("url") or "Sin titulo",
        "thumbnail": item.get("thumbnail") or _derive_thumbnail(url),
        "channel": item.get("channel") or item.get("sourceChannelName") or "",
        "durationText": item.get("durationText") or "",
        "date": date,
        "player": item.get("player") or "",
        "kind": item.get("kind") or "",
    }


def list_history() -> list[dict[str, Any]]:
    return [normalize_history_item(item) for item in read_state().get("history", [])]


def add_history_event(item: dict[str, Any]) -> dict[str, Any]:
    normalized = normalize_history_item({
        **item,
        "id": item.get("id") or f"hist-{uuid.uuid4()}",
        "date": item.get("date") or time.strftime("%Y-%m-%d %H:%M:%S"),
    })

    def mutate(data: dict[str, Any]) -> None:
        data.setdefault("history", []).insert(0, normalized)

    update_state(mutate)
    return normalized


def delete_history_event(event_id: str) -> None:
    def mutate(data: dict[str, Any]) -> None:
        data["history"] = [item for item in data.get("history", []) if str(item.get("id")) != event_id]

    update_state(mutate)


def _source_for_url(url: str) -> str:
    value = str(url or "").lower()
    if "music.youtube.com" in value:
        return "music"
    if "youtube.com" in value or "youtu.be" in value:
        return "youtube"
    if value:
        return "local"
    return "playzi"
