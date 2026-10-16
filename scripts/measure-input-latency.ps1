<#
.SYNOPSIS
  Placeholder wrapper for `tools/latency-probe input` (written in M2): additional input latency.

.DESCRIPTION
  The probe injects timestamped mouse/keyboard events on the source device, receives them on the
  target device and reports the one-way latency (clock offset removed through the ping/pong
  exchange of the control channel). Plan section 3 target: p95 < 3 ms on a wired LAN,
  < 15 ms on 5 GHz Wi-Fi.

  Documented arguments (the probe must implement them):
    latency-probe input --peer <host[:port]> --count <n> --rate <events/s> --kind mouse|key
                        [--out <csv>] [--json]
  Output: min / mean / p50 / p95 / p99 / max in milliseconds, optionally the raw samples as CSV.

  Until the probe exists this script prints its plan and exits with code 2.

.PARAMETER Peer
  Target device (hostname or IP, optional :port).

.PARAMETER Count
  Number of events (default 2000).

.PARAMETER Rate
  Events per second (default 500).

.PARAMETER Kind
  mouse (default) or key.

.PARAMETER Out
  Optional CSV file for the raw samples.

.EXAMPLE
  .\scripts\measure-input-latency.ps1 -Peer 192.168.1.50 -Count 5000 -Out input.csv
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Peer,
    [int]$Count = 2000,
    [int]$Rate = 500,
    [ValidateSet('mouse', 'key')][string]$Kind = 'mouse',
    [string]$Out,
    [ValidateSet('Release', 'Debug')][string]$Config = 'Release'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
$exe = Join-Path (Join-Path $targetDir $Config.ToLower()) 'latency-probe.exe'

$probeArgs = @('input', '--peer', $Peer, '--count', $Count, '--rate', $Rate, '--kind', $Kind)
if ($Out) { $probeArgs += @('--out', $Out) }

if (-not (Test-Path $exe)) {
    Write-Warning "latency-probe is not built yet ($exe). Planned invocation:"
    Write-Host "    latency-probe $($probeArgs -join ' ')"
    Write-Host "Build with: cargo build -p latency-probe$(if ($Config -eq 'Release') { ' --release' })"
    exit 2
}
& $exe @probeArgs
exit $LASTEXITCODE
