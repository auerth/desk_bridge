<#
.SYNOPSIS
  Placeholder wrapper for `tools/latency-probe audio` (written in M3): end-to-end audio latency.

.DESCRIPTION
  Method (plan section 14): the laptop plays a click track; the probe records the headset/line
  output on the PC with a second input (or a loopback cable) and measures the distance between the
  click leaving the laptop's render endpoint and arriving in the PC capture. For the microphone
  path the roles are swapped. The headset's own wireless latency (20-40 ms) is not part of the
  result when measuring at the PC's render endpoint.

  Plan targets: < 30 ms on a wired LAN, < 60 ms on 5 GHz Wi-Fi.

  Documented arguments (the probe must implement them):
    latency-probe audio --direction laptop-to-pc|pc-to-laptop --peer <host[:port]>
                        --clicks <n> --interval-ms <ms> [--capture-device <name>] [--out <csv>] [--json]

  Until the probe exists this script prints its plan and exits with code 2.

.EXAMPLE
  .\scripts\measure-audio-latency.ps1 -Peer 192.168.1.50 -Direction laptop-to-pc -Clicks 50
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Peer,
    [ValidateSet('laptop-to-pc', 'pc-to-laptop')][string]$Direction = 'laptop-to-pc',
    [int]$Clicks = 50,
    [int]$IntervalMs = 500,
    [string]$CaptureDevice,
    [string]$Out,
    [ValidateSet('Release', 'Debug')][string]$Config = 'Release'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
$exe = Join-Path (Join-Path $targetDir $Config.ToLower()) 'latency-probe.exe'

$probeArgs = @('audio', '--direction', $Direction, '--peer', $Peer, '--clicks', $Clicks, '--interval-ms', $IntervalMs)
if ($CaptureDevice) { $probeArgs += @('--capture-device', $CaptureDevice) }
if ($Out) { $probeArgs += @('--out', $Out) }

if (-not (Test-Path $exe)) {
    Write-Warning "latency-probe is not built yet ($exe). Planned invocation:"
    Write-Host "    latency-probe $($probeArgs -join ' ')"
    Write-Host "Build with: cargo build -p latency-probe$(if ($Config -eq 'Release') { ' --release' })"
    exit 2
}
& $exe @probeArgs
exit $LASTEXITCODE
