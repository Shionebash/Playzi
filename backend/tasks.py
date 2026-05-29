from __future__ import annotations

import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Callable


_EXECUTOR = ThreadPoolExecutor(max_workers=3, thread_name_prefix="playzi-worker")
_LOCK = threading.Lock()
_TASKS: dict[str, dict[str, Any]] = {}
_MAX_TASKS = 100


def start_task(label: str, runner: Callable[[], Any]) -> dict[str, Any]:
    task_id = str(uuid.uuid4())
    now = _now()
    task = {
        "id": task_id,
        "label": label,
        "status": "queued",
        "progress": 0,
        "error": "",
        "result": None,
        "createdAt": now,
        "updatedAt": now,
    }
    with _LOCK:
        _TASKS[task_id] = task
        _trim_locked()
    _EXECUTOR.submit(_run_task, task_id, runner)
    return _public_task(task)


def get_task(task_id: str) -> dict[str, Any] | None:
    with _LOCK:
        task = _TASKS.get(task_id)
        return _public_task(task) if task else None


def list_tasks() -> list[dict[str, Any]]:
    with _LOCK:
        return [_public_task(task) for task in sorted(_TASKS.values(), key=lambda t: t["createdAt"], reverse=True)]


def _run_task(task_id: str, runner: Callable[[], Any]) -> None:
    _patch_task(task_id, status="running", progress=5)
    try:
        result = runner()
    except Exception as exc:
        _patch_task(task_id, status="error", progress=100, error=str(exc))
        return
    _patch_task(task_id, status="done", progress=100, result=result)


def _patch_task(task_id: str, **patch: Any) -> None:
    with _LOCK:
        task = _TASKS.get(task_id)
        if not task:
            return
        task.update(patch)
        task["updatedAt"] = _now()


def _public_task(task: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": task["id"],
        "label": task.get("label", ""),
        "status": task.get("status", ""),
        "progress": task.get("progress", 0),
        "error": task.get("error", ""),
        "updatedAt": task.get("updatedAt", ""),
    }


def _trim_locked() -> None:
    if len(_TASKS) <= _MAX_TASKS:
        return
    ordered = sorted(_TASKS.values(), key=lambda t: t["createdAt"])
    for task in ordered[:len(_TASKS) - _MAX_TASKS]:
        _TASKS.pop(task["id"], None)


def _now() -> str:
    return time.strftime("%Y-%m-%d %H:%M:%S")
