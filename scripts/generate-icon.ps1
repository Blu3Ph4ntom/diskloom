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
        [string]$Path
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
            Save-ResizedPng -Source $Source -Size $size -Path $tempPath
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
