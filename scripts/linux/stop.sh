#!/usr/bin/env bash
set -e

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

ENV_PATH="$ROOT/.env"
if [ -f "$ENV_PATH" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_PATH"
    set +a
fi

HOST="${APP_HOST:-127.0.0.1}"
PORT="${APP_PORT:-8765}"

PIDS="$(lsof -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null || true)"

if [ -z "$PIDS" ]; then
    echo "No hay ningun servidor Playzi escuchando en http://$HOST:$PORT"
    exit 0
fi

for pid in $PIDS; do
    echo "Cerrando proceso $pid..."
    kill "$pid" 2>/dev/null || true
done

sleep 0.5

REMAINING="$(lsof -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null || true)"
if [ -n "$REMAINING" ]; then
    for pid in $REMAINING; do
        kill -9 "$pid" 2>/dev/null || true
    done
fi

echo "Playzi detenido."
