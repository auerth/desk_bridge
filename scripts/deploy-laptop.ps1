<#
.SYNOPSIS
  Builds Desk Bridge and copies the binaries to the laptop (SMB share) or to a local folder.

.DESCRIPTION
  1. cargo build (agent, service, setup; with -WithUi also the Tauri app binary) unless -NoBuild.
  2. If the target is a UNC path (\\LAPTOP\share\...), tries to stop the running agent on that
     machine first (schtasks /End + taskkill over RPC; needs admin rights on the laptop). For a
     local -Destination the local agent process is stopped.
  3. Mirrors the binaries with `robocopy /MIR`. User data (%APPDATA%\softwelop\DeskBridge,
     %LOCALAPPDATA%\softwelop\DeskBridge) is never inside the deploy folder and is therefore never
     touched. installed.json, *.log and the .deskbridge-deploy marker are excluded from /MIR.
  4. With -Run the agent is restarted (scheduled task "DeskBridge Agent" on a remote machine, a
     plain process for a local destination).

  Safety: /MIR deletes everything in the destination that is not in the source. The script
  therefore refuses a non-empty destination that has no .deskbridge-deploy marker (written on
  the first deploy) unless -Force is given.

  First-time setup on the laptop (once, elevated, from the deployed folder):
      .\deskbridge-setup.exe install
  That registers the firewall rule and the "DeskBridge Agent" task pointing at the deployed
  deskbridge-agent.exe; later deploys only replace the files.

.PARAMETER Target
  UNC share or path on the laptop, e.g. \\LAPTOP\deskbridge or \\LAPTOP\c$\DeskBridge.

.PARAMETER Destination
  Local folder (e.g. D:\deskbridge-test). Alternative to -Target.

.PARAMETER Config
  Release (default) or Debug.

.PARAMETER NoBuild
  Skip cargo build and use what is already in target\<config>.

.PARAMETER WithUi
  Also build and copy the Tauri UI binary (desk-bridge-app.exe -> "Desk Bridge.exe").
  Requires `npm run build` in app/ to have produced app/dist.

.PARAMETER Run
  (Re)start the agent after copying.

.PARAMETER Force
  Allow mirroring into a non-empty folder without the deploy marker.

.EXAMPLE
  .\scripts\deploy-laptop.ps1 -Target \\LAPTOP\deskbridge -Run
.EXAMPLE
  .\scripts\deploy-laptop.ps1 -Destination D:\deskbridge-test -Config Debug -NoBuild
#>
[CmdletBinding(DefaultParameterSetName = 'Remote')]
param(
    [Parameter(ParameterSetName = 'Remote', Mandatory)][string]$Target,
    [Parameter(ParameterSetName = 'Local', Mandatory)][string]$Destination,
    [ValidateSet('Release', 'Debug')][string]$Config = 'Release',
    [switch]$NoBuild,
    [switch]$WithUi,
    [switch]$Run,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$profileDir = if ($Config -eq 'Release') { 'release' } else { 'debug' }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$bin = Join-Path $targetDir $profileDir
$dest = if ($PSCmdlet.ParameterSetName -eq 'Remote') { $Target } else { $Destination }
$marker = '.deskbridge-deploy'
$taskName = 'DeskBridge Agent'

# cmake from pip lives here on the dev PC (libopus build).
. "$PSScriptRoot\dev-env.ps1"

# Which machine is the destination on?
$remoteHost = $null
if ($dest -match '^\\\\([^\\]+)\\') {
    $remoteHost = $Matches[1]
    if ($remoteHost -in @($env:COMPUTERNAME, 'localhost', '127.0.0.1')) { $remoteHost = $null }
}

# ---- build ---------------------------------------------------------------------------------
$files = @('deskbridge-agent.exe', 'deskbridge-service.exe', 'deskbridge-setup.exe')
if (-not $NoBuild) {
    $pkgs = @('-p', 'deskbridge-agent', '-p', 'deskbridge-service', '-p', 'deskbridge-setup')
    if ($WithUi) { $pkgs += @('-p', 'desk-bridge-app') }
    $cargoArgs = @('build') + $pkgs
    if ($Config -eq 'Release') { $cargoArgs += '--release' }
    Write-Host "==> cargo $($cargoArgs -join ' ')" -ForegroundColor Cyan
    Push-Location $root
    try { & cargo @cargoArgs; if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" } }
    finally { Pop-Location }
}

# Stage into a temp folder so robocopy mirrors exactly the files we want.
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("deskbridge-deploy-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $stage | Out-Null
try {
    foreach ($f in $files) {
        $src = Join-Path $bin $f
        if (Test-Path $src) { Copy-Item $src $stage } else { Write-Warning "missing $src (skipped)" }
    }
    if ($WithUi) {
        $ui = Join-Path $bin 'desk-bridge-app.exe'
        if (Test-Path $ui) { Copy-Item $ui (Join-Path $stage 'Desk Bridge.exe') } else { Write-Warning "missing $ui (skipped)" }
    }
    $ff = Join-Path $root 'vendor\ffmpeg\bin'
    if (Test-Path $ff) { Get-ChildItem $ff -Filter *.dll | Copy-Item -Destination $stage }
    if (-not (Test-Path (Join-Path $stage 'deskbridge-agent.exe'))) { throw 'deskbridge-agent.exe was not built; nothing to deploy.' }

    # ---- safety check for /MIR ------------------------------------------------------------
    if ((Test-Path $dest) -and -not (Test-Path (Join-Path $dest $marker))) {
        $existing = @(Get-ChildItem $dest -Force -ErrorAction SilentlyContinue)
        if ($existing.Count -gt 0 -and -not $Force) {
            throw "'$dest' is not empty and has no $marker marker; /MIR would delete its contents. Use -Force if that is intended."
        }
    }

    # ---- stop the running agent ---------------------------------------------------------------
    if ($remoteHost) {
        Write-Host "==> stopping agent on $remoteHost" -ForegroundColor Cyan
        & schtasks.exe /End /S $remoteHost /TN $taskName 2>&1 | Out-Null
        & taskkill.exe /S $remoteHost /IM deskbridge-agent.exe /F 2>&1 | Out-Null
        try {
            Invoke-Command -ComputerName $remoteHost -ErrorAction Stop -ScriptBlock {
                Get-Process deskbridge-agent -ErrorAction SilentlyContinue | Stop-Process -Force
            }
        }
        catch { Write-Verbose "WinRM not available on ${remoteHost}: $($_.Exception.Message)" }
    }
    else {
        Get-Process -Name deskbridge-agent -ErrorAction SilentlyContinue | ForEach-Object {
            Write-Host "==> stopping local agent (pid $($_.Id))" -ForegroundColor Cyan
            $_.Kill(); $_.WaitForExit(5000) | Out-Null
        }
    }
    Start-Sleep -Milliseconds 500

    # ---- copy ----------------------------------------------------------------------------------
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Write-Host "==> robocopy /MIR -> $dest" -ForegroundColor Cyan
    & robocopy.exe $stage $dest /MIR /NJH /NJS /NP /R:2 /W:2 /XF installed.json '*.log' $marker /XD logs data config Incoming
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" }
    Set-Content -Path (Join-Path $dest $marker) -Value "Managed by scripts/deploy-laptop.ps1 - robocopy /MIR target. Last deploy: $(Get-Date -Format s)"

    # ---- start ---------------------------------------------------------------------------------
    if ($Run) {
        if ($remoteHost) {
            Write-Host "==> starting scheduled task '$taskName' on $remoteHost" -ForegroundColor Cyan
            & schtasks.exe /Run /S $remoteHost /TN $taskName
            if ($LASTEXITCODE -ne 0) {
                Write-Warning ("Task '$taskName' not found on $remoteHost. On the laptop run once (elevated) from the deployed folder:`n" +
                    "    .\deskbridge-setup.exe install`nor start .\deskbridge-agent.exe manually.")
            }
        }
        else {
            $exe = Join-Path $dest 'deskbridge-agent.exe'
            Write-Host "==> starting $exe" -ForegroundColor Cyan
            Start-Process -FilePath $exe -WorkingDirectory $dest
        }
    }
    Write-Host 'Done.' -ForegroundColor Green
}
finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}
