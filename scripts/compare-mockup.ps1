[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Variant,
    [int]$Width = 1180,
    [int]$Height = 900
)
# Screenshots one screen of docs/mockups/2026-11-17-main-window.html and the real
# egui window (examples/ui_snapshot.rs) at the same size and device scale, for
# side-by-side comparison. Writes target/ui-compare/<Variant>-{mockup,app}.png.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target/ui-compare'
[void](New-Item -ItemType Directory -Force $out)
$app = Join-Path $out "$Variant-app.png"
$mock = Join-Path $out "$Variant-mockup.png"
$previousAutoInstall = $env:RUSTUP_AUTO_INSTALL
$env:RUSTUP_AUTO_INSTALL = '0'
Push-Location $root
try {
    $lines = & cargo +stable run --quiet --offline --example ui_snapshot -- $Variant $Width $Height $app
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $app)) { throw 'ui_snapshot failed' }
    $scale = 1.0
    foreach ($line in $lines) {
        if ($line -match 'pixels_per_point=([0-9.]+)') { $scale = [double]$Matches[1] }
    }
    $edge = @(
        "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
        "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $edge) { throw 'Microsoft Edge was not found.' }
    $page = [Uri]::new((Join-Path $root 'docs/mockups/2026-11-17-main-window.html')).AbsoluteUri + "#$Variant"
    & $edge --headless=new --disable-gpu --hide-scrollbars --virtual-time-budget=3000 `
        "--force-device-scale-factor=$scale" "--window-size=$Width,$Height" "--screenshot=$mock" $page 2>$null | Out-Null
    if (-not (Test-Path $mock)) { throw 'Mockup screenshot failed.' }
    Write-Output $mock
    Write-Output $app
} finally {
    Pop-Location
    $env:RUSTUP_AUTO_INSTALL = $previousAutoInstall
}
