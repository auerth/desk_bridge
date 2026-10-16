<#
.SYNOPSIS
  Builds the release binaries and stages everything the NSIS installer bundles into installer/stage/.

.DESCRIPTION
  Output layout (what installer/README.md maps into bundle.resources):
    installer/stage/deskbridge-agent.exe
    installer/stage/deskbridge-service.exe
    installer/stage/deskbridge-setup.exe
    installer/stage/ffmpeg/*.dll          (from vendor/ffmpeg/bin, unless -NoFfmpeg)
    installer/stage/drivers/<name>/...    (driver packages that opt in with drivers/<name>/driver.json)
    installer/stage/drivers/drivers.json  (catalogue read by `deskbridge-setup install`)

  A driver is only staged when drivers/<name>/driver.json exists and has "enabled": true. Format
  (paths relative to drivers/<name>/):
    { "name": "Desk Bridge Display", "enabled": true, "inf": "package/MttVDD.inf", "hwid": "Root\\MttVDD",
      "files": [ { "from": "config/vdd_settings.deskbridge.xml", "to": "C:\\VirtualDisplayDriver\\vdd_settings.xml" } ] }

.PARAMETER NoFfmpeg
  Do not stage FFmpeg DLLs (M0-M4 builds have no video pipeline yet).

.PARAMETER SkipBuild
  Reuse existing target/<Config> binaries.

.PARAMETER Config
  Release (default) or Debug.

.EXAMPLE
  pwsh scripts/stage-installer.ps1 -NoFfmpeg
#>
[CmdletBinding()]
param(
    [switch]$NoFfmpeg,
    [switch]$SkipBuild,
    [ValidateSet('Release', 'Debug')][string]$Config = 'Release'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$stage = Join-Path $root 'installer\stage'
$profileDir = if ($Config -eq 'Release') { 'release' } else { 'debug' }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$bin = Join-Path $targetDir $profileDir

# pip-installed cmake (needed to build libopus) lives here on the dev PC.
. "$PSScriptRoot\dev-env.ps1"

if (-not $SkipBuild) {
    Write-Host "==> cargo build ($Config): agent, service, setup" -ForegroundColor Cyan
    $cargoArgs = @('build', '-p', 'deskbridge-agent', '-p', 'deskbridge-service', '-p', 'deskbridge-setup')
    if ($Config -eq 'Release') { $cargoArgs += '--release' }
    Push-Location $root
    try {
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }
    finally { Pop-Location }
}

if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Write-Host '==> staging binaries' -ForegroundColor Cyan
foreach ($exe in 'deskbridge-agent.exe', 'deskbridge-service.exe', 'deskbridge-setup.exe') {
    $src = Join-Path $bin $exe
    if (-not (Test-Path $src)) { throw "missing $src (build first or drop -SkipBuild)" }
    Copy-Item $src $stage
    Write-Host ("    {0,-26} {1,10:N0} bytes" -f $exe, (Get-Item $src).Length)
}

# --- FFmpeg (LGPL shared build, dynamically linked) ---------------------------------------------
if ($NoFfmpeg) {
    Write-Host '==> FFmpeg skipped (-NoFfmpeg)' -ForegroundColor Yellow
}
else {
    $ffBin = Join-Path $root 'vendor\ffmpeg\bin'
    if (Test-Path $ffBin) {
        # Only the libraries the agent links (avdevice/avfilter/avformat are not needed for focus mode).
        $needed = 'avcodec-*', 'avutil-*', 'swscale-*', 'swresample-*'
        $dlls = @(Get-ChildItem $ffBin -Filter *.dll | Where-Object { $n = $_.Name; $needed | Where-Object { $n -like "$_.dll" } })
        if ($dlls.Count -eq 0) { Write-Warning "no DLLs in $ffBin" }
        else {
            $ffStage = Join-Path $stage 'ffmpeg'
            New-Item -ItemType Directory -Force -Path $ffStage | Out-Null
            $dlls | Copy-Item -Destination $ffStage
            Write-Host "==> FFmpeg: staged $($dlls.Count) DLL(s)" -ForegroundColor Cyan
            $lic = Join-Path $root 'vendor\ffmpeg\LICENSE.txt'
            if (Test-Path $lic) { Copy-Item $lic (Join-Path $ffStage 'FFMPEG-LICENSE.txt') }
        }
    }
    else {
        Write-Warning 'vendor/ffmpeg/bin not found; continuing without FFmpeg (use -NoFfmpeg to silence this).'
    }
}

# --- Drivers ----------------------------------------------------------------------------------
$catalogue = @()
$driversRoot = Join-Path $root 'drivers'
if (Test-Path $driversRoot) {
    foreach ($dir in Get-ChildItem $driversRoot -Directory) {
        $meta = Join-Path $dir.FullName 'driver.json'
        if (-not (Test-Path $meta)) {
            Write-Host "==> drivers/$($dir.Name): no driver.json, not bundled" -ForegroundColor DarkGray
            continue
        }
        $m = Get-Content $meta -Raw | ConvertFrom-Json
        if (-not $m.enabled) { Write-Host "==> drivers/$($dir.Name): disabled in driver.json" -ForegroundColor DarkGray; continue }
        $pkg = Join-Path $dir.FullName 'package'
        if (-not (Test-Path $pkg)) { Write-Warning "drivers/$($dir.Name)/package missing, skipped"; continue }

        $dst = Join-Path $stage "drivers\$($dir.Name)"
        New-Item -ItemType Directory -Force -Path $dst | Out-Null
        Copy-Item $pkg (Join-Path $dst 'package') -Recurse
        $cfg = Join-Path $dir.FullName 'config'
        if (Test-Path $cfg) { Copy-Item $cfg (Join-Path $dst 'config') -Recurse }
        foreach ($doc in 'LICENSE', 'NOTICE.md') {
            $p = Join-Path $dir.FullName $doc
            if (Test-Path $p) { Copy-Item $p $dst }
        }

        $entry = [ordered]@{
            name    = [string]$m.name
            inf     = "drivers/$($dir.Name)/$($m.inf)"
            hwid    = [string]$m.hwid
            enabled = $true
        }
        $files = @()
        if ($m.PSObject.Properties.Name -contains 'files') {
            foreach ($f in $m.files) { $files += [ordered]@{ from = "drivers/$($dir.Name)/$($f.from)"; to = [string]$f.to } }
        }
        $entry.files = $files
        $catalogue += $entry
        Write-Host "==> driver staged: $($m.name) ($($m.hwid))" -ForegroundColor Cyan
    }
}
if ($catalogue.Count -gt 0) {
    $drvStage = Join-Path $stage 'drivers'
    New-Item -ItemType Directory -Force -Path $drvStage | Out-Null
    # ConvertTo-Json unwraps single-element arrays in PS 5.1 -> force an array.
    $json = ConvertTo-Json -InputObject @($catalogue) -Depth 6
    [System.IO.File]::WriteAllText((Join-Path $drvStage 'drivers.json'), $json, (New-Object System.Text.UTF8Encoding($false)))
}

Write-Host ''
Write-Host "Staged into $stage" -ForegroundColor Green
Get-ChildItem $stage -Recurse -File | ForEach-Object { '    ' + $_.FullName.Substring($stage.Length + 1) }
