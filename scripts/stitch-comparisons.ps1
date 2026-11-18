[CmdletBinding()]
param([Parameter(Mandatory)][string[]]$Names)
# Puts each target/ui-compare/<name>-{mockup,app}.png pair side by side (mockup
# left, app right, both at half size) in docs/verification/main-window-redesign/<name>.png.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent $PSScriptRoot
$in = Join-Path $root 'target/ui-compare'
$out = Join-Path $root 'docs/verification/main-window-redesign'
[void](New-Item -ItemType Directory -Force $out)
foreach ($name in $Names) {
    $mock = [System.Drawing.Image]::FromFile((Join-Path $in "$name-mockup.png"))
    $app = [System.Drawing.Image]::FromFile((Join-Path $in "$name-app.png"))
    try {
        $gap = 16
        $w = [int]($mock.Width / 2) + $gap + [int]($app.Width / 2)
        $h = [int]([Math]::Max($mock.Height, $app.Height) / 2)
        $canvas = New-Object System.Drawing.Bitmap $w, $h
        $g = [System.Drawing.Graphics]::FromImage($canvas)
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.Clear([System.Drawing.Color]::FromArgb(255, 40, 40, 40))
        $g.DrawImage($mock, 0, 0, [int]($mock.Width / 2), [int]($mock.Height / 2))
        $g.DrawImage($app, [int]($mock.Width / 2) + $gap, 0, [int]($app.Width / 2), [int]($app.Height / 2))
        $g.Dispose()
        $target = Join-Path $out "$name.png"
        $canvas.Save($target, [System.Drawing.Imaging.ImageFormat]::Png)
        $canvas.Dispose()
        Write-Output $target
    } finally {
        $mock.Dispose()
        $app.Dispose()
    }
}
