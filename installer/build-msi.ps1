# Builds the Spass MSI installer with WiX.

[CmdletBinding()]
param(
    [switch] $SkipBuild,
    [string] $Version = ''
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Compiler = Join-Path $RepoRoot 'compiler'
$Dist     = Join-Path $RepoRoot 'dist'
$Payload  = Join-Path $Dist 'msi-payload'
$Wxs      = Join-Path $RepoRoot 'installer\spass.wxs'

function Step { param($m) Write-Host "==> $m" -ForegroundColor Cyan }
function Ok   { param($m) Write-Host "    $m" -ForegroundColor Green }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path (Join-Path $cargoBin 'cargo.exe')) { $env:PATH = "$cargoBin;$env:PATH" }
}

# The WiX CLI installs as a dotnet global tool.
$wix = Join-Path $env:USERPROFILE '.dotnet\tools\wix.exe'
if (-not (Test-Path $wix)) {
    $wixCmd = Get-Command wix -ErrorAction SilentlyContinue
    if ($wixCmd) { $wix = $wixCmd.Source }
}
if (-not (Test-Path $wix)) {
    Write-Host 'error: the WiX CLI was not found.' -ForegroundColor Red
    Write-Host '  dotnet tool install --global wix --version 5.0.2' -ForegroundColor Red
    exit 1
}

# --- 1. The compiler, in release. The MSI ships these binaries. --------
if (-not $SkipBuild) {
    Step 'Building spass (release)'
    Push-Location $Compiler
    try {
        & cargo build --release -p spass
        if ($LASTEXITCODE -ne 0) { Write-Host 'error: release build failed' -ForegroundColor Red; exit 1 }
    } finally { Pop-Location }
}

$Release = Join-Path $Compiler 'target\release'
$SpassExe = Join-Path $Release 'spass.exe'
if (-not (Test-Path $SpassExe)) {
    Write-Host "error: $SpassExe not found - build the compiler first" -ForegroundColor Red
    exit 1
}

if (-not $Version) {
    # The workspace holds the single source of truth for the version.
    $match = Select-String -Path (Join-Path $Compiler 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
             Select-Object -First 1
    if (-not $match) { Write-Host 'error: could not read the product version' -ForegroundColor Red; exit 1 }
    $Version = $match.Matches[0].Groups[1].Value
}

# --- 2. Stage the payload ----------------------------------------------
Step 'Staging the payload'
New-Item -ItemType Directory -Path $Payload -Force | Out-Null
Copy-Item $SpassExe (Join-Path $Payload 'spass.exe') -Force
Ok 'spass.exe'

# A stale copy from an earlier run must not end up in the package, so an
# optional payload is removed when its source is gone.
function Stage-Optional {
    param([string] $Name, [string] $Source)
    $dest = Join-Path $Payload $Name
    if (Test-Path $Source) {
        Copy-Item $Source $dest -Force
        Ok $Name
    } elseif (Test-Path $dest) {
        Remove-Item -LiteralPath $dest -Force
    }
}

# `libunwind.dll` sits under <toolchain>/lib/rustlib/<triple>/bin. The
# gnullvm target links against it and it is not on PATH for a machine
# that only has Spass, so an install without it produces a spass.exe
# that dies before main with STATUS_DLL_NOT_FOUND.
function Find-LibUnwind {
    $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
    $root = Join-Path $rustupHome 'toolchains'
    if (-not (Test-Path $root)) { return $null }
    $best = $null
    Get-ChildItem $root -Recurse -Filter 'libunwind.dll' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like '*rustlib*' } |
        ForEach-Object {
            if ($null -eq $best -or $_.LastWriteTime -gt $best.LastWriteTime) { $best = $_ }
        }
    return $best
}
$unwind = Find-LibUnwind
if ($unwind) {
    Stage-Optional 'libunwind.dll' $unwind.FullName
} else {
    Write-Host '    libunwind.dll not found - spass.exe needs the Rust toolchain bin dir on PATH' -ForegroundColor Yellow
    if (Test-Path (Join-Path $Payload 'libunwind.dll')) { Remove-Item -LiteralPath (Join-Path $Payload 'libunwind.dll') -Force }
}

$Bootstrap = Join-Path $RepoRoot 'examples\bootstrap\src\bootstrap.spass'
if (Test-Path $Bootstrap) {
    New-Item -ItemType Directory -Path (Join-Path $Payload 'bootstrap') -Force | Out-Null
    Copy-Item $Bootstrap (Join-Path $Payload 'bootstrap\bootstrap.spass') -Force
    Ok 'bootstrap\bootstrap.spass'
}

# --- 3. Build the MSI --------------------------------------------------
Step 'Building the MSI'
New-Item -ItemType Directory -Path $Dist -Force | Out-Null
$Out = Join-Path $Dist "spass-$Version-x64.msi"

# A stable ProductCode per version lets Windows Installer recognise an
# upgrade of the same build instead of installing it side by side.
$ProductCode = '6B2D8F41-3C57-4A92-B0E6-71D4C8E5F309'

& $wix build -nologo -arch x64 `
    -d "ProductVersion=$Version" `
    -d "ProductCode=$ProductCode" `
    -d "PayloadDir=$Payload" `
    -o $Out $Wxs
if ($LASTEXITCODE -ne 0) { Write-Host 'error: the MSI build failed' -ForegroundColor Red; exit 1 }

$size = [math]::Round((Get-Item $Out).Length / 1MB, 2)
Write-Host ''
Write-Host "Built $Out ($size MB)" -ForegroundColor Green
Write-Host '  install : msiexec /i spass-<version>-x64.msi' -ForegroundColor Gray
Write-Host '  remove  : msiexec /x spass-<version>-x64.msi' -ForegroundColor Gray
# A rebuilt MSI keeps the product code but gets a new package code, so
# `msiexec /x <msi path>` no longer matches an install that came from an
# earlier build of the same file name. The product code always does.
Write-Host "  remove (any build) : msiexec /x {$ProductCode}" -ForegroundColor Gray
Write-Host ''
