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

    $shell = New-RoundedRectangle -X 18 -Y 18 -Width 220 -Height 220 -Radius 42
    $graphics.FillPath([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 16, 24, 29)), $shell)
    $graphics.DrawPath([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 169, 207, 217), 7), $shell)

    $accent = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 84, 198, 219), 8)
    $accent.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $accent.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $soft = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 218, 231, 234), 10)
    $soft.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $soft.EndCap = [System.Drawing.Drawing2D.LineCap]::Round

    $graphics.DrawBezier($accent, 58, 76, 92, 111, 160, 111, 198, 76)
    $graphics.DrawBezier($accent, 54, 104, 96, 137, 171, 137, 210, 104)
    $graphics.DrawLine($soft, 74, 170, 183, 170)
    $graphics.DrawLine($soft, 86, 90, 172, 90)
    $graphics.DrawLine($soft, 74, 170, 91, 90)
    $graphics.DrawLine($soft, 183, 170, 172, 90)
    $graphics.DrawLine([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 169, 207, 217), 7), 86, 128, 188, 128)

    $dotBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 84, 198, 219))
    foreach ($x in @(96, 128, 160)) {
        $graphics.FillEllipse($dotBrush, $x - 8, 190 - 8, 16, 16)
    }

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
  <rect x="18" y="18" width="220" height="220" rx="42" fill="#10181d" stroke="#a9cfd9" stroke-width="7"/>
  <path d="M58 76C92 111 160 111 198 76" stroke="#54c6db" stroke-width="8" stroke-linecap="round"/>
  <path d="M54 104C96 137 171 137 210 104" stroke="#54c6db" stroke-width="8" stroke-linecap="round"/>
  <path d="M74 170H183M86 90H172M74 170L91 90M183 170L172 90" stroke="#dae7ea" stroke-width="10" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M86 128H188" stroke="#a9cfd9" stroke-width="7" stroke-linecap="round"/>
  <circle cx="96" cy="190" r="8" fill="#54c6db"/>
  <circle cx="128" cy="190" r="8" fill="#54c6db"/>
  <circle cx="160" cy="190" r="8" fill="#54c6db"/>
</svg>
'@
Set-Content -LiteralPath $svgPath -Value $svg -Encoding UTF8

Write-Host "Generated $icoPath"
