param(
    [string]$SourceImage = "assets\icon.png",
    [string]$IconDir = "icons",
    [string]$FrontendPublicDir = "frontend\public"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$sourcePath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $SourceImage))
$iconDirPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $IconDir))
$frontendPublicPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $FrontendPublicDir))

if (-not (Test-Path -LiteralPath $sourcePath)) {
    throw "Source icon not found: $sourcePath"
}

New-Item -ItemType Directory -Path $iconDirPath -Force | Out-Null
New-Item -ItemType Directory -Path $frontendPublicPath -Force | Out-Null

Add-Type -AssemblyName System.Drawing

function Save-ResizedPng {
    param(
        [System.Drawing.Image]$Source,
        [int]$Size,
        [string]$Path,
        [double]$Padding = 0.06
    )

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $graphics.Clear([System.Drawing.Color]::Transparent)

        $targetSize = [Math]::Max(1.0, $Size * (1.0 - (2.0 * $Padding)))
        $scale = [Math]::Min($targetSize / [double]$Source.Width, $targetSize / [double]$Source.Height)
        $drawWidth = [int][Math]::Round($Source.Width * $scale)
        $drawHeight = [int][Math]::Round($Source.Height * $scale)
        $x = [int][Math]::Floor(($Size - $drawWidth) / 2)
        $y = [int][Math]::Floor(($Size - $drawHeight) / 2)
        $graphics.DrawImage($Source, $x, $y, $drawWidth, $drawHeight)

        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Save-MarkPng {
    param(
        [int]$Size,
        [string]$Path
    )

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.Clear([System.Drawing.Color]::Transparent)

        $scale = $Size / 256.0
        $rect = [System.Drawing.RectangleF]::new(24 * $scale, 24 * $scale, 208 * $scale, 208 * $scale)
        $hub = [System.Drawing.RectangleF]::new(86 * $scale, 86 * $scale, 84 * $scale, 84 * $scale)
        $core = [System.Drawing.RectangleF]::new(111 * $scale, 111 * $scale, 34 * $scale, 34 * $scale)

        $shadow = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(100, 0, 10, 24))
        $disc = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
            $rect,
            [System.Drawing.Color]::FromArgb(255, 236, 244, 255),
            [System.Drawing.Color]::FromArgb(255, 71, 95, 132),
            135
        )
        $hubBrush = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
            $hub,
            [System.Drawing.Color]::FromArgb(255, 242, 248, 255),
            [System.Drawing.Color]::FromArgb(255, 51, 65, 94),
            135
        )
        $cyan = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 22, 219, 211))
        $blue = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 0, 137, 244))
        $violet = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 117, 39, 236))
        $dark = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 7, 14, 28))
        $linePen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(210, 32, 224, 218), [Math]::Max(2.0, 8.0 * $scale))
        $outline = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(220, 179, 233, 255), [Math]::Max(1.0, 4.0 * $scale))
        try {
            $graphics.FillEllipse($shadow, [System.Drawing.RectangleF]::new(30 * $scale, 32 * $scale, 208 * $scale, 208 * $scale))
            $graphics.FillPie($cyan, $rect, 276, 54)
            $graphics.FillPie($blue, $rect, 333, 78)
            $graphics.FillPie($violet, $rect, 44, 72)
            $graphics.FillEllipse($disc, [System.Drawing.RectangleF]::new(50 * $scale, 50 * $scale, 156 * $scale, 156 * $scale))
            $graphics.DrawEllipse($outline, [System.Drawing.RectangleF]::new(50 * $scale, 50 * $scale, 156 * $scale, 156 * $scale))
            $graphics.FillEllipse($hubBrush, $hub)
            $graphics.FillEllipse($dark, [System.Drawing.RectangleF]::new(101 * $scale, 101 * $scale, 54 * $scale, 54 * $scale))
            $graphics.FillEllipse($hubBrush, $core)

            foreach ($y in @(58, 84, 111, 140, 171)) {
                $width = @(98, 82, 116, 78, 104)[[Array]::IndexOf(@(58, 84, 111, 140, 171), $y)]
                $graphics.DrawLine($linePen, 8 * $scale, $y * $scale, $width * $scale, $y * $scale)
            }
        }
        finally {
            $shadow.Dispose()
            $disc.Dispose()
            $hubBrush.Dispose()
            $cyan.Dispose()
            $blue.Dispose()
            $violet.Dispose()
            $dark.Dispose()
            $linePen.Dispose()
            $outline.Dispose()
        }

        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Write-IconFile {
    param(
        [System.Drawing.Image]$Source,
        [string]$Path
    )

    $sizes = @(256, 128, 64, 48, 32, 16)
    $pngEntries = @()
    $tempFiles = @()
    try {
        foreach ($size in $sizes) {
            $tempPath = [System.IO.Path]::GetTempFileName()
            $tempFiles += $tempPath
            if ($size -le 64) {
                Save-MarkPng -Size $size -Path $tempPath
            } else {
                Save-ResizedPng -Source $Source -Size $size -Path $tempPath -Padding 0.08
            }
            $pngEntries += [pscustomobject]@{
                Size = $size
                Bytes = [System.IO.File]::ReadAllBytes($tempPath)
            }
        }

        $stream = [System.IO.File]::Create($Path)
        $writer = [System.IO.BinaryWriter]::new($stream)
        try {
            $writer.Write([uint16]0)
            $writer.Write([uint16]1)
            $writer.Write([uint16]$pngEntries.Count)

            $offset = 6 + (16 * $pngEntries.Count)
            foreach ($entry in $pngEntries) {
                $dimension = if ($entry.Size -eq 256) { 0 } else { $entry.Size }
                $writer.Write([byte]$dimension)
                $writer.Write([byte]$dimension)
                $writer.Write([byte]0)
                $writer.Write([byte]0)
                $writer.Write([uint16]1)
                $writer.Write([uint16]32)
                $writer.Write([uint32]$entry.Bytes.Length)
                $writer.Write([uint32]$offset)
                $offset += $entry.Bytes.Length
            }

            foreach ($entry in $pngEntries) {
                $writer.Write($entry.Bytes)
            }
        }
        finally {
            $writer.Dispose()
            $stream.Dispose()
        }
    }
    finally {
        foreach ($tempFile in $tempFiles) {
            if (Test-Path -LiteralPath $tempFile) {
                Remove-Item -LiteralPath $tempFile -Force
            }
        }
    }
}

$strippedTempPath = [System.IO.Path]::GetTempFileName()
$source = [System.Drawing.Image]::FromFile($sourcePath)
try {
    Save-ResizedPng -Source $source -Size 1024 -Path $strippedTempPath
}
finally {
    $source.Dispose()
}

$cleanSource = [System.Drawing.Image]::FromFile($strippedTempPath)
try {
    Save-ResizedPng -Source $cleanSource -Size 256 -Path (Join-Path $iconDirPath "icon.png") -Padding 0.04
    Save-ResizedPng -Source $cleanSource -Size 256 -Path (Join-Path $frontendPublicPath "icon.png")
    Save-MarkPng -Size 256 -Path (Join-Path $iconDirPath "icon-small.png")
    Save-MarkPng -Size 256 -Path (Join-Path $frontendPublicPath "icon-small.png")
    Write-IconFile -Source $cleanSource -Path (Join-Path $iconDirPath "icon.ico")
}
finally {
    $cleanSource.Dispose()
    if (Test-Path -LiteralPath $strippedTempPath) {
        Remove-Item -LiteralPath $strippedTempPath -Force
    }
}

Write-Host "Generated DiskLoom icons from $sourcePath"
