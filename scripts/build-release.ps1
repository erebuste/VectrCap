[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$taskRepoRoot = Split-Path -Parent $PSScriptRoot
$previousAutoInstall = $env:RUSTUP_AUTO_INSTALL
$env:RUSTUP_AUTO_INSTALL = '0'
Push-Location $taskRepoRoot
try {
    if (-not [Environment]::Is64BitOperatingSystem -or $env:OS -ne 'Windows_NT') {
        throw 'The release package requires x64 Windows with the MSVC build tools and Windows SDK.'
    }
    $descriptor = [IO.File]::ReadAllText((Join-Path $taskRepoRoot 'rust-toolchain.toml'))
    if ($descriptor -notmatch 'channel\s*=\s*"([0-9.]+)"') { throw 'Missing exact Rust version.' }
    $pinned = $Matches[1]
    $compiler = & rustc +stable --version
    if ($LASTEXITCODE -ne 0 -or $compiler -notmatch "^rustc $([regex]::Escape($pinned)) ") {
        throw "The installed +stable alias must be rustc $pinned. Provision it explicitly; auto-install is disabled."
    }
    $targets = & rustup target list --installed --toolchain stable
    if ($LASTEXITCODE -ne 0 -or $targets -notcontains 'x86_64-pc-windows-msvc') {
        throw 'Target x86_64-pc-windows-msvc is not installed for +stable.'
    }
    & cargo +stable build --release --locked --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed; no package was published.' }
    & (Join-Path $PSScriptRoot 'generate-notices.ps1') -Check
    # A fresh local output folder avoids overwriting an existing package. No
    # registry writes, global installation, elevation or execution-policy changes.
    $package = Join-Path $taskRepoRoot ('target/packages/VectrCap-x64-' + [guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory((Join-Path $package 'assets/fonts'))
    Copy-Item -LiteralPath 'target/x86_64-pc-windows-msvc/release/vectrcap.exe' -Destination (Join-Path $package 'VectrCap.exe')
    Copy-Item -LiteralPath 'assets/THIRD-PARTY-NOTICES.md' -Destination (Join-Path $package 'assets/THIRD-PARTY-NOTICES.md')
    Copy-Item -LiteralPath 'assets/fonts/OFL.txt' -Destination (Join-Path $package 'assets/fonts/OFL.txt')
    Copy-Item -LiteralPath 'README.md' -Destination (Join-Path $package 'README.md')
    Write-Host "Unsigned portable release package: $package"
    Write-Output $package
} finally {
    Pop-Location
    $env:RUSTUP_AUTO_INSTALL = $previousAutoInstall
}
