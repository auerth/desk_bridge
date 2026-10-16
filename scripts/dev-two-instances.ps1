<#
.SYNOPSIS
  Runs two Desk Bridge agents on this PC (A and B) for development without a second computer.

.DESCRIPTION
  Each instance gets its own data directory, UDP port and UI pipe through environment variables.
  The agent crate honours exactly these three variables:

    DESKBRIDGE_DATA_ROOT   root folder for config + data of this instance (AppPaths::under(root):
                           <root>\config and <root>\data). Without it the real
                           %APPDATA%\softwelop\DeskBridge locations are used.
    DESKBRIDGE_PORT        UDP port of the QUIC listener (default 47820).
    DESKBRIDGE_PIPE        full named-pipe name of the UI IPC server (default \\.\pipe\deskbridge-ui).

  Defaults:  A = port 47820, pipe \\.\pipe\deskbridge-ui-a, data <Root>\a
             B = port 47821, pipe \\.\pipe\deskbridge-ui-b, data <Root>\b

  To pair the two instances use manual IP entry 127.0.0.1:47821 (resp. :47820); mDNS between two
  processes on the same host is unreliable.

  With -WithUi the Tauri UI is started for both instances as well (needs a debug build of
  desk-bridge-app; the single-instance plugin of the app may refuse a second window - in that case
  attach only one UI and drive the other instance with tools/fake-peer).

  Ctrl+C stops everything that was started.

.PARAMETER Root
  Base folder for the instance data (default: %TEMP%\deskbridge-dev). Use -Clean to wipe it first.

.PARAMETER Config
  Debug (default) or Release binaries from target\<config>.

.PARAMETER NoBuild
  Skip `cargo build -p deskbridge-agent`.

.EXAMPLE
  .\scripts\dev-two-instances.ps1 -Clean
#>
[CmdletBinding()]
param(
    [string]$Root = (Join-Path $env:TEMP 'deskbridge-dev'),
    [ValidateSet('Debug', 'Release')][string]$Config = 'Debug',
    [int]$PortA = 47820,
    [int]$PortB = 47821,
    [switch]$WithUi,
    [switch]$NoBuild,
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$profileDir = if ($Config -eq 'Release') { 'release' } else { 'debug' }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
$bin = Join-Path $targetDir $profileDir

. "$PSScriptRoot\dev-env.ps1"

if (-not $NoBuild) {
    $cargoArgs = @('build', '-p', 'deskbridge-agent')
    if ($WithUi) { $cargoArgs += @('-p', 'desk-bridge-app') }
    if ($Config -eq 'Release') { $cargoArgs += '--release' }
    Push-Location $repo
    try { & cargo @cargoArgs; if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" } }
    finally { Pop-Location }
}

$agentExe = Join-Path $bin 'deskbridge-agent.exe'
if (-not (Test-Path $agentExe)) { throw "missing $agentExe" }
$uiExe = Join-Path $bin 'desk-bridge-app.exe'

if ($Clean -and (Test-Path $Root)) { Remove-Item -Recurse -Force $Root }

$instances = @(
    @{ Name = 'A'; Port = $PortA; Pipe = '\\.\pipe\deskbridge-ui-a'; Data = Join-Path $Root 'a' },
    @{ Name = 'B'; Port = $PortB; Pipe = '\\.\pipe\deskbridge-ui-b'; Data = Join-Path $Root 'b' }
)

function Start-Instance([string]$exe, [hashtable]$inst) {
    New-Item -ItemType Directory -Force -Path $inst.Data | Out-Null
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = Split-Path -Parent $exe
    $psi.EnvironmentVariables['DESKBRIDGE_DATA_ROOT'] = $inst.Data
    $psi.EnvironmentVariables['DESKBRIDGE_PORT'] = [string]$inst.Port
    $psi.EnvironmentVariables['DESKBRIDGE_PIPE'] = $inst.Pipe
    if (-not $psi.EnvironmentVariables.ContainsKey('RUST_LOG')) { $psi.EnvironmentVariables['RUST_LOG'] = 'info' }
    return [System.Diagnostics.Process]::Start($psi)
}

$procs = @()
try {
    foreach ($i in $instances) {
        $p = Start-Instance $agentExe $i
        $procs += $p
        Write-Host ("agent {0}: pid {1}  udp {2}  pipe {3}  data {4}" -f $i.Name, $p.Id, $i.Port, $i.Pipe, $i.Data) -ForegroundColor Green
    }
    if ($WithUi) {
        if (Test-Path $uiExe) {
            foreach ($i in $instances) {
                $p = Start-Instance $uiExe $i
                $procs += $p
                Write-Host ("ui    {0}: pid {1}" -f $i.Name, $p.Id) -ForegroundColor Green
            }
        }
        else { Write-Warning "missing $uiExe; build it (cargo build -p desk-bridge-app) or drop -WithUi" }
    }
    Write-Host "`nRunning. Pair via manual IP: A -> 127.0.0.1:$PortB, B -> 127.0.0.1:$PortA.  Ctrl+C stops all." -ForegroundColor Cyan
    while ($true) {
        $alive = @($procs | Where-Object { -not $_.HasExited })
        if ($alive.Count -eq 0) { Write-Host 'All processes exited.'; break }
        Start-Sleep -Milliseconds 500
    }
}
finally {
    foreach ($p in $procs) {
        if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } }
    }
    Write-Host 'Stopped.'
}
