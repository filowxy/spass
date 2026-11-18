# Verifies the runner does not hold the caller's output handles.

[CmdletBinding()]
param(
    # The idle window the runner is given. It has to be long enough that a
    # runner that holds the caller's pipe cannot be mistaken for one that
    # does not.
    [int] $IdleSecs = 45
)

$ErrorActionPreference = 'Stop'
# The script lives in <repo>/compiler/scripts, so the repo root is three
# levels up: scripts -> compiler -> repo.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Spass = Join-Path $RepoRoot 'compiler\target\release\spass.exe'
$Project = Join-Path $RepoRoot 'examples\await'

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

$env:SPASS_DAEMON_IDLE_SECS = [string]$IdleSecs

function Stop-Runner {
    & $Spass daemon --stop 2>&1 | Out-Null
    Get-Process spass -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

# Run the CLI with its output on a pipe and read that pipe to the end.
# The elapsed time is what tells the two situations apart: a runner that
# inherited the pipe holds the read open until it exits.
function Invoke-Piped {
    param([string[]] $ArgList, [string] $WorkingDir)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Spass
    $psi.Arguments = (($ArgList | ForEach-Object { if ($_ -match '[\s"]') { '"' + $_ + '"' } else { $_ } }) -join ' ')
    $psi.WorkingDirectory = $WorkingDir
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = [System.Diagnostics.Process]::Start($psi)
    $out = $proc.StandardOutput.ReadToEndAsync()
    $err = $proc.StandardError.ReadToEndAsync()
    $proc.WaitForExit()
    $text = $out.Result + $err.Result
    $sw.Stop()
    return [pscustomobject]@{ ExitCode = $proc.ExitCode; Output = $text; Milliseconds = $sw.ElapsedMilliseconds }
}

$fail = 0

Write-Host '==> the runner must not hold the caller output open' -ForegroundColor Cyan
Stop-Runner
$call = Invoke-Piped -ArgList @('run', '--direct', 'driver') -WorkingDir $Project
$limit = ($IdleSecs * 1000) / 2
if ($call.ExitCode -ne 0) { Write-Host $call.Output -ForegroundColor Red; exit 1 }
if (-not ($call.Output -match '1\.5')) {
    Write-Host ("    FAIL the call did not produce the channel value: {0}" -f $call.Output.Trim()) -ForegroundColor Red
    $fail++
} else {
    Write-Host ("    ok   the channel value arrived through the runner: 1.5") -ForegroundColor Green
}
if ($call.Milliseconds -lt $limit) {
    Write-Host ("    ok   the call returned in {0} ms (the runner idles for {1} s)" -f $call.Milliseconds, $IdleSecs) -ForegroundColor Green
} else {
    Write-Host ("    FAIL the call took {0} ms - the runner kept the caller output open" -f $call.Milliseconds) -ForegroundColor Red
    $fail++
}

Write-Host '==> the runner outlives the call and answers' -ForegroundColor Cyan
$status = & $Spass daemon --status 2>&1 | Out-String
if ($status -match 'not running') {
    Write-Host '    FAIL the runner is gone' -ForegroundColor Red
    $fail++
} else {
    Write-Host ("    ok   {0}" -f $status.Trim()) -ForegroundColor Green
}

Write-Host '==> the runner stops on request' -ForegroundColor Cyan
& $Spass daemon --stop 2>&1 | Out-Null
Start-Sleep -Milliseconds 500
$left = Get-Process spass -ErrorAction SilentlyContinue
if ($left) {
    Write-Host '    FAIL the runner is still running' -ForegroundColor Red
    $fail++
} else {
    Write-Host '    ok   the runner stopped' -ForegroundColor Green
}

Write-Host '==> the runner stops by itself when it goes idle' -ForegroundColor Cyan
$env:SPASS_DAEMON_IDLE_SECS = '3'
Stop-Runner
# A project without channels needs no runner, so start one explicitly.
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $Spass
$psi.Arguments = 'daemon'
$psi.WorkingDirectory = $Project
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$runner = [System.Diagnostics.Process]::Start($psi)
Start-Sleep -Seconds 6
if ($runner.HasExited) {
    Write-Host '    ok   the runner exited when it went idle' -ForegroundColor Green
} else {
    Write-Host '    FAIL the runner is still running after its idle window' -ForegroundColor Red
    $fail++
    try { $runner.Kill() } catch { }
}

Remove-Item Env:\SPASS_DAEMON_IDLE_SECS -ErrorAction SilentlyContinue
Stop-Runner

if ($fail -gt 0) { Write-Host "$fail runner check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host 'runner verified' -ForegroundColor Green
