# Measures wall-clock time of the full `spass run` pipeline.

[CmdletBinding()]
param(
    [int]    $Runs      = 3,
    [string] $Project   = '',
    [string] $Func      = '',
    [string] $FuncArgs  = '0.5 1.5',
    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
# The script lives in <repo>/compiler/scripts, so the repo root is three
# levels up: scripts -> compiler -> repo.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Compiler = Join-Path $RepoRoot 'compiler'
$Spass    = Join-Path $Compiler 'target\release\spass.exe'

function Step { param($m) Write-Host "==> $m" -ForegroundColor Cyan }
function Ok   { param($m) Write-Host "    $m" -ForegroundColor Green }

# The trailing arguments are one string on the command line but several
# arguments to `spass run`.
function Split-Args { param([string] $Text) @($Text -split '\s+' | Where-Object { $_ -ne '' }) }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path (Join-Path $cargoBin 'cargo.exe')) { $env:PATH = "$cargoBin;$env:PATH" }
}

# spass.exe links against the toolchain's libunwind.dll, which is not on
# PATH by default - without it spass.exe fails to start with
# STATUS_DLL_NOT_FOUND and every measurement would time a failed launch.
$unwind = Get-ChildItem -Path (Join-Path $env:USERPROFILE '.rustup\toolchains') -Recurse -Filter 'libunwind.dll' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\bin\\' } | Select-Object -First 1
if ($unwind) {
    $unwindDir = Split-Path -Parent $unwind.FullName
    if ($env:PATH -notlike "*$unwindDir*") { $env:PATH = "$unwindDir;$env:PATH" }
}

if (-not $SkipBuild) {
    Step 'Building spass (release)'
    Push-Location $Compiler
    try {
        & cargo build --release -p spass
        if ($LASTEXITCODE -ne 0) { Write-Host 'error: release build failed' -ForegroundColor Red; exit 1 }
    } finally { Pop-Location }
    Ok $Spass
}
if (-not (Test-Path $Spass)) { Write-Host "error: $Spass not found" -ForegroundColor Red; exit 1 }

# The project set to measure. Each entry is a path to an example project
# and the function to call.
$targets = if ($Project) {
    @(@{ Path = $Project; Func = $Func })
} else {
    @(
        @{ Path = (Join-Path $RepoRoot 'examples\terrain');  Func = 'height' },
        @{ Path = (Join-Path $RepoRoot 'examples\bootstrap'); Func = 'self_check' }
    )
}

function Invoke-Timed {
    # Run one `spass run` and return the elapsed milliseconds.
    #
    # A failed invocation is reported, not measured: the time of a process
    # that never started is not a measurement of anything.
    param([string] $Dir, [string[]] $ArgList)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Push-Location $Dir
    try {
        $out = & $Spass @ArgList 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally { Pop-Location }
    $sw.Stop()
    if ($code -ne 0) {
        throw ("spass {0} failed in {1} (exit {2}):`n{3}" -f ($ArgList -join ' '), $Dir, $code, $out)
    }
    return $sw.Elapsed.TotalMilliseconds
}

function Measure-Cold {
    # A cold run: the recorded build inputs no longer match the sources, so
    # the compiler has to re-parse, re-analyse, re-emit and re-compile the
    # module with `rustc`.
    #
    # The cache is invalidated by touching the source mtimes rather than
    # by deleting the cache file: the build inputs are keyed on mtime and
    # size, so a newer mtime is a miss. Touching needs no delete rights.
    param([string] $Dir, [string] $Fn)
    Get-ChildItem -Path (Join-Path $Dir 'src') -Recurse -Filter '*.spass' -ErrorAction SilentlyContinue |
        ForEach-Object { $_.LastWriteTime = Get-Date }
    return Invoke-Timed -Dir $Dir -ArgList (@('run', '--direct', $Fn) + (Split-Args $FuncArgs))
}

function Measure-Warm {
    # A warm run: the cache hit skips the re-emit and the `rustc` call.
    param([string] $Dir, [string] $Fn)
    return Invoke-Timed -Dir $Dir -ArgList (@('run', '--direct', $Fn) + (Split-Args $FuncArgs))
}

function Measure-Daemon {
    # A daemon-backed run: the module is already compiled and cached in
    # the runner, so only the dispatch and the program run are measured.
    param([string] $Dir, [string] $Fn)
    return Invoke-Timed -Dir $Dir -ArgList (@('run', $Fn) + (Split-Args $FuncArgs))
}

function Median { param([double[]] $xs) $s = $xs | Sort-Object; $s[[int][math]::Floor($s.Count / 2)] }

$rows = @()
foreach ($t in $targets) {
    if (-not $t.Path) { continue }
    if (-not (Test-Path $t.Path)) { Write-Host "skip: $($t.Path) not found" -ForegroundColor Yellow; continue }
    $fn = if ($t.Func) { $t.Func } else { 'height' }

    # Work on a copy so the example tree keeps its own build state.
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("spass-measure-" + [System.Guid]::NewGuid().ToString('N').Substring(0,8))
    New-Item -ItemType Directory -Path $work -Force | Out-Null
    Copy-Item -Path (Join-Path $t.Path '*') -Destination $work -Recurse -Force
    if (Test-Path (Join-Path $work 'build')) { Remove-Item -LiteralPath (Join-Path $work 'build') -Recurse -Force -ErrorAction SilentlyContinue }

    Step ("{0} -> {1}" -f (Split-Path -Leaf $t.Path), $fn)

    $cold = @(); $warm = @(); $daemon = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        # The cold measurement touches the sources, which invalidates the
        # daemon's cache entry too. The first daemon call after that has to
        # recompile, so it is run untimed and only the second one - the
        # steady state - is measured.
        $cold  += Measure-Cold  $work $fn
        $warm  += Measure-Warm  $work $fn
        Measure-Daemon $work $fn | Out-Null
        $daemon += Measure-Daemon $work $fn
    }
    $rows += [pscustomobject]@{
        Project   = Split-Path -Leaf $t.Path
        ColdMs    = [math]::Round((Median $cold),  0)
        WarmMs    = [math]::Round((Median $warm),  0)
        DaemonMs  = [math]::Round((Median $daemon), 0)
    }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host ('median of {0} run(s), milliseconds' -f $Runs) -ForegroundColor Cyan
$rows | Format-Table -AutoSize | Out-String | Write-Host
Write-Host 'Cold  = no build cache: parse + analyse + emit + rustc + run' -ForegroundColor Gray
Write-Host 'Warm  = build cache hit: dispatch + run only' -ForegroundColor Gray
Write-Host 'Daemon= runner holds the compiled module: dispatch + run only' -ForegroundColor Gray
