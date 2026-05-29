$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$VenvPython = Join-Path $Root ".venv\Scripts\python.exe"
if (!(Test-Path $VenvPython)) {
    throw "No existe .venv. Ejecuta scripts\windows\setup.ps1 primero."
}

& $VenvPython -m pip install --upgrade yt-dlp
& $VenvPython -m yt_dlp --version
