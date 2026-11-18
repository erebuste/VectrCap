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
    # One fixed local output folder, replaced on every build. No registry
    # writes, global installation, elevation or execution-policy changes.
    $packages = Join-Path $taskRepoRoot 'target/packages'
    $package = Join-Path $packages 'VectrCap-x64'
    # A running packaged VectrCap locks its exe: stop only copies started from
    # target/packages (an installed or dev copy elsewhere keeps running).
    $running = @(Get-Process -Name 'VectrCap' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($packages, [StringComparison]::OrdinalIgnoreCase) })
    if ($running.Count -gt 0) {
        Write-Host "Stopping $($running.Count) running packaged VectrCap process(es)."
        $running | Stop-Process -Force
        $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    }
    # Replace the package, and drop the per-build folders older scripts made.
    # Windows can hold a stopped exe's file lock for a moment, so retry.
    for ($attempt = 1; ; $attempt++) {
        try {
            Get-ChildItem -LiteralPath $packages -Directory -Filter 'VectrCap-x64*' -ErrorAction SilentlyContinue |
                Remove-Item -Recurse -Force
            break
        } catch {
            if ($attempt -ge 20) { throw "Could not replace the package folder: $_" }
            Start-Sleep -Milliseconds 250
        }
    }
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
