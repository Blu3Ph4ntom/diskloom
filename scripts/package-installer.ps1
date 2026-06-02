param(
    [string]$OutputRoot = "dist",
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$outputRootPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
$packageName = "diskloom-installer-windows-x64"
$packageDir = Join-Path $outputRootPath $packageName
$zipPath = Join-Path $outputRootPath "$packageName.zip"

function Assert-UnderPath {
    param(
        [string]$Child,
        [string]$Parent
    )

    $childFull = [System.IO.Path]::GetFullPath($Child).TrimEnd('\') + '\'
    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\') + '\'
    if (-not $childFull.StartsWith($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to write outside $parentFull"
    }
}

Assert-UnderPath -Child $outputRootPath -Parent $repoRoot
Assert-UnderPath -Child $packageDir -Parent $outputRootPath

Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        cargo build --release --locked -p diskloom-app
        cargo build --release --locked -p diskloom-cli --bin dlm
        cargo build --release --locked -p diskloom-installer --bin diskloom-setup
    }

    if (Test-Path -LiteralPath $packageDir) {
        Remove-Item -LiteralPath $packageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $packageDir | Out-Null

    Copy-Item -LiteralPath (Join-Path $repoRoot "target\release\diskloom.exe") -Destination $packageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot "target\release\dlm.exe") -Destination $packageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot "target\release\diskloom-setup.exe") -Destination $packageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination $packageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE-MIT") -Destination $packageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE-APACHE") -Destination $packageDir

    $docsDir = Join-Path $packageDir "docs"
    New-Item -ItemType Directory -Path $docsDir | Out-Null
    Copy-Item -LiteralPath (Join-Path $repoRoot "docs\BENCHMARKS.md") -Destination $docsDir
    Copy-Item -LiteralPath (Join-Path $repoRoot "docs\ROADMAP.md") -Destination $docsDir

    if (Test-Path -LiteralPath $zipPath) {
        Remove-Item -LiteralPath $zipPath -Force
    }
    Compress-Archive -Path (Join-Path $packageDir "*") -DestinationPath $zipPath -CompressionLevel Optimal

    Write-Host "Installer package written to $zipPath"
}
finally {
    Pop-Location
}
