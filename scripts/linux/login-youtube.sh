#!/usr/bin/env bash
set -e

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VENV_PYTHON="$ROOT/.venv/bin/python"

if [ ! -f "$VENV_PYTHON" ]; then
    echo "No existe .venv. Ejecutando setup..."
    bash "$(dirname "$0")/setup.sh"
fi

cd "$ROOT"
"$VENV_PYTHON" -m playwright install chromium
"$VENV_PYTHON" -c "from backend.managed_browser import open_login_browser; open_login_browser()"
