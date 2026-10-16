<#
.SYNOPSIS
  Verifies that a bundled FFmpeg build is LGPL-only (plan 12.1 / risk table: no --enable-gpl,
  no --enable-nonfree).

.DESCRIPTION
  1. If ffmpeg.exe exists in <Path>\bin (shared builds ship it) runs `ffmpeg -version` and
     `ffmpeg -L` and inspects the configuration line and the licence text.
  2. Always also scans avcodec-*.dll / avformat-*.dll / avutil-*.dll for the embedded configuration
     string (what avcodec_configuration() returns), so a build without ffmpeg.exe is checked too.
  Exit code 0 = clean, 1 = forbidden flag or GPL licence found, 2 = nothing to check.

.PARAMETER Path
  FFmpeg root (default vendor/ffmpeg). DLLs are expected in <Path>\bin.

.EXAMPLE
  .\scripts\check-ffmpeg-license.ps1
#>
[CmdletBinding()]
param([string]$Path)

$ErrorActionPreference = 'Stop'
if (-not $Path) { $Path = Join-Path (Split-Path -Parent $PSScriptRoot) 'vendor\ffmpeg' }
$bin = Join-Path $Path 'bin'
if (-not (Test-Path $bin)) { Write-Host "No FFmpeg at $bin - nothing to check."; exit 2 }

$forbidden = @('--enable-gpl', '--enable-nonfree', '--enable-version3-gpl')
$failed = $false

$ffmpeg = Join-Path $bin 'ffmpeg.exe'
if (Test-Path $ffmpeg) {
    $version = (& $ffmpeg -hide_banner -version 2>&1 | Out-String)
    $license = (& $ffmpeg -hide_banner -L 2>&1 | Out-String)
    Write-Host ($version -split "`r?`n" | Select-Object -First 2 | Out-String)
    foreach ($f in $forbidden) {
        if ($version.Contains($f)) { Write-Error "ffmpeg -version contains $f" -ErrorAction Continue; $failed = $true }
    }
    if ($license -match 'GNU General Public License' -and $license -notmatch 'Lesser') {
        Write-Error 'ffmpeg -L reports the GPL (not the LGPL).' -ErrorAction Continue; $failed = $true
    }
    if ($license -notmatch 'Lesser General Public License') {
        Write-Warning 'ffmpeg -L did not mention the LGPL.'
    }
}
else { Write-Host 'ffmpeg.exe not present; checking DLL configuration strings only.' }

$checked = 0
foreach ($dll in Get-ChildItem $bin -Filter *.dll | Where-Object { $_.Name -match '^(avcodec|avformat|avutil|swscale|swresample|avfilter)-' }) {
    $text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($dll.FullName))
    $checked++
    foreach ($f in $forbidden) {
        if ($text.Contains($f)) { Write-Error "$($dll.Name) contains '$f'" -ErrorAction Continue; $failed = $true }
    }
    if ($text.Contains('libavutil license: GPL')) { Write-Error "$($dll.Name) reports a GPL licence" -ErrorAction Continue; $failed = $true }
}
if ($checked -eq 0 -and -not (Test-Path $ffmpeg)) { Write-Host 'No FFmpeg binaries found.'; exit 2 }

if ($failed) { Write-Host 'FFmpeg licence check FAILED.' -ForegroundColor Red; exit 1 }
Write-Host "FFmpeg licence check passed ($checked DLL(s) scanned)." -ForegroundColor Green
exit 0
