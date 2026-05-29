#!/usr/bin/env bash
set -e

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VENV="$ROOT/.venv"

if [ ! -d "$VENV" ]; then
    python3 -m venv "$VENV"
fi

"$VENV/bin/python" -m pip install --upgrade pip
"$VENV/bin/python" -m pip install -r "$ROOT/requirements.txt"

ENV_PATH="$ROOT/.env"
if [ ! -f "$ENV_PATH" ]; then
    MPV="$(command -v mpv 2>/dev/null || echo mpv)"
    VLC="$(command -v vlc 2>/dev/null || echo vlc)"
    FFMPEG="$(command -v ffmpeg 2>/dev/null || echo ffmpeg)"
    cat > "$ENV_PATH" << EOF
APP_HOST=127.0.0.1
APP_PORT=8765
MEDIA_ROOT=$ROOT/media
DATA_DIR=$ROOT/data
STATE_FILE=$ROOT/data/state.json
DEFAULT_PLAYER=mpv
MPV_PATH=$MPV
VLC_PATH=$VLC
FFMPEG_PATH=$FFMPEG
AUDIO_FORMAT=opus
YTDLP_SEARCH_LIMIT=12
EOF
fi

mkdir -p "$ROOT/media" "$ROOT/data"
echo "Playzi listo."
echo "Arranca con: bash scripts/linux/run.sh"
