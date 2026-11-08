#Requires -Version 5.1
<#
.SYNOPSIS
    Checks build prerequisites and builds Teams Chat Exporter (release).
    Output: a date-versioned executable and a latest-name copy under .\bin.
#>
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function Fail($msg) { Write-Host "`n$msg" -ForegroundColor Red; exit 1 }

# Rust
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail "Rust is not installed (or PowerShell needs reopening).`n  winget install --id Rustlang.Rustup -e`n  then reopen PowerShell and run: rustup default stable-msvc"
}
$ver = (& rustc --version) -replace '^rustc (\d+\.\d+).*', '$1'
if ([version]$ver -lt [version]'1.95') {
    Write-Host "Rust $ver is too old - updating..." -ForegroundColor Yellow
    rustup update stable
}
$host_triple = (& rustc -vV | Select-String '^host:').ToString().Split(' ')[1]
if ($host_triple -notlike '*msvc*') {
    Write-Host "Switching to the MSVC toolchain (current: $host_triple)..." -ForegroundColor Yellow
    rustup default stable-msvc
}

# MSVC C++ build tools
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$hasVc = $false
if (Test-Path $vswhere) {
    $hasVc = [bool](& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
}
if (-not $hasVc) {
    Fail "Visual Studio C++ Build Tools not found. Install with:`n  winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override `"--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended`""
}

Write-Host "Building (first build takes 5-10 minutes)..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { Fail 'Build failed - see errors above.' }

$versionMatch = [regex]::Match((Get-Content .\Cargo.toml -Raw), '(?m)^version\s*=\s*"(\d{4})\.(\d{1,2})\.(\d{1,2})"\s*$')
if (-not $versionMatch.Success) { Fail 'Cargo.toml version must use YYYY.M.D format, for example 2026.11.6.' }
$dateVersion = '{0}{1}{2}' -f $versionMatch.Groups[1].Value, $versionMatch.Groups[2].Value.PadLeft(2, '0'), $versionMatch.Groups[3].Value.PadLeft(2, '0')
$binDir = Join-Path $PSScriptRoot 'bin'
New-Item -ItemType Directory -Path $binDir -Force | Out-Null
$latestExe = Join-Path $binDir 'teams-chat-exporter.exe'
$versionedExe = Join-Path $binDir "teams-chat-exporter-$dateVersion.exe"
Copy-Item .\target\release\teams-chat-exporter.exe $versionedExe -Force
try {
    Copy-Item .\target\release\teams-chat-exporter.exe $latestExe -Force
} catch {
    Write-Warning "Could not update $latestExe. Close the running app and rerun the build script to refresh the latest-name copy."
}
$size = [math]::Round((Get-Item $versionedExe).Length / 1MB, 1)
Write-Host "`nDone: $versionedExe ($size MB)" -ForegroundColor Green
