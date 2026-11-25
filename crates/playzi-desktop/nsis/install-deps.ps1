param(
    [string]$InstallDir = "",
    [switch]$Silent,
    [switch]$InstallMpv,
    [switch]$InstallVlc
)

$ErrorActionPreference = "Continue"
$ProgressPreference = "SilentlyContinue"
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch {}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Split-Path -Parent $PSScriptRoot
}

$BinDir = Join-Path $InstallDir "bin"
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

$LogDir = Join-Path $env:LOCALAPPDATA "Playzi"
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$LogFile = Join-Path $LogDir "deps-install.log"

function Write-Log([string]$Message) {
    $ts = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss")
    Write-Output $Message
    try {
        Add-Content -LiteralPath $LogFile -Value "[$ts] $Message" -Encoding UTF8
    } catch {}
}

function Find-WingetPackageExe([string]$PackagePattern, [string]$ExeName) {
    if (-not $env:LOCALAPPDATA) { return $null }
    $pkgsDir = Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages"
    if (-not (Test-Path -LiteralPath $pkgsDir)) { return $null }
    try {
        $dirs = Get-ChildItem -LiteralPath $pkgsDir -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like $PackagePattern }
        foreach ($d in $dirs) {
            $direct = Join-Path $d.FullName $ExeName
            if (Test-Path -LiteralPath $direct) { return $direct }
            $found = Get-ChildItem -LiteralPath $d.FullName -Filter $ExeName -Recurse -File -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($found) { return $found.FullName }
        }
    } catch {}
    return $null
}

function Find-YtDlp {
    $candidates = @(
        (Join-Path $BinDir "yt-dlp.exe"),
        (Join-Path $InstallDir "yt-dlp.exe")
    )
    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Links\yt-dlp.exe")
        $candidates += (Join-Path $env:LOCALAPPDATA "Programs\yt-dlp\yt-dlp.exe")
    }
    if ($env:USERPROFILE) {
        $candidates += (Join-Path $env:USERPROFILE "scoop\shims\yt-dlp.exe")
    }
    $candidates += "C:\ProgramData\chocolatey\bin\yt-dlp.exe"

    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    $cmd = Get-Command "yt-dlp" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) { return $cmd.Source }
    return Find-WingetPackageExe "*yt-dlp*" "yt-dlp.exe"
}

function Find-Ffmpeg {
    $candidates = @(
        (Join-Path $BinDir "ffmpeg.exe"),
        (Join-Path $InstallDir "ffmpeg.exe")
    )
    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Links\ffmpeg.exe")
    }
    if ($env:USERPROFILE) {
        $candidates += (Join-Path $env:USERPROFILE "scoop\shims\ffmpeg.exe")
    }
    $candidates += "C:\ProgramData\chocolatey\bin\ffmpeg.exe"

    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    $cmd = Get-Command "ffmpeg" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) { return $cmd.Source }
    return Find-WingetPackageExe "*FFmpeg*" "ffmpeg.exe"
}

function Find-Mpv {
    $candidates = @(
        (Join-Path $BinDir "mpv.exe"),
        (Join-Path $InstallDir "mpv.exe"),
        "C:\Program Files\MPV Player\mpv.exe",
        "C:\Program Files (x86)\MPV Player\mpv.exe",
        "C:\Program Files\mpv\mpv.exe"
    )
    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA "Programs\mpv\mpv.exe")
        $candidates += (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Links\mpv.exe")
    }
    if ($env:USERPROFILE) {
        $candidates += (Join-Path $env:USERPROFILE "scoop\shims\mpv.exe")
    }

    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    $cmd = Get-Command "mpv" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) { return $cmd.Source }
    return Find-WingetPackageExe "*mpv*" "mpv.exe"
}

function Find-Vlc {
    $candidates = @(
        (Join-Path $BinDir "vlc.exe"),
        (Join-Path $InstallDir "vlc.exe"),
        "C:\Program Files\VideoLAN\VLC\vlc.exe",
        "C:\Program Files (x86)\VideoLAN\VLC\vlc.exe"
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    $cmd = Get-Command "vlc" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) { return $cmd.Source }
    return $null
}

function Get-WingetPath {
    $cmd = Get-Command "winget.exe" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) { return $cmd.Source }
    if ($env:LOCALAPPDATA) {
        $alias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\winget.exe"
        if (Test-Path -LiteralPath $alias) { return $alias }
    }
    return $null
}

function Invoke-WingetInstall([string]$PackageId, [string]$DisplayName) {
    $winget = Get-WingetPath
    if (-not $winget) {
        Write-Log "No se encontro winget para instalar $DisplayName ($PackageId)."
        return $false
    }
    Write-Log "Instalando $DisplayName mediante winget ($PackageId)..."
    $args = @(
        "install",
        "--id", $PackageId,
        "-e",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--silent"
    )
    $proc = Start-Process -FilePath $winget -ArgumentList $args -Wait -PassThru -WindowStyle Hidden
    if ($proc -and $proc.ExitCode -eq 0) {
        Write-Log "$DisplayName instalado correctamente."
        return $true
    }
    $code = if ($proc) { $proc.ExitCode } else { -1 }
    Write-Log "winget termino con codigo $code al instalar $DisplayName."
    return $false
}

function Install-YtDlp {
    $target = Join-Path $BinDir "yt-dlp.exe"
    $tmp = Join-Path $env:TEMP "playzi-yt-dlp.exe"
    $url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    Write-Log "Descargando yt-dlp desde GitHub..."
    try {
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing -TimeoutSec 120
        if ((Test-Path -LiteralPath $tmp) -and ((Get-Item -LiteralPath $tmp).Length -gt 1000000)) {
            Move-Item -LiteralPath $tmp -Destination $target -Force
            Write-Log "yt-dlp instalado en $target"
            return $true
        }
    } catch {
        Write-Log "Error descargando yt-dlp directamente: $($_.Exception.Message)"
    } finally {
        if (Test-Path -LiteralPath $tmp) {
            Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        }
    }

    return Invoke-WingetInstall -PackageId "yt-dlp.yt-dlp" -DisplayName "yt-dlp"
}

function Install-Ffmpeg {
    $zipPath = Join-Path $env:TEMP "playzi-ffmpeg-essentials.zip"
    $extractDir = Join-Path $env:TEMP ("playzi-ffmpeg-" + [guid]::NewGuid().ToString("N"))
    $url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
    Write-Log "Descargando ffmpeg (essentials)..."
    try {
        Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing -TimeoutSec 300
        if ((Test-Path -LiteralPath $zipPath) -and ((Get-Item -LiteralPath $zipPath).Length -gt 5000000)) {
            Write-Log "Extrayendo ffmpeg..."
            Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force
            $ffmpegExe = Get-ChildItem -LiteralPath $extractDir -Filter "ffmpeg.exe" -Recurse -File | Select-Object -First 1
            $ffprobeExe = Get-ChildItem -LiteralPath $extractDir -Filter "ffprobe.exe" -Recurse -File | Select-Object -First 1
            if ($ffmpegExe) {
                Copy-Item -LiteralPath $ffmpegExe.FullName -Destination (Join-Path $BinDir "ffmpeg.exe") -Force
                if ($ffprobeExe) {
                    Copy-Item -LiteralPath $ffprobeExe.FullName -Destination (Join-Path $BinDir "ffprobe.exe") -Force
                }
                Write-Log "ffmpeg instalado en $(Join-Path $BinDir 'ffmpeg.exe')"
                return $true
            }
        }
    } catch {
        Write-Log "Error descargando ffmpeg directamente: $($_.Exception.Message)"
    } finally {
        if (Test-Path -LiteralPath $zipPath) {
            Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $extractDir) {
            Remove-Item -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

    return Invoke-WingetInstall -PackageId "Gyan.FFmpeg.Essentials" -DisplayName "ffmpeg"
}

function Show-DepsDialog(
    [bool]$HasYtDlp,
    [bool]$HasFfmpeg,
    [bool]$HasMpv,
    [bool]$HasVlc,
    [bool]$HasWinget
) {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    [System.Windows.Forms.Application]::EnableVisualStyles()

    $form = New-Object System.Windows.Forms.Form
    $form.Text = "Playzi - Requisitos y reproductores"
    $form.Size = New-Object System.Drawing.Size(470, 355)
    $form.StartPosition = "CenterScreen"
    $form.FormBorderStyle = "FixedDialog"
    $form.MaximizeBox = $false
    $form.MinimizeBox = $false
    $form.TopMost = $true
    $form.Font = New-Object System.Drawing.Font("Segoe UI", 9.5)

    $lblIntro = New-Object System.Windows.Forms.Label
    $lblIntro.Text = "Playzi instalara automaticamente los requisitos necesarios si faltan. Tambien puedes elegir instalar reproductores externos opcionales."
    $lblIntro.Location = New-Object System.Drawing.Point(16, 14)
    $lblIntro.Size = New-Object System.Drawing.Size(425, 42)
    $form.Controls.Add($lblIntro)

    $grpReq = New-Object System.Windows.Forms.GroupBox
    $grpReq.Text = "Requisitos obligatorios"
    $grpReq.Location = New-Object System.Drawing.Point(16, 62)
    $grpReq.Size = New-Object System.Drawing.Size(422, 82)
    $form.Controls.Add($grpReq)

    $chkYt = New-Object System.Windows.Forms.CheckBox
    $chkYt.Text = if ($HasYtDlp) { "yt-dlp (ya instalado)" } else { "yt-dlp (se instalara automaticamente)" }
    $chkYt.Checked = $true
    $chkYt.Enabled = $false
    $chkYt.Location = New-Object System.Drawing.Point(14, 24)
    $chkYt.Size = New-Object System.Drawing.Size(390, 22)
    $grpReq.Controls.Add($chkYt)

    $chkFf = New-Object System.Windows.Forms.CheckBox
    $chkFf.Text = if ($HasFfmpeg) { "ffmpeg (ya instalado)" } else { "ffmpeg (se instalara automaticamente)" }
    $chkFf.Checked = $true
    $chkFf.Enabled = $false
    $chkFf.Location = New-Object System.Drawing.Point(14, 50)
    $chkFf.Size = New-Object System.Drawing.Size(390, 22)
    $grpReq.Controls.Add($chkFf)

    $grpOpt = New-Object System.Windows.Forms.GroupBox
    $grpOpt.Text = "Reproductores externos (opcionales)"
    $grpOpt.Location = New-Object System.Drawing.Point(16, 154)
    $grpOpt.Size = New-Object System.Drawing.Size(422, 102)
    $form.Controls.Add($grpOpt)

    $chkMpv = New-Object System.Windows.Forms.CheckBox
    $chkMpv.Text = if ($HasMpv) { "MPV (ya instalado)" } else { "Instalar MPV (reproductor externo ligero)" }
    $chkMpv.Checked = $HasMpv
    $chkMpv.Enabled = (-not $HasMpv) -and $HasWinget
    $chkMpv.Location = New-Object System.Drawing.Point(14, 24)
    $chkMpv.Size = New-Object System.Drawing.Size(390, 22)
    $grpOpt.Controls.Add($chkMpv)

    $chkVlc = New-Object System.Windows.Forms.CheckBox
    $chkVlc.Text = if ($HasVlc) { "VLC (ya instalado)" } else { "Instalar VLC (VideoLAN Media Player)" }
    $chkVlc.Checked = $HasVlc
    $chkVlc.Enabled = (-not $HasVlc) -and $HasWinget
    $chkVlc.Location = New-Object System.Drawing.Point(14, 50)
    $chkVlc.Size = New-Object System.Drawing.Size(390, 22)
    $grpOpt.Controls.Add($chkVlc)

    $lblNote = New-Object System.Windows.Forms.Label
    $lblNote.Text = if ($HasWinget) {
        "Puedes marcar ninguno, uno o ambos reproductores."
    } else {
        "winget no esta disponible en este equipo; instala MPV/VLC manualmente si los deseas."
    }
    $lblNote.ForeColor = [System.Drawing.SystemColors]::GrayText
    $lblNote.Location = New-Object System.Drawing.Point(14, 76)
    $lblNote.Size = New-Object System.Drawing.Size(395, 20)
    $grpOpt.Controls.Add($lblNote)

    $btnSkip = New-Object System.Windows.Forms.Button
    $btnSkip.Text = "Solo requisitos"
    $btnSkip.Location = New-Object System.Drawing.Point(188, 272)
    $btnSkip.Size = New-Object System.Drawing.Size(120, 32)
    $btnSkip.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
    $form.Controls.Add($btnSkip)

    $btnOk = New-Object System.Windows.Forms.Button
    $btnOk.Text = "Continuar"
    $btnOk.Location = New-Object System.Drawing.Point(318, 272)
    $btnOk.Size = New-Object System.Drawing.Size(120, 32)
    $btnOk.DialogResult = [System.Windows.Forms.DialogResult]::OK
    $form.Controls.Add($btnOk)

    $form.AcceptButton = $btnOk
    $form.CancelButton = $btnSkip

    $res = $form.ShowDialog()
    $wantMpv = ($res -eq [System.Windows.Forms.DialogResult]::OK) -and (-not $HasMpv) -and $chkMpv.Checked
    $wantVlc = ($res -eq [System.Windows.Forms.DialogResult]::OK) -and (-not $HasVlc) -and $chkVlc.Checked
    $form.Dispose()

    return [PSCustomObject]@{
        InstallMpv = $wantMpv
        InstallVlc = $wantVlc
    }
}

Write-Log "Iniciando verificacion de requisitos de Playzi (InstallDir=$InstallDir)"

$existingYtDlp = Find-YtDlp
$existingFfmpeg = Find-Ffmpeg
$existingMpv = Find-Mpv
$existingVlc = Find-Vlc
$hasWinget = [bool](Get-WingetPath)

$doInstallMpv = [bool]$InstallMpv
$doInstallVlc = [bool]$InstallVlc

if (-not $Silent) {
    try {
        $choice = Show-DepsDialog `
            -HasYtDlp ([bool]$existingYtDlp) `
            -HasFfmpeg ([bool]$existingFfmpeg) `
            -HasMpv ([bool]$existingMpv) `
            -HasVlc ([bool]$existingVlc) `
            -HasWinget $hasWinget
        $doInstallMpv = [bool]$choice.InstallMpv
        $doInstallVlc = [bool]$choice.InstallVlc
    } catch {
        Write-Log "No se pudo mostrar el dialogo de seleccion: $($_.Exception.Message)"
    }
}

if ($existingYtDlp) {
    Write-Log "yt-dlp detectado: $existingYtDlp"
} else {
    $null = Install-YtDlp
}

if ($existingFfmpeg) {
    Write-Log "ffmpeg detectado: $existingFfmpeg"
} else {
    $null = Install-Ffmpeg
}

if ($doInstallMpv -and (-not $existingMpv)) {
    $null = Invoke-WingetInstall -PackageId "shinchiro.mpv" -DisplayName "MPV"
} elseif ($existingMpv) {
    Write-Log "MPV ya instalado: $existingMpv"
}

if ($doInstallVlc -and (-not $existingVlc)) {
    $null = Invoke-WingetInstall -PackageId "VideoLAN.VLC" -DisplayName "VLC"
} elseif ($existingVlc) {
    Write-Log "VLC ya instalado: $existingVlc"
}

Write-Log "Verificacion de requisitos finalizada."
exit 0
