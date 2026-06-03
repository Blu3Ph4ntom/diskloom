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

function New-ResizedBitmap {
    param(
        [System.Drawing.Image]$Source,
        [int]$Size
    )

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $graphics.Clear([System.Drawing.Color]::Transparent)

        $scale = [Math]::Min($Size / [double]$Source.Width, $Size / [double]$Source.Height)
        $drawWidth = [int][Math]::Round($Source.Width * $scale)
        $drawHeight = [int][Math]::Round($Source.Height * $scale)
        $x = [int][Math]::Floor(($Size - $drawWidth) / 2)
        $y = [int][Math]::Floor(($Size - $drawHeight) / 2)
        $graphics.DrawImage($Source, $x, $y, $drawWidth, $drawHeight)

        return $bitmap
    }
    finally {
        $graphics.Dispose()
    }
}

function Save-ResizedPng {
    param(
        [System.Drawing.Image]$Source,
        [int]$Size,
        [string]$Path
    )

    $bitmap = New-ResizedBitmap -Source $Source -Size $Size
    try {
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $bitmap.Dispose()
    }
}

function New-IconDibBytes {
    param(
        [System.Drawing.Image]$Source,
        [int]$Size
    )

    $bitmap = New-ResizedBitmap -Source $Source -Size $Size
    $stream = [System.IO.MemoryStream]::new()
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        $maskStride = [int]([Math]::Floor(($Size + 31) / 32) * 4)
        $pixelBytes = $Size * $Size * 4
        $maskBytes = $maskStride * $Size

        $writer.Write([uint32]40)
        $writer.Write([int32]$Size)
        $writer.Write([int32]($Size * 2))
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]0)
        $writer.Write([uint32]($pixelBytes + $maskBytes))
        $writer.Write([int32]0)
        $writer.Write([int32]0)
        $writer.Write([uint32]0)
        $writer.Write([uint32]0)

        for ($y = $Size - 1; $y -ge 0; $y--) {
            for ($x = 0; $x -lt $Size; $x++) {
                $color = $bitmap.GetPixel($x, $y)
                $writer.Write([byte]$color.B)
                $writer.Write([byte]$color.G)
                $writer.Write([byte]$color.R)
                $writer.Write([byte]$color.A)
            }
        }

        for ($y = $Size - 1; $y -ge 0; $y--) {
            $maskRow = [byte[]]::new($maskStride)
            for ($x = 0; $x -lt $Size; $x++) {
                $color = $bitmap.GetPixel($x, $y)
                if ($color.A -lt 128) {
                    $byteIndex = [int][Math]::Floor($x / 8)
                    $bit = [byte](0x80 -shr ($x % 8))
                    $maskRow[$byteIndex] = [byte]($maskRow[$byteIndex] -bor $bit)
                }
            }
            $writer.Write($maskRow)
        }

        return ,$stream.ToArray()
    }
    finally {
        $writer.Dispose()
        $stream.Dispose()
        $bitmap.Dispose()
    }
}

function Write-IconFile {
    param(
        [System.Drawing.Image]$Source,
        [string]$Path
    )

    $sizes = @(256, 128, 64, 48, 32, 16)
    $entries = @()
    foreach ($size in $sizes) {
        $entries += [pscustomobject]@{
            Size = $size
            Bytes = [byte[]](New-IconDibBytes -Source $Source -Size $size)
        }
    }

    $stream = [System.IO.File]::Create($Path)
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([uint16]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]$entries.Count)

        $offset = 6 + (16 * $entries.Count)
        foreach ($entry in $entries) {
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

        foreach ($entry in $entries) {
            $writer.Write($entry.Bytes)
        }
    }
    finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

$source = [System.Drawing.Image]::FromFile($sourcePath)
try {
    Save-ResizedPng -Source $source -Size 256 -Path (Join-Path $iconDirPath "icon.png")
    Write-IconFile -Source $source -Path (Join-Path $iconDirPath "icon.ico")
}
finally {
    $source.Dispose()
}

Copy-Item -LiteralPath $sourcePath -Destination (Join-Path $frontendPublicPath "icon.png") -Force

Write-Host "Generated DiskLoom icons from $sourcePath"
