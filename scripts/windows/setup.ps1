$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Venv = Join-Path $Root ".venv"
$Python = "python"

function Find-Exe($Name) {
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $Name
}

if (!(Test-Path $Venv)) {
    & $Python -m venv $Venv
}

$VenvPython = Join-Path $Venv "Scripts\python.exe"
& $VenvPython -m pip install --upgrade pip
& $VenvPython -m pip install -r (Join-Path $Root "requirements.txt")
& $VenvPython -m playwright install chromium 2>&1 | Out-Null

$EnvPath = Join-Path $Root ".env"
if (!(Test-Path $EnvPath)) {
    $mpv = Find-Exe "mpv"
    $vlcCmd = Get-Command "vlc" -ErrorAction SilentlyContinue
    $vlc = if ($vlcCmd) { $vlcCmd.Source } elseif (Test-Path "C:\Program Files\VideoLAN\VLC\vlc.exe") { "C:\Program Files\VideoLAN\VLC\vlc.exe" } else { "vlc" }
    $ffmpeg = Find-Exe "ffmpeg"
    @"
APP_HOST=127.0.0.1
APP_PORT=8765
MEDIA_ROOT=$Root\media
DATA_DIR=$Root\data
STATE_FILE=$Root\data\state.json
DEFAULT_PLAYER=mpv
MPV_PATH=$mpv
VLC_PATH=$vlc
FFMPEG_PATH=$ffmpeg
AUDIO_FORMAT=opus
YTDLP_SEARCH_LIMIT=12
"@ | Set-Content -LiteralPath $EnvPath -Encoding UTF8
}

New-Item -ItemType Directory -Force -Path (Join-Path $Root "media"), (Join-Path $Root "data") | Out-Null
Write-Host "Playzi listo."
Write-Host "Arranca con: scripts\windows\run.ps1"
