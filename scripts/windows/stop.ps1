$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$EnvPath = Join-Path $Root ".env"
if (Test-Path $EnvPath) {
    Get-Content $EnvPath | ForEach-Object {
        if ($_ -match "^\s*([^#][^=]+)=(.*)$") {
            [Environment]::SetEnvironmentVariable($matches[1].Trim(), $matches[2].Trim(), "Process")
        }
    }
}

$hostName = if ($env:APP_HOST) { $env:APP_HOST } else { "127.0.0.1" }
$port = if ($env:APP_PORT) { [int]$env:APP_PORT } else { 8765 }
$connections = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Where-Object {
    $_.LocalAddress -eq $hostName -or $_.LocalAddress -eq "0.0.0.0" -or $_.LocalAddress -eq "::"
}

if (!$connections) {
    Write-Host "No hay ningun servidor Playzi escuchando en http://$hostName`:$port"
    exit 0
}

$seen = @{}
do {
    $connections = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Where-Object {
        $_.LocalAddress -eq $hostName -or $_.LocalAddress -eq "0.0.0.0" -or $_.LocalAddress -eq "::"
    }
    $pids = $connections | Select-Object -ExpandProperty OwningProcess -Unique
    foreach ($pidValue in $pids) {
        $proc = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
        if (!$proc) { continue }
        if ($seen[$pidValue]) { continue }
        $seen[$pidValue] = $true
        if ($proc) {
            Write-Host "Cerrando proceso $pidValue ($($proc.ProcessName))..."
            Stop-Process -Id $pidValue -Force
        }
    }
    Start-Sleep -Milliseconds 300
    $live = @($pids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
} while ($live.Count -gt 0)

Write-Host "Playzi detenido."
