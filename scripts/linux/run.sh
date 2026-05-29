#!/usr/bin/env bash
set -e

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VENV_PYTHON="$ROOT/.venv/bin/python"

if [ ! -f "$VENV_PYTHON" ]; then
    echo "No existe .venv. Ejecutando setup..."
    bash "$(dirname "$0")/setup.sh"
fi

ENV_PATH="$ROOT/.env"
if [ -f "$ENV_PATH" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_PATH"
    set +a
fi

HOST="${APP_HOST:-127.0.0.1}"
PORT="${APP_PORT:-8765}"

if ss -tlnp 2>/dev/null | grep -q ":$PORT " || \
   lsof -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null | grep -q .; then
    echo "Playzi ya esta corriendo en http://$HOST:$PORT"
    echo "Si quieres reiniciarlo, ejecuta: bash scripts/linux/stop.sh"
    exit 0
fi

echo "Abriendo Playzi en http://$HOST:$PORT"
cd "$ROOT"
"$VENV_PYTHON" -m uvicorn backend.main:app --host "$HOST" --port "$PORT"
