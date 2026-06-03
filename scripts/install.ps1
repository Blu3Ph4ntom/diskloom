param(
    [string]$SourceRoot = $PSScriptRoot,
    [string]$InstallDir = "$env:ProgramFiles\DiskLoom"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Join-QuotedArgument {
    param([string[]]$Arguments)
    return ($Arguments | ForEach-Object { '"' + ($_ -replace '"', '\"') + '"' }) -join " "
}

if (-not (Test-Administrator)) {
    $args = @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-File", $PSCommandPath,
        "-SourceRoot", ([System.IO.Path]::GetFullPath($SourceRoot)),
        "-InstallDir", $InstallDir
    )
    Start-Process -FilePath "powershell.exe" -ArgumentList (Join-QuotedArgument $args) -Verb RunAs
    exit 0
}

$sourceRootPath = [System.IO.Path]::GetFullPath($SourceRoot)
$installDirPath = [System.IO.Path]::GetFullPath($InstallDir)
$diskloomExe = Join-Path $sourceRootPath "diskloom.exe"
$dlmExe = Join-Path $sourceRootPath "dlm.exe"

if (-not (Test-Path -LiteralPath $diskloomExe)) {
    throw "Missing diskloom.exe in $sourceRootPath"
}
if (-not (Test-Path -LiteralPath $dlmExe)) {
    throw "Missing dlm.exe in $sourceRootPath"
}

New-Item -ItemType Directory -Path $installDirPath -Force | Out-Null
Copy-Item -LiteralPath $diskloomExe -Destination $installDirPath -Force
Copy-Item -LiteralPath $dlmExe -Destination $installDirPath -Force

foreach ($name in @("README.md", "LICENSE-MIT")) {
    $source = Join-Path $sourceRootPath $name
    if (Test-Path -LiteralPath $source) {
        Copy-Item -LiteralPath $source -Destination $installDirPath -Force
    }
}

$sourceDocs = Join-Path $sourceRootPath "docs"
if (Test-Path -LiteralPath $sourceDocs) {
    Copy-Item -LiteralPath $sourceDocs -Destination $installDirPath -Recurse -Force
}

$pathKey = "HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
$machinePath = (Get-ItemProperty -Path $pathKey -Name Path).Path
$pathParts = $machinePath -split ";" | Where-Object { $_.Trim().Length -gt 0 }
if (-not ($pathParts | Where-Object { $_.TrimEnd("\") -ieq $installDirPath.TrimEnd("\") })) {
    $nextPath = ($pathParts + $installDirPath) -join ";"
    Set-ItemProperty -Path $pathKey -Name Path -Value $nextPath
    [Environment]::SetEnvironmentVariable("Path", $nextPath, "Machine")
}

$programs = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs"
New-Item -ItemType Directory -Path $programs -Force | Out-Null
$shortcutPath = Join-Path $programs "DiskLoom.lnk"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = Join-Path $installDirPath "diskloom.exe"
$shortcut.WorkingDirectory = $installDirPath
$shortcut.Description = "DiskLoom"
$shortcut.Save()

Write-Host "DiskLoom installed to $installDirPath"
Write-Host "CLI installed as dlm.exe and added to the machine PATH."
