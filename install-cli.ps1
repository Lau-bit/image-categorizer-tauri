<#
.SYNOPSIS
  Builds `icat` and copies it to a stable path so agents can call it.

.DESCRIPTION
  `icat` is the agent-facing door to this app's extracted-text index — it works with the GUI closed
  and takes no focus, which is the whole reason it exists (a WebView2 window can only receive
  synthetic input in the foreground, so driving the UI would steal focus from whoever is using the
  machine).

  For Hermes and Claude Code to call it reliably it needs a path that does not move.
  `src-tauri\target\release\icat.exe` is not that path: it does not exist until a release build and
  it disappears on `cargo clean`. So the built binary is copied to the same folder as the other
  agent-facing tools on this machine (`known-good.ps1`, `dev-process-registry.ps1`), which is
  already the first place an agent looks.

.PARAMETER Destination
  Where to install. Defaults to ~\.claude\scripts.

.PARAMETER SkipBuild
  Copy the binary that is already built instead of rebuilding.

.EXAMPLE
  pwsh install-cli.ps1
  pwsh install-cli.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $HOME '.claude\scripts'),
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$cargoDir = Join-Path $projectRoot 'src-tauri'
$builtExe = Join-Path $cargoDir 'target\release\icat.exe'

if (-not $SkipBuild) {
    Write-Host 'Building icat (release)...'
    Push-Location $cargoDir
    try {
        cargo build --release --bin icat
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $builtExe)) {
    throw "icat.exe not found at $builtExe. Run without -SkipBuild."
}

if (-not (Test-Path $Destination)) {
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
}

$target = Join-Path $Destination 'icat.exe'

# A running icat would hold its own file. It is a short-lived console tool, so this is rare, but the
# failure is an opaque "access denied" that reads like a permissions problem rather than a lock.
try {
    Copy-Item -Path $builtExe -Destination $target -Force
} catch {
    throw "Could not write $target - is an icat process still running? ($_)"
}

$version = & $target status --help 2>&1 | Out-Null
Write-Host "Installed: $target"
Write-Host ''
Write-Host 'Try it:'
Write-Host "  icat status"
Write-Host "  icat search `"webview2 cdp`" --from 7d"
Write-Host "  icat timeline --bucket 168"
Write-Host ''
Write-Host 'Root resolution: --root, else $env:ICAT_ROOT, else the root the app last opened.'
