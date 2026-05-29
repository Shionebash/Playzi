$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$VenvPython = Join-Path $Root ".venv\Scripts\python.exe"
if (!(Test-Path $VenvPython)) {
    Write-Host "No existe .venv. Ejecutando setup..."
    & (Join-Path $Root "scripts\windows\setup.ps1")
}

$EnvPath = Join-Path $Root ".env"
if (Test-Path $EnvPath) {
    Get-Content $EnvPath | ForEach-Object {
        if ($_ -match "^\s*([^#][^=]+)=(.*)$") {
            [Environment]::SetEnvironmentVariable($matches[1].Trim(), $matches[2].Trim(), "Process")
        }
    }
}

$hostName = if ($env:APP_HOST) { $env:APP_HOST } else { "127.0.0.1" }
$port = if ($env:APP_PORT) { $env:APP_PORT } else { "8765" }
$existing = Get-NetTCPConnection -LocalAddress $hostName -LocalPort ([int]$port) -State Listen -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "Playzi ya esta corriendo en http://$hostName`:$port"
    Write-Host "Si quieres reiniciarlo, ejecuta: scripts\windows\stop.ps1"
    exit 0
}

Write-Host "Abriendo Playzi en http://$hostName`:$port"
Set-Location $Root
& $VenvPython -m uvicorn backend.main:app --host $hostName --port $port
