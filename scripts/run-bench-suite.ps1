param(
    [string]$Path = ".",
    [ValidateSet("auto", "fallback", "ntfs")]
    [string]$Scanner = "fallback",
    [int]$Iterations = 5,
    [int]$SampleMs = 10,
    [int]$ProgressEvery = 1024,
    [string]$OutputRoot = "target\bench-suites",
    [string]$DatasetLabel = "unspecified",
    [string]$CacheState = "unknown",
    [string]$CompetitorCsv = "",
    [string[]]$Claim = @(),
    [switch]$FilesOnly,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$outputRootPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))

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

function Assert-Positive {
    param(
        [string]$Name,
        [int]$Value
    )

    if ($Value -lt 1) {
        throw "$Name must be at least 1"
    }
}

Assert-Positive -Name "Iterations" -Value $Iterations
Assert-Positive -Name "SampleMs" -Value $SampleMs
Assert-Positive -Name "ProgressEvery" -Value $ProgressEvery
Assert-UnderPath -Child $outputRootPath -Parent $repoRoot

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$leaf = "diskloom-$Scanner-$timestamp"
$outputDir = Join-Path $outputRootPath $leaf
Assert-UnderPath -Child $outputDir -Parent $outputRootPath

if (Test-Path -LiteralPath $outputDir) {
    throw "Benchmark output directory already exists: $outputDir"
}

Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        cargo build --release --locked -p diskloom-bench
    }

    $benchExe = Join-Path $repoRoot "target\release\diskloom-bench.exe"
    if (-not (Test-Path -LiteralPath $benchExe)) {
        throw "Missing benchmark binary: $benchExe"
    }

    $suiteArgs = @(
        "suite",
        $Path,
        $outputDir,
        "--dataset-label",
        $DatasetLabel,
        "--cache-state",
        $CacheState,
        "--iterations",
        $Iterations.ToString(),
        "--sample-ms",
        $SampleMs.ToString(),
        "--progress-every",
        $ProgressEvery.ToString(),
        "--scanner",
        $Scanner
    )

    if ($FilesOnly) {
        $suiteArgs += @("--include-directories", "false")
    }

    if (-not [string]::IsNullOrWhiteSpace($CompetitorCsv)) {
        $suiteArgs += @("--competitor-csv", $CompetitorCsv)
    }

    foreach ($claimId in $Claim) {
        if (-not [string]::IsNullOrWhiteSpace($claimId)) {
            $suiteArgs += @("--claim", $claimId)
        }
    }

    & $benchExe @suiteArgs
    if ($LASTEXITCODE -ne 0) {
        throw "diskloom-bench suite failed with exit code $LASTEXITCODE"
    }

    Write-Host "Benchmark bundle: $outputDir"
    Write-Host "Report: $(Join-Path $outputDir "report.md")"
    Write-Host "Audit: $(Join-Path $outputDir "audit.csv")"
    Write-Host "Same-machine comparison CSV: $(Join-Path $outputDir "same-machine-comparison.csv")"
    Write-Host "Public comparison CSV: $(Join-Path $outputDir "public-comparison.csv")"
}
finally {
    Pop-Location
}
