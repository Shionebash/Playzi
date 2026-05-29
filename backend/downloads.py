from __future__ import annotations

import threading
import time
import uuid
from pathlib import Path
from typing import Any

import yt_dlp

from . import config
from .history import list_history as normalized_history
from .state import read_state, read_state_key, update_state
from .youtube import metadata_for_url
from .ytdlp_opts import javascript_runtime_opts


QUALITY_HEIGHT = {
    "4K": 2160,
    "2160p": 2160,
    "1440p": 1440,
    "1080p": 1080,
    "720p": 720,
    "480p": 480,
    "360p": 360,
}

_CANCEL_EVENTS: dict[str, threading.Event] = {}


class _DownloadCancelled(Exception):
    pass


def list_downloads() -> list[dict[str, Any]]:
    return read_state_key("downloads")


def list_download_status() -> list[dict[str, Any]]:
    return [
        {
            "id": job.get("id"),
            "status": job.get("status", ""),
            "progress": job.get("progress", 0),
            "speed": job.get("speed", ""),
            "eta": job.get("eta", ""),
            "error": job.get("error", ""),
            "updatedAt": job.get("updatedAt", job.get("createdAt", "")),
        }
        for job in read_state_key("downloads")
    ]


def list_library() -> list[dict[str, Any]]:
    return sorted(read_state_key("library"), key=lambda item: item.get("createdAt", ""), reverse=True)


def list_history() -> list[dict[str, Any]]:
    return normalized_history()


def start_download(payload: dict[str, Any]) -> dict[str, Any]:
    url = payload["url"]
    kind = payload.get("kind", "video")
    active = [
        j for j in read_state_key("downloads")
        if j.get("url") == url and j.get("status") not in ("done", "error", "cancelled")
    ]
    if active:
        return active[0]
    quality = payload.get("quality") or "1080p"
    collection = payload.get("collection") or ""
    title = payload.get("title") or url
    channel = payload.get("channel") or "YouTube"
    thumbnail = payload.get("thumbnail")
    item_id = str(uuid.uuid4())
    now = time.strftime("%Y-%m-%d %H:%M:%S")
    job = {
        "id": item_id,
        "url": url,
        "title": title,
        "channel": channel,
        "thumbnail": thumbnail,
        "kind": kind,
        "format": "audio" if kind == "audio" else "video",
        "quality": None if kind == "audio" else quality,
        "collection": collection,
        "progress": 0,
        "speed": "",
        "eta": "",
        "status": "queued",
        "error": "",
        "createdAt": now,
        "updatedAt": now,
    }

    def add(data):
        data["downloads"].insert(0, job)
        data["history"].insert(0, {**job, "action": "download", "source": _source_for_url(url), "date": now})

    update_state(add)
    cancel_event = threading.Event()
    _CANCEL_EVENTS[item_id] = cancel_event
    thread = threading.Thread(target=_run_download, args=(item_id, cancel_event), daemon=True)
    thread.start()
    return job


def cancel_download(job_id: str) -> None:
    event = _CANCEL_EVENTS.get(job_id)
    if event:
        event.set()
    _set_job(job_id, {"status": "cancelled", "error": "Cancelado por el usuario", "speed": "", "eta": ""})


def delete_download(job_id: str) -> None:
    event = _CANCEL_EVENTS.get(job_id)
    if event:
        event.set()

    def mutate(data):
        data["downloads"] = [j for j in data["downloads"] if j["id"] != job_id]

    update_state(mutate)
    _CANCEL_EVENTS.pop(job_id, None)


def _set_job(job_id: str, patch: dict[str, Any], *, volatile: bool = False) -> None:
    def mutate(data):
        for job in data["downloads"]:
            if job["id"] == job_id:
                patch.setdefault("updatedAt", time.strftime("%Y-%m-%d %H:%M:%S"))
                job.update(patch)
                break

    update_state(mutate, volatile=volatile)


def _finish_job(job_id: str, library_item: dict[str, Any]) -> None:
    def mutate(data):
        for job in data["downloads"]:
            if job["id"] == job_id:
                job.update({
                    "status": "done",
                    "progress": 100,
                    "speed": "",
                    "eta": "",
                    "path": library_item["path"],
                    "updatedAt": time.strftime("%Y-%m-%d %H:%M:%S"),
                })
                break
        data["library"].insert(0, library_item)

    update_state(mutate)


def _run_download(job_id: str, cancel_event: threading.Event) -> None:
    job = next((j for j in read_state()["downloads"] if j["id"] == job_id), None)
    if not job:
        _CANCEL_EVENTS.pop(job_id, None)
        return
    _set_job(job_id, {"status": "starting"})
    try:
        meta = metadata_for_url(job["url"])
        title = meta.get("title") or job["title"]
        channel = meta.get("channel") or job["channel"]
        out_dir = _safe_dir(config.MEDIA_ROOT / _safe_name(job.get("collection") or channel))
        out_tmpl = str(out_dir / "%(title).180B [%(id)s].%(ext)s")
        opts = _ydl_options(job, out_tmpl, cancel_event)
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info(job["url"], download=True)
            final_path = Path(ydl.prepare_filename(info))
        final_path = _normalize_final_path(final_path, job)
        library_item = {
            "id": job_id,
            "url": job["url"],
            "title": title,
            "channel": channel,
            "thumbnail": meta.get("thumbnail") or job.get("thumbnail"),
            "kind": job["kind"],
            "format": "MP3" if job["kind"] == "audio" and config.AUDIO_FORMAT == "mp3" else ("OPUS" if job["kind"] == "audio" else "MP4"),
            "quality": job.get("quality"),
            "collection": job.get("collection") or "",
            "durationText": meta.get("durationText"),
            "path": str(final_path),
            "mediaUrl": _media_url(final_path),
            "size": _size_text(final_path),
            "createdAt": time.strftime("%Y-%m-%d %H:%M:%S"),
        }
        _finish_job(job_id, library_item)
    except _DownloadCancelled:
        _set_job(job_id, {"status": "cancelled", "error": "Cancelado por el usuario", "speed": "", "eta": ""})
    except Exception as exc:  # yt-dlp errors should stay visible in the UI.
        _set_job(job_id, {"status": "error", "error": str(exc), "speed": "", "eta": ""})
    finally:
        _CANCEL_EVENTS.pop(job_id, None)


def _ydl_options(job: dict[str, Any], out_tmpl: str, cancel_event: threading.Event) -> dict[str, Any]:
    common = {
        "outtmpl": out_tmpl,
        "quiet": True,
        "noplaylist": True,
        "writethumbnail": True,
        "writeinfojson": True,
        "ffmpeg_location": config.FFMPEG_PATH,
        "progress_hooks": [_progress_hook(job["id"], cancel_event)],
        **javascript_runtime_opts(),
    }
    if job["kind"] == "audio":
        common.update(
            {
                "format": "bestaudio/best",
                "postprocessors": [
                    {
                        "key": "FFmpegExtractAudio",
                        "preferredcodec": config.AUDIO_FORMAT,
                        "preferredquality": "0",
                    }
                ],
            }
        )
        return common
    height = QUALITY_HEIGHT.get(job.get("quality") or "1080p", 1080)
    common.update(
        {
            "format": f"bv*[height<={height}][ext=mp4]+ba[ext=m4a]/bv*[height<={height}]+ba/b[height<={height}]/best",
            "merge_output_format": "mp4",
        }
    )
    return common


def _progress_hook(job_id: str, cancel_event: threading.Event):
    def hook(d: dict[str, Any]) -> None:
        if cancel_event.is_set():
            raise _DownloadCancelled()
        status = d.get("status")
        if status == "downloading":
            total = d.get("total_bytes") or d.get("total_bytes_estimate") or 0
            downloaded = d.get("downloaded_bytes") or 0
            progress = round((downloaded / total) * 100, 1) if total else 0
            _set_job(
                job_id,
                {
                    "status": "downloading",
                    "progress": progress,
                    "speed": _speed_text(d.get("speed")),
                    "eta": _eta_text(d.get("eta")),
                },
                volatile=True,
            )
        elif status == "finished":
            _set_job(job_id, {"status": "processing", "progress": 99, "speed": "", "eta": "procesando"})

    return hook


def _safe_dir(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    return path


def _safe_name(value: str) -> str:
    cleaned = "".join("_" if ch in '<>:"/\\|?*' else ch for ch in (value or "YouTube"))
    cleaned = cleaned.strip().strip(".")
    return cleaned or "YouTube"


def _normalize_final_path(path: Path, job: dict[str, Any]) -> Path:
    if job["kind"] == "audio":
        candidate = path.with_suffix("." + config.AUDIO_FORMAT)
        if candidate.exists():
            return candidate
    if path.exists():
        return path
    candidates = sorted(path.parent.glob(path.stem + ".*"), key=lambda p: p.stat().st_mtime, reverse=True)
    return candidates[0] if candidates else path


def _media_url(path: Path) -> str:
    try:
        rel = path.resolve().relative_to(config.MEDIA_ROOT)
    except ValueError:
        return ""
    return "/media/" + "/".join(rel.parts)


def _size_text(path: Path) -> str:
    if not path.exists():
        return "--"
    size = path.stat().st_size
    for unit in ("B", "KB", "MB", "GB"):
        if size < 1024 or unit == "GB":
            return f"{size:.1f} {unit}" if unit != "B" else f"{size} B"
        size /= 1024
    return "--"


def _speed_text(speed: Any) -> str:
    if not speed:
        return ""
    value = float(speed)
    for unit in ("B/s", "KB/s", "MB/s", "GB/s"):
        if value < 1024 or unit == "GB/s":
            return f"{value:.1f} {unit}"
        value /= 1024
    return ""


def _eta_text(eta: Any) -> str:
    if eta is None:
        return ""
    eta = int(eta)
    m, s = divmod(eta, 60)
    h, m = divmod(m, 60)
    return f"{h}h {m}m" if h else f"{m}m {s}s"


def _source_for_url(url: str) -> str:
    value = str(url or "").lower()
    if "music.youtube.com" in value:
        return "music"
    if "youtube.com" in value or "youtu.be" in value:
        return "youtube"
    return "local"
