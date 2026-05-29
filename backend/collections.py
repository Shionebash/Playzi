from __future__ import annotations

import time
import uuid
from typing import Any

from .state import read_state, update_state


def list_collections() -> list[dict[str, Any]]:
    data = read_state()
    manual = data["collections"]
    counts: dict[str, int] = {}
    for item in data["library"]:
        name = item.get("collection") or ""
        if name:
            counts[name] = counts.get(name, 0) + 1
    by_name = {c["name"]: {**c, "count": counts.get(c["name"], 0)} for c in manual}
    for name, count in counts.items():
        by_name.setdefault(name, {"id": name, "name": name, "createdAt": "", "count": count})
    return sorted(by_name.values(), key=lambda c: c.get("name", "").lower())


def delete_collection(collection_id: str) -> None:
    def mutate(data):
        data["collections"] = [c for c in data["collections"] if c["id"] != collection_id]

    update_state(mutate)


def create_collection(name: str) -> dict[str, Any]:
    clean = name.strip()
    if not clean:
        raise ValueError("Nombre de coleccion vacio")
    collection = {"id": str(uuid.uuid4()), "name": clean, "createdAt": time.strftime("%Y-%m-%d %H:%M:%S")}

    def mutate(data):
        if not any(c.get("name", "").lower() == clean.lower() for c in data["collections"]):
            data["collections"].append(collection)

    update_state(mutate)
    return collection
