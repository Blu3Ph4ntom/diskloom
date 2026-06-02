param(
    [string]$IconDir = "icons"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$iconDirPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $IconDir))
New-Item -ItemType Directory -Path $iconDirPath -Force | Out-Null

Add-Type -AssemblyName System.Drawing

function New-RoundedRectangle {
    param(
        [float]$X,
        [float]$Y,
        [float]$Width,
        [float]$Height,
        [float]$Radius
    )

    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $diameter = $Radius * 2
    $path.AddArc($X, $Y, $diameter, $diameter, 180, 90)
    $path.AddArc($X + $Width - $diameter, $Y, $diameter, $diameter, 270, 90)
    $path.AddArc($X + $Width - $diameter, $Y + $Height - $diameter, $diameter, $diameter, 0, 90)
    $path.AddArc($X, $Y + $Height - $diameter, $diameter, $diameter, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-IconPng {
    param(
        [int]$Size,
        [string]$Path
    )

    $scale = $Size / 256.0
    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.ScaleTransform($scale, $scale)

    $shell = New-RoundedRectangle -X 18 -Y 18 -Width 220 -Height 220 -Radius 44
    $graphics.FillPath([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 12, 18, 22)), $shell)
    $graphics.DrawPath([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 112, 214, 232), 8), $shell)

    $accent = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 84, 198, 219), 10)
    $accent.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $accent.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLine($accent, 58, 190, 198, 190)

    $font = [System.Drawing.Font]::new("Segoe UI Semibold", 86, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $format = [System.Drawing.StringFormat]::new()
    $format.Alignment = [System.Drawing.StringAlignment]::Center
    $format.LineAlignment = [System.Drawing.StringAlignment]::Center
    $brush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 229, 239, 242))
    $graphics.DrawString("DL", $font, $brush, [System.Drawing.RectangleF]::new(22, 50, 212, 124), $format)

    $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
}

$pngPath = Join-Path $iconDirPath "icon.png"
$icoPath = Join-Path $iconDirPath "icon.ico"
$svgPath = Join-Path $iconDirPath "icon.svg"

New-IconPng -Size 256 -Path $pngPath
$pngBytes = [System.IO.File]::ReadAllBytes($pngPath)
$stream = [System.IO.File]::Create($icoPath)
$writer = [System.IO.BinaryWriter]::new($stream)
$writer.Write([uint16]0)
$writer.Write([uint16]1)
$writer.Write([uint16]1)
$writer.Write([byte]0)
$writer.Write([byte]0)
$writer.Write([byte]0)
$writer.Write([byte]0)
$writer.Write([uint16]1)
$writer.Write([uint16]32)
$writer.Write([uint32]$pngBytes.Length)
$writer.Write([uint32]22)
$writer.Write($pngBytes)
$writer.Dispose()
$stream.Dispose()

$svg = @'
<svg width="256" height="256" viewBox="0 0 256 256" fill="none" xmlns="http://www.w3.org/2000/svg">
  <rect x="18" y="18" width="220" height="220" rx="44" fill="#0c1216" stroke="#70d6e8" stroke-width="8"/>
  <text x="128" y="146" text-anchor="middle" font-family="Segoe UI, system-ui, sans-serif" font-size="92" font-weight="700" fill="#e5eff2">DL</text>
  <path d="M58 190H198" stroke="#54c6db" stroke-width="10" stroke-linecap="round"/>
</svg>
'@
Set-Content -LiteralPath $svgPath -Value $svg -Encoding UTF8

Write-Host "Generated $icoPath"
