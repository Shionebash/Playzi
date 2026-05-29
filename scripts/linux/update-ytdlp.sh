#!/usr/bin/env bash
set -e

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VENV_PYTHON="$ROOT/.venv/bin/python"

if [ ! -f "$VENV_PYTHON" ]; then
    echo "Error: No existe .venv. Ejecuta scripts/linux/setup.sh primero." >&2
    exit 1
fi

"$VENV_PYTHON" -m pip install --upgrade yt-dlp
"$VENV_PYTHON" -m yt_dlp --version
