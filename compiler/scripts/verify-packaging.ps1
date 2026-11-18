# Verifies the packaging modes produce equivalent exports.

[CmdletBinding()]
param(
    [string] $Project = '',
    [int] $TimeoutSeconds = 300
)

$ErrorActionPreference = 'Stop'
# The script lives in <repo>/compiler/scripts, so the repo root is three
# levels up: scripts -> compiler -> repo.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Spass = Join-Path $RepoRoot 'compiler\target\release\spass.exe'
if (-not $Project) { $Project = Join-Path $RepoRoot 'examples\terrain' }

if (-not (Test-Path $Spass)) { Write-Host "error: $Spass not found - build the compiler first" -ForegroundColor Red; exit 1 }
if (-not (Test-Path $Project)) { Write-Host "error: $Project not found" -ForegroundColor Red; exit 1 }

# spass.exe links against the toolchain's libunwind.dll, which is not on
# PATH by default.
$unwind = Get-ChildItem -Path (Join-Path $env:USERPROFILE '.rustup\toolchains') -Recurse -Filter 'libunwind.dll' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\bin\\' } | Select-Object -First 1
if ($unwind) {
    $unwindDir = Split-Path -Parent $unwind.FullName
    if ($env:PATH -notlike "*$unwindDir*") { $env:PATH = "$unwindDir;$env:PATH" }
}

# Run a child process with a hard timeout. The two streams stay apart:
# the numbers a caller sees are on stdout, the progress readout is on
# stderr, and mixing them would put the readout's numbers into the
# comparison.
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
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    if (-not $proc.WaitForExit($TimeoutMs)) {
        try { $proc.Kill() } catch { }
        throw "spass $($ArgList -join ' ') timed out"
    }
    return [pscustomobject]@{
        ExitCode = $proc.ExitCode
        Stdout   = $outTask.Result
        Stderr   = $errTask.Result
    }
}

# The numbers in a piece of output.
#
# A digit that belongs to an identifier is not a number: the multi-return
# readout is `SpassRet2 { _f0: 3.0, _f1: 6.0 }`, and the `0` of `_f0` must
# not be counted as a result.
function Get-Numbers {
    param([string] $Text)
    $found = @()
    foreach ($m in [regex]::Matches($Text, '(?<![\w.])-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?')) {
        $v = 0.0
        if ([double]::TryParse($m.Value, [ref]$v)) { $found += $v }
    }
    return $found
}

# The host for the deeppack mode.
#
# It loads the library into this process and calls the module's uniform
# entry point, so one host serves every signature: the manifest says
# which symbol to call and how many result slots it fills.
$hostSource = @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class SpassPackagingHost {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadLibraryW(string path);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Ansi)]
    private static extern IntPtr GetProcAddress(IntPtr module, string name);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FreeLibrary(IntPtr module);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int StartFn();

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate void WaitFn();

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int CallFn(IntPtr symbol, UIntPtr symbolLen, double[] args, UIntPtr argc, double[] outs, UIntPtr outCap);

    /// The digest of one entry of a zip package. The package and the
    /// library have to be the same bytes, and a digest says that without
    /// reading four megabytes twice.
    public static string HashEntry(string zipPath, string entryName) {
        using (var zip = System.IO.Compression.ZipFile.OpenRead(zipPath)) {
            foreach (var entry in zip.Entries) {
                if (entry.FullName == entryName) {
                    using (var s = entry.Open())
                    using (var sha = System.Security.Cryptography.SHA256.Create()) {
                        return BitConverter.ToString(sha.ComputeHash(s));
                    }
                }
            }
        }
        return null;
    }

    /// Load the module, call one function, join its sequences and unload
    /// it again. The join happens even when the call fails: unloading a
    /// library whose sequence threads are still running crashes the
    /// process.
    public static double[] Call(string dll, string symbol, double[] args, int outSlots) {
        IntPtr h = LoadLibraryW(dll);
        if (h == IntPtr.Zero) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        try {
            IntPtr startPtr = GetProcAddress(h, "spass_module_start");
            if (startPtr != IntPtr.Zero) {
                int code = ((StartFn)Marshal.GetDelegateForFunctionPointer(startPtr, typeof(StartFn)))();
                if (code != 0) { throw new InvalidOperationException("the module refused to start: " + code); }
            }
            IntPtr callPtr = GetProcAddress(h, "spass_module_call");
            if (callPtr == IntPtr.Zero) { throw new InvalidOperationException("the module has no spass_module_call"); }
            CallFn call = (CallFn)Marshal.GetDelegateForFunctionPointer(callPtr, typeof(CallFn));

            byte[] nameBytes = Encoding.UTF8.GetBytes(symbol);
            IntPtr namePtr = Marshal.AllocHGlobal(Math.Max(nameBytes.Length, 1));
            try {
                Marshal.Copy(nameBytes, 0, namePtr, nameBytes.Length);
                // The module checks the pointers for null, so an empty
                // list still needs a real array to point at.
                double[] argArray = args.Length > 0 ? args : new double[1];
                double[] outArray = new double[Math.Max(outSlots, 1)];
                int rc = call(namePtr, (UIntPtr)nameBytes.Length, argArray, (UIntPtr)args.Length,
                              outArray, (UIntPtr)outSlots);
                if (rc != 0) { throw new InvalidOperationException("the call failed: " + rc); }
                double[] results = new double[outSlots];
                Array.Copy(outArray, results, outSlots);
                return results;
            } finally {
                Marshal.FreeHGlobal(namePtr);
            }
        } finally {
            IntPtr waitPtr = GetProcAddress(h, "spass_module_wait");
            if (waitPtr != IntPtr.Zero) {
                ((WaitFn)Marshal.GetDelegateForFunctionPointer(waitPtr, typeof(WaitFn)))();
            }
            FreeLibrary(h);
        }
    }
}
"@
Add-Type -TypeDefinition $hostSource -ReferencedAssemblies 'System.IO.Compression.dll', 'System.IO.Compression.FileSystem.dll'

$timeoutMs = $TimeoutSeconds * 1000
$fail = 0

function Fail {
    param([string] $Message)
    Write-Host ("    FAIL {0}" -f $Message) -ForegroundColor Red
    $script:fail++
}

# --- 1. Build the project once. The package and the library come out of
# the same build, which is the point: one machine code, two
# containers.
Write-Host '==> building the project' -ForegroundColor Cyan
$build = Invoke-Captured -Exe $Spass -ArgList @('build', '--deeppack') -WorkingDir $Project -TimeoutMs $timeoutMs
if ($build.ExitCode -ne 0) { Write-Host $build.Stderr -ForegroundColor Red; exit 1 }

$buildDir = Join-Path $Project 'build'
$saple = Join-Path $buildDir 'spassfile.saple'
$lib = Join-Path $buildDir 'spass_module.dll'
$manifestPath = Join-Path $buildDir 'spass_module.abi.json'
foreach ($p in @($saple, $lib, $manifestPath)) {
    if (-not (Test-Path $p)) { Write-Host "error: $p not found" -ForegroundColor Red; exit 1 }
}
Write-Host ("    package:  {0} ({1} bytes)" -f (Split-Path -Leaf $saple), (Get-Item $saple).Length)
Write-Host ("    library:  {0} ({1} bytes)" -f (Split-Path -Leaf $lib), (Get-Item $lib).Length)

$manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json

# --- 2. The package and the library have to be the same machine code.
Write-Host '==> the package carries the library' -ForegroundColor Cyan
$nativeName = $null
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($saple)
try {
    $native = $zip.Entries | Where-Object { $_.FullName -like 'native/*.dll' -or $_.FullName -like 'native/*.so' -or $_.FullName -like 'native/*.dylib' }
    if ($native) { $nativeName = $native[0].FullName }
} finally {
    $zip.Dispose()
}
if (-not $nativeName) {
    Fail 'the package carries no native module'
} else {
    $packedHash = [SpassPackagingHost]::HashEntry($saple, $nativeName)
    $onDiskHash = (Get-FileHash -LiteralPath $lib -Algorithm SHA256).Hash
    if (($packedHash -replace '-', '') -ne ($onDiskHash -replace '-', '')) {
        Fail ("the package's module is {0}, the library is {1}" -f $packedHash, $onDiskHash)
    } else {
        Write-Host ("    ok   the package's {0} is the library, byte for byte" -f $nativeName) -ForegroundColor Green
    }
}

# --- 3. Every exported function, three ways.
#
# The cases cover the shapes the ABI has: a plain float, an int, a
# value-domain parameter and return, a multi-return, `out` parameters and
# a `@dispatch` expansion. The arguments are the ones the command line
# takes: a value-domain parameter takes its state index.
$cases = @(
    @{ Name = 'height';      Args = @('0.5', '1.5') }
    @{ Name = 'slope';       Args = @('0.5', '1.5') }
    @{ Name = 'add';         Args = @('1.5', '2.5') }
    @{ Name = 'quantise';    Args = @('5') }
    @{ Name = 'height_quant'; Args = @('123.4') }
    @{ Name = 'split_value'; Args = @('3.0') }
    @{ Name = 'combine';     Args = @('4.0') }
    @{ Name = 'write_two';   Args = @('2.5') }
    @{ Name = 'write_quant'; Args = @('2.5') }
    @{ Name = 'sculpt';      Args = @('10.0', '5.0') }
    @{ Name = 'tagged_add';  Args = @('1.5', '2.5') }
)

Write-Host '==> every exported function, three ways' -ForegroundColor Cyan
$rows = @()
foreach ($c in $cases) {
    $entry = $manifest.functions | Where-Object { $_.name -eq $c.Name } | Select-Object -First 1
    if (-not $entry) {
        Write-Host ("    skip {0} (the project does not export it)" -f $c.Name) -ForegroundColor Yellow
        continue
    }
    $symbol = $entry.symbol
    $slots = [int]$entry.results

    # The program: the dispatcher in the generated module.
    $program = Invoke-Captured -Exe $Spass -ArgList (@('run', '--direct', $c.Name) + $c.Args) -WorkingDir $Project -TimeoutMs $timeoutMs
    if ($program.ExitCode -ne 0) {
        Fail ("{0}: the program mode exited with {1}: {2}" -f $c.Name, $program.ExitCode, $program.Stderr.Trim())
        continue
    }
    # The compiler's warnings share the output stream with the result, so
    # the results are the last numbers of it.
    $programValues = @(Get-Numbers $program.Stdout | Select-Object -Last $slots)

    # The package: the saple runner.
    $packaged = Invoke-Captured -Exe $Spass -ArgList (@('runner', $saple, $c.Name) + $c.Args) -WorkingDir $Project -TimeoutMs $timeoutMs
    if ($packaged.ExitCode -ne 0) {
        Fail ("{0}: the saple mode exited with {1}: {2}" -f $c.Name, $packaged.ExitCode, $packaged.Stderr.Trim())
        continue
    }
    # The runner prints the results and nothing else, one per line.
    $packagedLines = @($packaged.Stdout -split "`n" | Where-Object { $_.Trim() -ne '' })
    $packagedValues = @(Get-Numbers $packaged.Stdout | Select-Object -Last $slots)

    # The library: a host that loads it in-process.
    $argNumbers = @($c.Args | ForEach-Object { [double]$_ })
    try {
        $hostValues = [SpassPackagingHost]::Call($lib, $symbol, [double[]]$argNumbers, $slots)
    } catch {
        Fail ("{0}: the deeppack host failed: {1}" -f $c.Name, $_.Exception.Message)
        continue
    }

    # The three have to agree, and they have to fill the slots the
    # manifest says the call fills.
    $problems = @()
    if ($programValues.Count -ne $slots) {
        $problems += ("the manifest says {0} result slot(s), the program produced {1}" -f $slots, $programValues.Count)
    }
    if ($packagedLines.Count -ne $slots) {
        $problems += ("the manifest says {0} result slot(s), the package printed {1} line(s)" -f $slots, $packagedLines.Count)
    }
    for ($i = 0; $i -lt [Math]::Min($programValues.Count, $packagedValues.Count); $i++) {
        if ([math]::Abs($programValues[$i] - $packagedValues[$i]) -gt 1e-12) {
            $problems += ("the package returned {0} where the program returned {1}" -f $packagedValues[$i], $programValues[$i])
        }
    }
    for ($i = 0; $i -lt [Math]::Min($programValues.Count, $hostValues.Count); $i++) {
        if ([math]::Abs($programValues[$i] - $hostValues[$i]) -gt 1e-12) {
            $problems += ("the library returned {0} where the program returned {1}" -f $hostValues[$i], $programValues[$i])
        }
    }

    $shown = ($programValues | ForEach-Object { $_.ToString() }) -join ', '
    if ($problems.Count -eq 0) {
        Write-Host ("    ok   {0,-14} {1}" -f $c.Name, $shown) -ForegroundColor Green
    } else {
        Fail ("{0}: {1}" -f $c.Name, ($problems -join '; '))
    }
    $rows += [pscustomobject]@{ Function = $c.Name; Slots = $slots; Values = $shown }
}

# --- 4. The sequences of a module.
#
# A package is a resident world: its sequences are the entry. The runner
# starts them and joins them, and the result of the sequence has to land
# in the same place the program's own run puts it.
$sequenceProject = Join-Path $RepoRoot 'examples\sequence'
if (Test-Path $sequenceProject) {
    Write-Host '==> the sequences of a package' -ForegroundColor Cyan
    $seqBuild = Invoke-Captured -Exe $Spass -ArgList @('build') -WorkingDir $sequenceProject -TimeoutMs $timeoutMs
    if ($seqBuild.ExitCode -ne 0) {
        Fail ("the sequence project did not build: {0}" -f $seqBuild.Stderr.Trim())
    } else {
        $seqSaple = Join-Path $sequenceProject 'build\spassfile.saple'
        $seqResult = Join-Path $sequenceProject 'data\output\result.spb'
        if (Test-Path $seqResult) { Remove-Item -LiteralPath $seqResult -Force }

        # The package alone: no function to call, just the sequences.
        $seqRun = Invoke-Captured -Exe $Spass -ArgList @('runner', $seqSaple) -WorkingDir $sequenceProject -TimeoutMs $timeoutMs
        if ($seqRun.ExitCode -ne 0) {
            Fail ("the package's sequences exited with {0}: {1}" -f $seqRun.ExitCode, $seqRun.Stderr.Trim())
        } elseif (-not (Test-Path $seqResult)) {
            Fail 'the package ran, but its sequence wrote nothing'
        } else {
            $value = [System.BitConverter]::ToDouble([System.IO.File]::ReadAllBytes($seqResult), 0)
            # The producer sends 0, 10 and 20; the consumer sums them.
            if ([math]::Abs($value - 30.0) -gt 1e-9) { Fail ("the package's sequence produced {0}, expected 30" -f $value) }
            else { Write-Host ("    ok   the package's sequences ran and produced {0}" -f $value) -ForegroundColor Green }
        }
    }
} else {
    Write-Host '    skip the sequence check (no examples\sequence)' -ForegroundColor Yellow
}

# --- 5. A module that reads a spasio channel.
#
# The channel is runner-owned: the value has to arrive through the runner
# in the package mode exactly as it does in the program mode.
$awaitProject = Join-Path $RepoRoot 'examples\await'
if (Test-Path $awaitProject) {
    Write-Host '==> a package that reads a spasio channel' -ForegroundColor Cyan
    $awaitBuild = Invoke-Captured -Exe $Spass -ArgList @('build') -WorkingDir $awaitProject -TimeoutMs $timeoutMs
    if ($awaitBuild.ExitCode -ne 0) {
        Fail ("the await project did not build: {0}" -f $awaitBuild.Stderr.Trim())
    } else {
        $awaitSaple = Join-Path $awaitProject 'build\spassfile.saple'
        $programRun = Invoke-Captured -Exe $Spass -ArgList @('run', '--direct', 'driver') -WorkingDir $awaitProject -TimeoutMs $timeoutMs
        $programValue = (Get-Numbers $programRun.Stdout) | Select-Object -Last 1
        $packageRun = Invoke-Captured -Exe $Spass -ArgList @('runner', $awaitSaple, 'driver') -WorkingDir $awaitProject -TimeoutMs $timeoutMs
        if ($packageRun.ExitCode -ne 0) {
            Fail ("the package exited with {0}: {1}" -f $packageRun.ExitCode, $packageRun.Stderr.Trim())
        } else {
            $packageValue = (Get-Numbers $packageRun.Stdout) | Select-Object -Last 1
            if ($null -eq $programValue -or $null -eq $packageValue) {
                Fail ("the channel value did not arrive (program: {0}, package: {1})" -f $programValue, $packageValue)
            } elseif ([math]::Abs($programValue - $packageValue) -gt 1e-12) {
                Fail ("the package read {0} where the program read {1}" -f $packageValue, $programValue)
            } else {
                Write-Host ("    ok   the channel value arrived in both modes: {0}" -f $packageValue) -ForegroundColor Green
            }
        }
    }
} else {
    Write-Host '    skip the spasio check (no examples\await)' -ForegroundColor Yellow
}

Write-Host ''
$rows | Format-Table -AutoSize | Out-String | Write-Host
if ($fail -gt 0) { Write-Host "$fail packaging check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host ("packaging verified: {0} function(s) compared across three modes" -f $rows.Count) -ForegroundColor Green
