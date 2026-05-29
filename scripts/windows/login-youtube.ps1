$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$VenvPython = Join-Path $Root ".venv\Scripts\python.exe"
if (!(Test-Path $VenvPython)) {
    Write-Host "No existe .venv. Ejecutando setup..."
    & (Join-Path $Root "scripts\windows\setup.ps1")
}

Set-Location $Root
& $VenvPython -m playwright install chromium
& $VenvPython -c "from backend.managed_browser import open_login_browser; open_login_browser()"
