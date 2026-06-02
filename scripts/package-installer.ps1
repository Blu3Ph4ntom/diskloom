param(
    [string]$OutputRoot = "dist",
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$outputRootPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
$installerPath = Join-Path $outputRootPath "DiskLoomSetup-x64.exe"
$legacyPackageName = "diskloom-installer-windows-x64"
$legacyPackageDir = Join-Path $outputRootPath $legacyPackageName
$legacyZipPath = Join-Path $outputRootPath "$legacyPackageName.zip"

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
Assert-UnderPath -Child $installerPath -Parent $outputRootPath
Assert-UnderPath -Child $legacyPackageDir -Parent $outputRootPath

Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        cargo build --release --locked -p diskloom-cli --bin dlm
        cargo build --release --locked -p diskloom-installer --bin diskloom-setup
        cargo tauri build --bundles nsis --ci --config tauri.installer.conf.json
    }

    if (Test-Path -LiteralPath $legacyPackageDir) {
        Remove-Item -LiteralPath $legacyPackageDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $legacyZipPath) {
        Remove-Item -LiteralPath $legacyZipPath -Force
    }

    $nsisDir = Join-Path $repoRoot "target\release\bundle\nsis"
    $nsisInstaller = Get-ChildItem -LiteralPath $nsisDir -Filter "*.exe" |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1

    if (-not $nsisInstaller) {
        throw "No NSIS installer was produced in $nsisDir"
    }

    New-Item -ItemType Directory -Path $outputRootPath -Force | Out-Null
    Copy-Item -LiteralPath $nsisInstaller.FullName -Destination $installerPath -Force

    Write-Host "Native installer written to $installerPath"
}
finally {
    Pop-Location
}
