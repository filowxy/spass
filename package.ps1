# Packages the release toolchain into a single self-contained installer.

[CmdletBinding()]
param([switch] $SkipTests)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$Compiler = Join-Path $RepoRoot 'compiler'
$Dist = Join-Path $RepoRoot 'dist'
$Setup = Join-Path $Dist 'spass-setup.exe'

function Step { param($m) Write-Host "==> $m" -ForegroundColor Cyan }
function Ok   { param($m) Write-Host "    $m" -ForegroundColor Green }

# cargo is installed per-user and is often absent from the session PATH.
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
        $env:PATH = "$cargoBin;$env:PATH"
    } else {
        Write-Host 'error: cargo not found. Install Rust from https://rustup.rs' -ForegroundColor Red
        exit 1
    }
}

Push-Location $Compiler
try {
    if (-not $SkipTests) {
        Step 'Running tests'
        & cargo test --workspace
        if ($LASTEXITCODE -ne 0) { Write-Host 'error: tests failed' -ForegroundColor Red; exit 1 }
        Ok 'tests passed'
    }

    # 1. The compiler, in release. Must be first: the installer embeds it.
    Step 'Building spass (release)'
    & cargo build --release -p spass
    if ($LASTEXITCODE -ne 0) { Write-Host 'error: release build failed' -ForegroundColor Red; exit 1 }
    Ok 'spass.exe'

    # 2. The installer, which embeds the payload from step 1.
    Step 'Building spass-setup'
    & cargo build --release -p spass-installer
    if ($LASTEXITCODE -ne 0) { Write-Host 'error: installer build failed' -ForegroundColor Red; exit 1 }
} finally {
    Pop-Location
}

$built = Join-Path $Compiler 'target\release\spass-setup.exe'
if (-not (Test-Path $built)) { Write-Host "error: $built not found" -ForegroundColor Red; exit 1 }

Step 'Packaging'
New-Item -ItemType Directory -Path $Dist -Force | Out-Null
Copy-Item $built $Setup -Force
Ok $Setup

$size = [math]::Round((Get-Item $Setup).Length / 1MB, 2)
Write-Host ''
Write-Host "Packaged spass-setup.exe ($size MB)" -ForegroundColor Green
Write-Host '  install : spass-setup.exe' -ForegroundColor Gray
Write-Host '  remove  : spass-setup.exe --uninstall' -ForegroundColor Gray
Write-Host ''
