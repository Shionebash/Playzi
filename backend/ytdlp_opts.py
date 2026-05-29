from __future__ import annotations

import shutil
import sys


def javascript_runtime_opts() -> dict:
    node_path = shutil.which("node")
    if not node_path and sys.platform == "win32":
        node_path = r"C:\Program Files\nodejs\node.exe"
    if not node_path:
        return {}
    return {
        "js_runtimes": {"node": {"path": node_path}},
        "remote_components": {"ejs:github"},
    }
