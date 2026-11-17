[CmdletBinding()]
param([switch]$Check)
$ErrorActionPreference = 'Stop'
$taskRepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $taskRepoRoot
try {
    # Only locked normal/build dependencies reachable for the shipped Windows target.
    # Cached crate license files and committed supplements make this offline/repeatable.
    $raw = & cargo +stable metadata --locked --offline --filter-platform x86_64-pc-windows-msvc --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read locked dependency metadata.' }
    $metadata = ($raw -join "`n") | ConvertFrom-Json
    $packagesById = @{}
    $nodes = @{}
    foreach ($package in $metadata.packages) { $packagesById[$package.id] = $package }
    foreach ($node in $metadata.resolve.nodes) { $nodes[$node.id] = $node }
    $seen = [Collections.Generic.HashSet[string]]::new()
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($metadata.resolve.root)
    while ($pending.Count -gt 0) {
        $id = $pending.Pop()
        if (-not $seen.Add($id)) { continue }
        foreach ($dependency in $nodes[$id].deps) {
            if (@($dependency.dep_kinds | Where-Object { $_.kind -ne 'dev' }).Count -gt 0) {
                $pending.Push($dependency.pkg)
            }
        }
    }
    $packages = @($seen | ForEach-Object { $packagesById[$_] } | Where-Object { $_.source } | Sort-Object name,version)
    $supplements = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'third-party-supplements.json')) | ConvertFrom-Json
    $texts = @{}
    $rows = [Collections.Generic.List[string]]::new()
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        foreach ($package in $packages) {
            $folder = Split-Path -Parent $package.manifest_path
            $entries = [Collections.Generic.List[object]]::new()
            $files = @(Get-ChildItem -LiteralPath $folder -File -Recurse | Where-Object {
                $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|PATENTS|UNLICENSE)([-_.].*)?$|^(OFL|UFL)\.txt$|^emoji-icon-font-mit-license\.txt$'
            } | Sort-Object FullName)
            if ($package.license_file) {
                $explicit = $package.license_file
                if (-not [IO.Path]::IsPathRooted($explicit)) { $explicit = Join-Path $folder $explicit }
                $files += Get-Item -LiteralPath $explicit
            }
            foreach ($file in $files) {
                $entries.Add(@{
                    text = [IO.File]::ReadAllText($file.FullName)
                    source = $file.FullName.Substring($folder.Length + 1).Replace('\','/')
                    note = 'From the published crate source.'
                })
            }
            $key = '{0} {1}' -f $package.name,$package.version
            foreach ($supplement in $supplements) {
                if ($supplement.packages -contains $key) {
                    $entries.Add(@{ text=$supplement.text; source=$supplement.source; note=$supplement.note })
                }
            }
            if ($entries.Count -eq 0) { throw "No retained license/notice text for $key. Add an attributed supplement before packaging." }
            $links = [Collections.Generic.List[string]]::new()
            foreach ($entry in $entries) {
                $content = [regex]::Replace($entry.text.Replace("`r`n","`n").Trim(),'(?m)[ \t]+$','')
                $hash = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($content)))).Replace('-','').ToLowerInvariant()
                if (-not $texts.ContainsKey($hash)) {
                    $texts[$hash] = @{text=$content; sources=[Collections.Generic.HashSet[string]]::new()}
                }
                [void]$texts[$hash].sources.Add(('{0}: {1}. {2}' -f $key,$entry.source,$entry.note))
                $link = '[text {0}](#license-{0})' -f $hash.Substring(0,16)
                if (-not $links.Contains($link)) { $links.Add($link) }
            }
            $license = $package.license
            if (-not $license) { $license = 'See upstream license file' }
            $rows.Add(('| {0} | {1} | [upstream]({2}) | {3} |' -f $key,$license,$package.repository,($links -join ', ')))
        }
    } finally { $sha.Dispose() }
    $out = [Text.StringBuilder]::new()
    [void]$out.AppendLine('# Third-party notices — VectrCap x64 Windows')
    [void]$out.AppendLine()
    # Git may check the same lockfile out with LF or CRLF. Fingerprint its
    # canonical UTF-8/LF text so packaging does not depend on core.autocrlf.
    $lockText = [IO.File]::ReadAllText((Join-Path $taskRepoRoot 'Cargo.lock')).Replace("`r`n", "`n")
    $lockSha = [Security.Cryptography.SHA256]::Create()
    try {
        $lockHash = ([BitConverter]::ToString($lockSha.ComputeHash([Text.Encoding]::UTF8.GetBytes($lockText)))).Replace('-', '').ToLowerInvariant()
    } finally { $lockSha.Dispose() }
    [void]$out.AppendLine(('Cargo.lock SHA-256 (UTF-8, LF): `{0}`.' -f $lockHash))
    [void]$out.AppendLine()
    [void]$out.AppendLine(('Generated by scripts/generate-notices.ps1 for {0} locked normal/build dependency packages reachable on x86_64-pc-windows-msvc. Dev-only and other-platform-only packages are excluded. License expressions are retained as declared by each package; referenced texts include bundled native-library and font notices. Duplicate texts are shared below. The application license is separate.' -f $packages.Count))
    [void]$out.AppendLine()
    [void]$out.AppendLine('Supplements retain sources for crates that omit repository-level licenses from their published archives. Any source-revision limitations are stated beside the affected text; no missing copyright year is invented.')
    [void]$out.AppendLine()
    [void]$out.AppendLine('| Crate and version | Declared license | Source | License / notices |')
    [void]$out.AppendLine('|---|---|---|---|')
    foreach ($row in $rows) { [void]$out.AppendLine($row) }
    # Geist is embedded by the application, independently of Cargo packages.
    [void]$out.AppendLine()
    [void]$out.AppendLine('## Application-bundled Geist Regular, Medium and SemiBold')
    [void]$out.AppendLine('Unmodified Vercel Geist Regular, Medium and SemiBold. Source revision and font hashes: assets/fonts/README.md. License: SIL OFL 1.1, retained verbatim below and in the release package.')
    [void]$out.AppendLine()
    [void]$out.AppendLine('~~~text')
    [void]$out.AppendLine([IO.File]::ReadAllText((Join-Path $taskRepoRoot 'assets/fonts/OFL.txt')).Replace("`r`n","`n").Trim())
    [void]$out.AppendLine('~~~')
    foreach ($hash in @($texts.Keys | Sort-Object)) {
        [void]$out.AppendLine()
        [void]$out.AppendLine(('<a id="license-{0}"></a>' -f $hash.Substring(0,16)))
        [void]$out.AppendLine(('## License text {0}' -f $hash.Substring(0,16)))
        [void]$out.AppendLine()
        foreach ($source in @($texts[$hash].sources | Sort-Object)) { [void]$out.AppendLine(('- {0}' -f $source)) }
        [void]$out.AppendLine()
        [void]$out.AppendLine('~~~text')
        [void]$out.AppendLine($texts[$hash].text)
        [void]$out.AppendLine('~~~')
    }
    $content = [regex]::Replace($out.ToString().Replace("`r`n","`n"),'(?m)[ \t]+$','')
    $destination = Join-Path $taskRepoRoot 'assets/THIRD-PARTY-NOTICES.md'
    if ($Check) {
        if (-not (Test-Path -LiteralPath $destination) -or [IO.File]::ReadAllText($destination).Replace("`r`n","`n") -cne $content) {
            throw 'Third-party notices are stale. Run scripts/generate-notices.ps1 and review the diff.'
        }
    } else {
        [IO.File]::WriteAllText($destination,$content,[Text.UTF8Encoding]::new($false))
    }
    Write-Host "Verified notices for $($packages.Count) dependency packages."
} finally { Pop-Location }
