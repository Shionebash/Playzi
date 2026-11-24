param(
    [Parameter(Position = 0)]
    [string]$Version = "",
    [switch]$Major,
    [switch]$Minor,
    [switch]$Patch
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$cargoTomlPath = Join-Path $repoRoot "Cargo.toml"
$tauriConfPath = Join-Path $repoRoot "crates\playzi-desktop\tauri.conf.json"
$cargoLockPath = Join-Path $repoRoot "Cargo.lock"

$cargoContent = Get-Content -LiteralPath $cargoTomlPath -Raw
$versionMatch = [regex]::Match($cargoContent, '(?ms)(\[workspace\.package\][^\[]*?version\s*=\s*")(\d+)\.(\d+)\.(\d+)(")')
if (-not $versionMatch.Success) {
    throw "Could not locate [workspace.package] version in $cargoTomlPath"
}

$curMajor = [int]$versionMatch.Groups[2].Value
$curMinor = [int]$versionMatch.Groups[3].Value
$curPatch = [int]$versionMatch.Groups[4].Value
$oldVersion = "$curMajor.$curMinor.$curPatch"

if (-not [string]::IsNullOrWhiteSpace($Version)) {
    $cleaned = $Version.Trim().TrimStart('v', 'V')
    if ($cleaned -notmatch '^\d+\.\d+\.\d+$') {
        throw "Invalid version '$Version'. Expected semantic version format X.Y.Z (e.g. 1.2.0)."
    }
    $newVersion = $cleaned
} elseif ($Major) {
    $newVersion = "$($curMajor + 1).0.0"
} elseif ($Minor) {
    $newVersion = "$curMajor.$($curMinor + 1).0"
} else {
    $newVersion = "$curMajor.$curMinor.$($curPatch + 1)"
}

# 1. Update Cargo.toml ([workspace.package])
$updatedCargo = [regex]::Replace(
    $cargoContent,
    '(?ms)(\[workspace\.package\][^\[]*?version\s*=\s*")\d+\.\d+\.\d+(")',
    ('${1}' + $newVersion + '${2}'),
    1
)
[System.IO.File]::WriteAllText($cargoTomlPath, $updatedCargo, [System.Text.UTF8Encoding]::new($false))

# 2. Update crates/playzi-desktop/tauri.conf.json
if (Test-Path -LiteralPath $tauriConfPath) {
    $tauriContent = Get-Content -LiteralPath $tauriConfPath -Raw
    $updatedTauri = [regex]::Replace(
        $tauriContent,
        '("version"\s*:\s*")\d+\.\d+\.\d+(")',
        ('${1}' + $newVersion + '${2}'),
        1
    )
    [System.IO.File]::WriteAllText($tauriConfPath, $updatedTauri, [System.Text.UTF8Encoding]::new($false))
}

# 3. Update Cargo.lock entries for workspace crates (playzi-core, playzi-desktop)
if (Test-Path -LiteralPath $cargoLockPath) {
    $lockContent = Get-Content -LiteralPath $cargoLockPath -Raw
    $updatedLock = [regex]::Replace(
        $lockContent,
        '(?m)(name\s*=\s*"(?:playzi-core|playzi-desktop)"\r?\nversion\s*=\s*")\d+\.\d+\.\d+(")',
        ('${1}' + $newVersion + '${2}')
    )
    [System.IO.File]::WriteAllText($cargoLockPath, $updatedLock, [System.Text.UTF8Encoding]::new($false))
}

Write-Output "Playzi version updated: $oldVersion -> $newVersion"
