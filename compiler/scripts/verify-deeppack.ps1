# Verifies the deeppack library by loading it in-process and calling its exports.

[CmdletBinding()]
param(
    [string] $Project = ''
)

$ErrorActionPreference = 'Stop'
# The script lives in <repo>/compiler/scripts, so the repo root is three
# levels up: scripts -> compiler -> repo.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Spass = Join-Path $RepoRoot 'compiler\target\release\spass.exe'
if (-not $Project) { $Project = Join-Path $RepoRoot 'examples\terrain' }

if (-not (Test-Path $Spass)) { Write-Host "error: $Spass not found - build the compiler first" -ForegroundColor Red; exit 1 }
if (-not (Test-Path $Project)) { Write-Host "error: $Project not found" -ForegroundColor Red; exit 1 }

# spass.exe links against the toolchain's libunwind.dll, which is not
# on PATH by default.
$unwind = Get-ChildItem -Path (Join-Path $env:USERPROFILE '.rustup\toolchains') -Recurse -Filter 'libunwind.dll' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\bin\\' } | Select-Object -First 1
if ($unwind) {
    $unwindDir = Split-Path -Parent $unwind.FullName
    if ($env:PATH -notlike "*$unwindDir*") { $env:PATH = "$unwindDir;$env:PATH" }
}

function Invoke-Captured {
    param([string] $Exe, [string[]] $ArgList, [string] $WorkingDir, [int] $TimeoutMs = 600000)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = (($ArgList | ForEach-Object { if ($_ -match '[\s"]') { '"' + $_ + '"' } else { $_ } }) -join ' ')
    $psi.WorkingDirectory = $WorkingDir
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $proc = [System.Diagnostics.Process]::Start($psi)
    $out = $proc.StandardOutput.ReadToEndAsync()
    $err = $proc.StandardError.ReadToEndAsync()
    if (-not $proc.WaitForExit($TimeoutMs)) {
        try { $proc.Kill() } catch { }
        throw "spass $($ArgList -join ' ') timed out"
    }
    return [pscustomobject]@{ ExitCode = $proc.ExitCode; Output = $out.Result + $err.Result }
}

Write-Host '==> building with --deeppack' -ForegroundColor Cyan
$b = Invoke-Captured -Exe $Spass -ArgList @('build', '--deeppack') -WorkingDir $Project
if ($b.ExitCode -ne 0) { Write-Host $b.Output -ForegroundColor Red; exit 1 }

$build = Join-Path $Project 'build'
$lib = Join-Path $build 'spass_module.dll'
$manifest = Join-Path $build 'spass_module.abi.json'
if (-not (Test-Path $lib)) { Write-Host "error: $lib not found" -ForegroundColor Red; exit 1 }
if (-not (Test-Path $manifest)) { Write-Host "error: $manifest not found" -ForegroundColor Red; exit 1 }

$m = Get-Content $manifest -Raw | ConvertFrom-Json
Write-Host ("    artifact: {0} ({1} bytes), requires: {2}" -f $m.artifact, (Get-Item $lib).Length, ($m.requires -join ', '))

# The symbols the checks need. They come from the manifest, so the test
# follows the compiler's own description of the ABI.
$height = $m.functions | Where-Object { $_.name -eq 'height' } | Select-Object -First 1
$quantise = $m.functions | Where-Object { $_.name -eq 'quantise' } | Select-Object -First 1
$split = $m.functions | Where-Object { $_.name -eq 'split_value' } | Select-Object -First 1
if (-not $height -or -not $quantise -or -not $split) {
    Write-Host 'error: the example project does not have the functions the checks need' -ForegroundColor Red
    exit 1
}

# The host. It knows nothing about Spass except the manifest and the
# symbol names.
$hostSource = @"
using System;
using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)]
public struct SpassRet2 { public double _f0; public double _f1; }
public static class SpassDeeppackHost {
    [DllImport(@"$lib", CallingConvention = CallingConvention.Cdecl)]
    public static extern double $($height.symbol)(double a, double b);
    [DllImport(@"$lib", CallingConvention = CallingConvention.Cdecl)]
    public static extern byte $($quantise.symbol)(byte x);
    [DllImport(@"$lib", CallingConvention = CallingConvention.Cdecl)]
    public static extern SpassRet2 $($split.symbol)(double x);
}
"@
Add-Type -TypeDefinition $hostSource

$fail = 0
function Check($what, $got, $want, $tolerance) {
    if ([math]::Abs($got - $want) -le $tolerance) {
        Write-Host ("    ok   {0,-34} {1}" -f $what, $got) -ForegroundColor Green
    } else {
        Write-Host ("    FAIL {0,-34} got {1}, want {2}" -f $what, $got, $want) -ForegroundColor Red
        $script:fail++
    }
}

Write-Host '==> calling the library from the host' -ForegroundColor Cyan

# 1. A plain function. The reference value is what the same module
# produces when it is run as a program.
$reference = Invoke-Captured -Exe $Spass -ArgList @('run', '--direct', 'height', '0.5', '1.5') -WorkingDir $Project
$referenceValue = 0.0
foreach ($line in ($reference.Output -split "`n")) {
    $t = $line.Trim()
    $v = 0.0
    if ([double]::TryParse($t, [ref]$v)) { $referenceValue = $v }
}
$got = [SpassDeeppackHost]::$($height.symbol)(0.5, 1.5)
Check 'height(0.5, 1.5)' $got $referenceValue 1e-12

# 2. A value-domain function: the parameter and the return cross the
# boundary as encoded state indices. State 2 of float[0.0, 1.0, 0.5]
# decodes to 1.0.
$state = [SpassDeeppackHost]::$($quantise.symbol)([byte]2)
Check 'quantise(state 2) -> state' $state 2 0

# 3. A multi-return function: the result is a repr(C) struct.
$pair = [SpassDeeppackHost]::$($split.symbol)(3.5)
Check 'split_value(3.5)._f0' $pair._f0 3.5 1e-12
Check 'split_value(3.5)._f1' $pair._f1 7.0 1e-12

if ($fail -gt 0) { Write-Host "$fail deeppack check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host 'deeppack verified' -ForegroundColor Green
