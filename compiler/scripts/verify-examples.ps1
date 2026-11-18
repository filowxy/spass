# Builds every example project and runs one exported function from each.

[CmdletBinding()]
param(
    [switch] $SkipBuild,
    [switch] $NoRunner,
    [int] $TimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
# The script lives in <repo>/compiler/scripts, so the repo root is three
# levels up: scripts -> compiler -> repo.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Spass = Join-Path $RepoRoot 'compiler\target\release\spass.exe'

if (-not (Test-Path $Spass)) { Write-Host "error: $Spass not found - build the compiler first" -ForegroundColor Red; exit 1 }

# spass.exe links against the toolchain's libunwind.dll. The MSI ships
# it next to spass.exe, but in the dev tree it lives in the rustup
# toolchain, which is not on PATH by default - without it spass.exe fails
# to start with STATUS_DLL_NOT_FOUND.
$unwind = Get-ChildItem -Path (Join-Path $env:USERPROFILE '.rustup\toolchains') -Recurse -Filter 'libunwind.dll' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\bin\\' } | Select-Object -First 1
if ($unwind) {
    $unwindDir = Split-Path -Parent $unwind.FullName
    if ($env:PATH -notlike "*$unwindDir*") { $env:PATH = "$unwindDir;$env:PATH" }
}

# Run a child process with a hard timeout and capture both streams.
# `ReadToEndAsync` on both pipes keeps a full pipe buffer from deadlocking
# the child while we wait for it to exit.
#
# PowerShell 5.1 runs on.NET Framework, where ProcessStartInfo has no
# ArgumentList collection - the arguments have to be joined into one
# command-line string.
function Quote-Arg {
    param([string] $Value)
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + ($Value -replace '(\\*)"', '$1$1\"' -replace '(\\*)$', '$1$1') + '"'
}

function Invoke-Captured {
    param([string] $Exe, [string[]] $ArgList, [string] $WorkingDir, [int] $TimeoutMs)

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = (($ArgList | ForEach-Object { Quote-Arg $_ }) -join ' ')
    $psi.WorkingDirectory = $WorkingDir
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false

    $proc = [System.Diagnostics.Process]::Start($psi)
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $timedOut = -not $proc.WaitForExit($TimeoutMs)
    if ($timedOut) {
        try { $proc.Kill() } catch { }
        $proc.WaitForExit(5000) | Out-Null
        return [pscustomobject]@{ ExitCode = -1; Output = "timed out after $TimeoutMs ms"; TimedOut = $true }
    }
    $text = $outTask.Result + $errTask.Result
    return [pscustomobject]@{ ExitCode = $proc.ExitCode; Output = $text; Stdout = $outTask.Result; Stderr = $errTask.Result; TimedOut = $false }
}

# The runner: the spasio channels are runner-owned, so the daemon has to be
# up for the examples that touch a channel. Start one if it is not already
# listening, and stop it again if this script started it.
$runnerUp = $false
$runnerStarted = $false
if (-not $NoRunner) {
    $status = Invoke-Captured -Exe $Spass -ArgList @('daemon', '--status') -WorkingDir $RepoRoot -TimeoutMs 30000
    $runnerUp = ($status.ExitCode -eq 0)
    if (-not $runnerUp) {
        Start-Process -FilePath $Spass -ArgumentList @('daemon') -WindowStyle Hidden | Out-Null
        Start-Sleep -Seconds 2
        $status = Invoke-Captured -Exe $Spass -ArgList @('daemon', '--status') -WorkingDir $RepoRoot -TimeoutMs 30000
        $runnerUp = ($status.ExitCode -eq 0)
        $runnerStarted = $runnerUp
    }
    if ($runnerUp) { Write-Host "runner: daemon is up" }
    else { Write-Host "runner: could not start the daemon - channel examples will fail" -ForegroundColor Yellow }
}

# Example -> the function to call and the arguments it takes.
$cases = @(
    @{ Name = 'assign';      Func = 'replace';    Args = @('1.0') }
    @{ Name = 'async';       Func = 'driver';     Args = @('1.0', '2.0') }
    @{ Name = 'await';       Func = 'driver';     Args = @() }
    @{ Name = 'bootstrap';   Func = 'self_check'; Args = @() }
    @{ Name = 'checkpoint';  Func = 'driver';     Args = @('1.0', '3.0') }
    @{ Name = 'game';        Func = '';           Args = @(); BuildOnly = $true }
    @{ Name = 'if_present';  Func = 'scale';      Args = @('2.0') }
    @{ Name = 'list';        Func = 'sum_squares'; Args = @() }
    @{ Name = 'module';      Func = 'compute_b';  Args = @('10.0') }
    @{ Name = 'parallel';    Func = 'batch_sum';  Args = @() }
    @{ Name = 'random';      Func = 'pick_float'; Args = @() }
    @{ Name = 'sequence';    Func = 'driver';     Args = @() }
    @{ Name = 'setget';      Func = 'publish';    Args = @() }
    @{ Name = 'socket';      Func = 'send_height'; Args = @('1.0') }
    @{ Name = 'struct';      Func = 'make_point'; Args = @('1.0', '2.0', '3.0') }
    @{ Name = 'terrain';     Func = 'height';     Args = @('0.5', '1.5') }
    @{ Name = 'terrain_gen'; Func = '';           Args = @(); BuildOnly = $true }
    @{ Name = 'vec';         Func = 'vdot';       Args = @('1', '2', '3', '4', '5', '6') }
    @{ Name = 'voxel';       Func = '';           Args = @(); BuildOnly = $true }
)

$timeoutMs = $TimeoutSeconds * 1000
$fail = 0
$rows = @()
foreach ($c in $cases) {
    $dir = Join-Path $RepoRoot ("examples\" + $c.Name)
    if (-not (Test-Path $dir)) { $rows += [pscustomobject]@{ Example = $c.Name; Build = 'skip'; Run = 'skip'; Note = 'no project' }; continue }

    $buildOnly = $c.ContainsKey('BuildOnly') -and $c.BuildOnly

    if ($SkipBuild) {
        $buildOk = $true
        $buildOut = ''
    } else {
        $b = Invoke-Captured -Exe $Spass -ArgList @('build') -WorkingDir $dir -TimeoutMs $timeoutMs
        $buildOk = ($b.ExitCode -eq 0)
        $buildOut = $b.Output
    }

    $runOk = $true
    $runOut = ''
    $runStdout = ''
    if ($buildOk -and -not $buildOnly) {
        $r = Invoke-Captured -Exe $Spass -ArgList (@('run', '--direct', $c.Func) + $c.Args) -WorkingDir $dir -TimeoutMs $timeoutMs
        $runOk = ($r.ExitCode -eq 0)
        $runOut = $r.Output
        $runStdout = $r.Stdout
    } elseif ($buildOnly) {
        $runOut = 'build-only (needs an external peer)'
    }

    $note = ''
    if (-not $buildOk) {
        $note = (($buildOut -split "`n" | Where-Object { $_ -match 'error' } | Select-Object -First 1) -join '').Trim()
        if ($note -eq '') { $note = (($buildOut -split "`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -Last 1) -join '').Trim() }
    } elseif (-not $runOk) {
        $note = (($runOut -split "`n" | Where-Object { $_ -match 'error|panic' } | Select-Object -First 1) -join '').Trim()
        if ($note -eq '') { $note = (($runOut -split "`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -Last 1) -join '').Trim() }
    } else {
        # The function's return value is on stdout; the progress readout
        # is on stderr, and the two streams interleave when they are
        # merged. So the value comes from stdout, not from the tail of
        # the merged text.
        $note = (($runStdout -split "`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -Last 1) -join '').Trim()
        if ($note -eq '') { $note = (($runOut -split "`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -Last 1) -join '').Trim() }
    }

    if (-not $buildOk -or -not $runOk) { $fail++ }
    $rows += [pscustomobject]@{
        Example = $c.Name
        Build   = if ($buildOk) { 'ok' } else { 'FAIL' }
        Run     = if ($buildOnly) { 'n/a' } elseif ($runOk) { 'ok' } else { 'FAIL' }
        Note    = $note
    }
}

if ($runnerStarted) {
    Invoke-Captured -Exe $Spass -ArgList @('daemon', '--stop') -WorkingDir $RepoRoot -TimeoutMs 30000 | Out-Null
}

$built = ($rows | Where-Object { $_.Build -eq 'ok' }).Count
$ran = ($rows | Where-Object { $_.Run -eq 'ok' }).Count
$buildOnly = ($rows | Where-Object { $_.Run -eq 'n/a' }).Count
$rows | Format-Table -AutoSize -Wrap | Out-String | Write-Host
if ($fail -gt 0) { Write-Host "$fail example(s) failed" -ForegroundColor Red; exit 1 }
Write-Host "$built/$($rows.Count) examples built, $ran ran, $buildOnly are build-only" -ForegroundColor Green
