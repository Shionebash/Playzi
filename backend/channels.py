from __future__ import annotations

import concurrent.futures
import hashlib
import json
import logging
import re
import time
import urllib.request
import uuid
from typing import Any

import yt_dlp

from .state import read_state, update_state
from .youtube import _entry_to_item
from .ytmusic import _cookie_dict_from_file, _music_cookiefile

logger = logging.getLogger(__name__)
_YT_WEB_CLIENT_VERSION = "2.20260401.00.00"
_YT_USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36"
_UC_RE = re.compile(r"/channel/(UC[A-Za-z0-9_-]{10,})")


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
                # refresh_channel ya guardo el error en el canal; aqui solo log
                logger.warning("Fallo el refresh del canal %s", channel_id, exc_info=True)
                refreshed.append(next(c for c in read_state()["channels"] if c["id"] == channel_id))
    return refreshed


def delete_channel(channel_id: str) -> None:
    def mutate(data):
        data["channels"] = [c for c in data["channels"] if c["id"] != channel_id]
        data["feed"] = [f for f in data["feed"] if f.get("sourceChannelId") != channel_id]

    update_state(mutate)


def preview_channel(url: str) -> dict[str, Any]:
    """Fetch a channel's recent videos by URL without persisting it."""
    normalized = _videos_url(url)
    followed = next((c for c in read_state()["channels"] if c["url"] == normalized), None)
    info = _extract_channel(normalized)
    channel_name = info.get("title") or (followed["name"] if followed else url)
    items = []
    for entry in info.get("entries") or []:
        if not entry:
            continue
        item = _entry_to_item(entry)
        if not item.get("url"):
            continue
        if followed:
            item["sourceChannelId"] = followed["id"]
            item["sourceChannelName"] = channel_name
        items.append(item)
    return {
        "name": channel_name,
        "url": normalized,
        "channelId": followed["id"] if followed else None,
        "alreadyFollowed": bool(followed),
        "items": items,
    }


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


def _yt_auth_headers() -> dict[str, str] | None:
    cookiefile = _music_cookiefile()
    if not cookiefile:
        return None
    cookies = _cookie_dict_from_file(cookiefile)
    if not cookies:
        return None
    origin = "https://www.youtube.com"
    headers = {
        "Content-Type": "application/json",
        "User-Agent": _YT_USER_AGENT,
        "Origin": origin,
        "Referer": f"{origin}/",
        "Cookie": "; ".join(f"{k}={v}" for k, v in cookies.items()),
        "X-Goog-AuthUser": str(read_state().get("musicAuthUser", 0)),
    }
    sapisid = cookies.get("SAPISID") or cookies.get("__Secure-3PAPISID") or cookies.get("__Secure-1PAPISID")
    if not sapisid:
        return None
    ts = str(int(time.time()))
    digest = hashlib.sha1(f"{ts} {sapisid} {origin}".encode("utf-8")).hexdigest()
    headers["Authorization"] = f"SAPISIDHASH {ts}_{digest}"
    headers["X-Origin"] = origin
    return headers


def _post_yt_innertube(endpoint: str, payload: dict[str, Any], headers: dict[str, str]) -> dict[str, Any]:
    body = dict(payload)
    body["context"] = {
        "client": {
            "clientName": "WEB",
            "clientVersion": _YT_WEB_CLIENT_VERSION,
            "hl": "es",
            "gl": "EC",
        }
    }
    req = urllib.request.Request(
        f"https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false",
        data=json.dumps(body).encode("utf-8"),
        headers=headers,
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.loads(resp.read().decode("utf-8", errors="ignore"))


def _walk_renderers(node: Any, key: str) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    if isinstance(node, dict):
        if key in node and isinstance(node[key], dict):
            found.append(node[key])
        for v in node.values():
            found.extend(_walk_renderers(v, key))
    elif isinstance(node, list):
        for item in node:
            found.extend(_walk_renderers(item, key))
    return found


def _text_from_node(node: Any) -> str:
    if isinstance(node, str):
        return node.strip()
    if isinstance(node, dict):
        if isinstance(node.get("simpleText"), str):
            return node["simpleText"].strip()
        runs = node.get("runs")
        if isinstance(runs, list):
            return "".join(str(r.get("text") or "") for r in runs if isinstance(r, dict)).strip()
    return ""


def _best_thumb(node: Any) -> str:
    if not isinstance(node, dict):
        return ""
    thumbs = node.get("thumbnails") or []
    if not isinstance(thumbs, list) or not thumbs:
        return ""
    url = str(thumbs[-1].get("url") or "").strip()
    if url.startswith("//"):
        url = "https:" + url
    return url


def _extract_continuation(data: dict[str, Any]) -> str | None:
    for node in _walk_renderers(data, "continuationCommand"):
        tok = node.get("token")
        if isinstance(tok, str) and tok:
            return tok
    for node in _walk_renderers(data, "nextContinuationData"):
        tok = node.get("continuation")
        if isinstance(tok, str) and tok:
            return tok
    return None


def _parse_subscribed_channels(data: dict[str, Any]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for rname in ("channelRenderer", "gridChannelRenderer"):
        for node in _walk_renderers(data, rname):
            nav = node.get("navigationEndpoint") or {}
            browse = nav.get("browseEndpoint") or {}
            cid = str(node.get("channelId") or browse.get("browseId") or "").strip()
            if not cid or cid.lower() in seen:
                continue
            seen.add(cid.lower())
            canonical = str(browse.get("canonicalBaseUrl") or "")
            handle = canonical.lstrip("/") if canonical.startswith("/@") else ""
            out.append({
                "channelId": cid,
                "name": _text_from_node(node.get("title")) or cid,
                "url": f"https://www.youtube.com/channel/{cid}/videos",
                "handle": handle,
                "thumbnail": _best_thumb(node.get("thumbnail")),
                "subscriberCount": _text_from_node(node.get("subscriberCountText")) or None,
            })
    return out


def sync_youtube_subscriptions() -> dict[str, Any]:
    headers = _yt_auth_headers()
    if not headers:
        return {
            "authenticated": False,
            "syncedCount": 0,
            "addedCount": 0,
            "updatedCount": 0,
            "removedCount": 0,
            "totalChannels": len(list_channels()),
            "lastSync": read_state().get("channelsLastSync", ""),
        }

    subs: list[dict[str, Any]] = []
    seen: set[str] = set()
    seen_tokens: set[str] = set()
    pagination_complete = False

    try:
        data = _post_yt_innertube("browse", {"browseId": "FEchannels"}, headers)
        for ch in _parse_subscribed_channels(data):
            key = ch["channelId"].lower()
            if key not in seen:
                seen.add(key)
                subs.append(ch)
        for _ in range(9):
            tok = _extract_continuation(data)
            if not tok or tok in seen_tokens:
                pagination_complete = True
                break
            seen_tokens.add(tok)
            try:
                data = _post_yt_innertube("browse", {"continuation": tok}, headers)
                before = len(subs)
                for ch in _parse_subscribed_channels(data):
                    key = ch["channelId"].lower()
                    if key not in seen:
                        seen.add(key)
                        subs.append(ch)
                if len(subs) == before:
                    pagination_complete = True
                    break
            except Exception:
                pagination_complete = False
                break
        if not pagination_complete and _extract_continuation(data) is None:
            pagination_complete = True
    except Exception:
        logger.warning("Failed fetching FEchannels subscriptions", exc_info=True)

    if not subs:
        try:
            guide = _post_yt_innertube("guide", {}, headers)
            for sec in _walk_renderers(guide, "guideSubscriptionsSectionRenderer"):
                for entry in _walk_renderers(sec, "guideEntryRenderer"):
                    browse = (entry.get("navigationEndpoint") or {}).get("browseEndpoint") or {}
                    cid = str(browse.get("browseId") or "").strip()
                    if not cid.startswith("UC") or cid.lower() in seen:
                        continue
                    seen.add(cid.lower())
                    canonical = str(browse.get("canonicalBaseUrl") or "")
                    subs.append({
                        "channelId": cid,
                        "name": _text_from_node(entry.get("formattedTitle")) or cid,
                        "url": f"https://www.youtube.com/channel/{cid}/videos",
                        "handle": canonical.lstrip("/") if canonical.startswith("/@") else "",
                        "thumbnail": _best_thumb(entry.get("thumbnail")),
                        "subscriberCount": None,
                    })
            pagination_complete = False
        except Exception:
            logger.warning("Failed fetching guide subscriptions fallback", exc_info=True)

    now = time.strftime("%Y-%m-%d %H:%M:%S")
    stats = {"added": 0, "updated": 0, "removed": 0, "total": 0}
    active_ucs = {s["channelId"].lower() for s in subs}

    def mutate(data: dict[str, Any]) -> None:
        channels = data.setdefault("channels", [])
        for sub in subs:
            sub_uc = sub["channelId"].lower()
            sub_handle = sub["handle"].lstrip("@").lower()
            match = None
            for ch in channels:
                ch_url = str(ch.get("url") or "")
                uc_m = _UC_RE.search(ch_url)
                ch_uc = str(ch.get("channelId") or (uc_m.group(1) if uc_m else "")).lower()
                ch_handle = str(ch.get("handle") or "").lstrip("@").lower()
                if ch_uc == sub_uc or ch_url.lower() == sub["url"].lower() or (sub_handle and ch_handle == sub_handle):
                    match = ch
                    break
            if match is not None:
                if sub["name"]:
                    match["name"] = sub["name"]
                if sub["thumbnail"]:
                    match["thumbnail"] = sub["thumbnail"]
                match["channelId"] = sub["channelId"]
                if sub["handle"]:
                    match["handle"] = sub["handle"]
                if sub.get("subscriberCount"):
                    match["subscriberCount"] = sub["subscriberCount"]
                match["syncedFromYoutube"] = True
                stats["updated"] += 1
            else:
                entry = {
                    "id": str(uuid.uuid4()),
                    "name": sub["name"],
                    "url": sub["url"],
                    "channelId": sub["channelId"],
                    "handle": sub["handle"],
                    "thumbnail": sub["thumbnail"],
                    "syncedFromYoutube": True,
                    "lastSync": "",
                    "error": "",
                }
                if sub.get("subscriberCount"):
                    entry["subscriberCount"] = sub["subscriberCount"]
                channels.append(entry)
                stats["added"] += 1

        if pagination_complete and subs:
            before = len(channels)
            kept = []
            removed_ids: set[str] = set()
            for ch in channels:
                if not ch.get("syncedFromYoutube"):
                    kept.append(ch)
                    continue
                ch_url = str(ch.get("url") or "")
                uc_m = _UC_RE.search(ch_url)
                ch_uc = str(ch.get("channelId") or (uc_m.group(1) if uc_m else "")).lower()
                if ch_uc in active_ucs:
                    kept.append(ch)
                else:
                    if ch.get("id"):
                        removed_ids.add(str(ch["id"]))
            data["channels"] = kept
            stats["removed"] = before - len(kept)
            if removed_ids:
                data["feed"] = [f for f in data.get("feed", []) if f.get("sourceChannelId") not in removed_ids]

        data["channelsLastSync"] = now
        stats["total"] = len(data["channels"])

    update_state(mutate)
    return {
        "authenticated": True,
        "syncedCount": len(subs),
        "addedCount": stats["added"],
        "updatedCount": stats["updated"],
        "removedCount": stats["removed"],
        "totalChannels": stats["total"],
        "lastSync": now,
    }


def refresh_manual_channels() -> list[dict[str, Any]]:
    manual = [c for c in list_channels() if not c.get("syncedFromYoutube")][:30]
    if not manual:
        return []
    with concurrent.futures.ThreadPoolExecutor(max_workers=4, thread_name_prefix="playzi-ch") as executor:
        futures = {executor.submit(refresh_channel, c["id"]): c["id"] for c in manual}
        refreshed = []
        for future in concurrent.futures.as_completed(futures):
            channel_id = futures[future]
            try:
                refreshed.append(future.result())
            except Exception:
                logger.warning("Fallo el refresh del canal %s", channel_id, exc_info=True)
                refreshed.append(next(c for c in read_state()["channels"] if c["id"] == channel_id))
    return refreshed
