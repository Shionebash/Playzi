from __future__ import annotations

import shutil
import sys

from . import config


def javascript_runtime_opts() -> dict:
    # Explicit cachedir so the resolved nsig/sig player functions persist
    # across restarts (yt-dlp's default cache location is less predictable
    # on Windows).
    opts: dict = {"cachedir": str(config.DATA_DIR / "ytdlp_cache")}
    node_path = shutil.which("node")
    if not node_path and sys.platform == "win32":
        node_path = r"C:\Program Files\nodejs\node.exe"
    if not node_path:
        return opts
    opts.update({
        "js_runtimes": {"node": {"path": node_path}},
        "remote_components": {"ejs:github"},
    })
    return opts
