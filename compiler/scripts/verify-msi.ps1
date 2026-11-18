# Verifies MSI install, run, and uninstall leave the machine unchanged.
# The MSI is per-machine, so this script needs elevation.
$ErrorActionPreference = 'Stop'

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'verify-msi.ps1 must run as Administrator (per-machine MSI).'
}

$Msi = 'D:\spass\dist\spass-0.1.0-x64.msi'
$ProductCode = '{6B2D8F41-3C57-4A92-B0E6-71D4C8E5F309}'
$InstallDir = Join-Path $env:ProgramFiles 'Spass'
$Probe = 'D:\spass\examples\await'

function MachinePath {
    $k = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -ErrorAction SilentlyContinue
    if ($k -and $k.Path) { return [string]$k.Path }
    return ''
}

# The PATH as a list of folders.
#
# The installer appends its folder and the uninstaller takes it away
# again, and the two do not always agree on the separator: a leftover
# trailing backslash is not a change to the machine's PATH, so the
# comparison is on the folders, not on the bytes.
function Normalise-Path {
    param([string] $Value)
    $parts = @($Value -split ';' | Where-Object { $_.Trim() -ne '' } | ForEach-Object { $_.Trim().TrimEnd('\') })
    return ($parts -join ';')
}

# The `.saple` file association. The keys live in the machine registry,
# matching the per-machine install; the uninstall must take them away.
$ExtKey = 'HKLM:\Software\Classes\.saple'
$ClassKey = 'HKLM:\Software\Classes\spassfile.saple'
$CommandKey = 'HKLM:\Software\Classes\spassfile.saple\shell\open\command'

function AssocState {
    $ext = $null; $cmd = $null
    if (Test-Path $ExtKey) { $ext = (Get-Item $ExtKey).GetValue('') }
    if (Test-Path $CommandKey) { $cmd = (Get-Item $CommandKey).GetValue('') }
    return @{ Ext = $ext; Command = $cmd }
}

$assocBefore = AssocState
Write-Host "association before: ext=$($assocBefore.Ext) command=$($assocBefore.Command)"

$pathBefore = MachinePath
Write-Host "machine PATH before: $($pathBefore.Length) chars"

# The daemon is a machine-wide singleton: a runner that another test
# left up changes what `daemon --status` says, and the build below
# starts one of its own. Record whether one was running, so the test
# stops only the one it started.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$DevSpass = Join-Path $RepoRoot 'compiler\target\release\spass.exe'
# The compiler links against the toolchain's libunwind.dll, which is not
# on PATH for a machine that only has Spass.
$unwind = Get-ChildItem -Path (Join-Path $env:USERPROFILE '.rustup\toolchains') -Recurse -Filter 'libunwind.dll' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\bin\\' } | Select-Object -First 1
if ($unwind) {
    $unwindDir = Split-Path -Parent $unwind.FullName
    if ($env:PATH -notlike "*$unwindDir*") { $env:PATH = "$unwindDir;$env:PATH" }
}
$daemonWasUp = $false
if (Test-Path $DevSpass) {
    $pre = & $DevSpass daemon --status 2>&1 | Out-String
    if ($LASTEXITCODE -eq 0 -and $pre -notmatch 'not running') {
        $daemonWasUp = $true
        Write-Host "a daemon is already up: $($pre.Trim() -split "`n" | Select-Object -First 1)"
    }
}

Write-Host '==> installing' -ForegroundColor Cyan
$p = Start-Process msiexec -ArgumentList @('/i', "`"$Msi`"", '/qn', '/norestart') -Wait -PassThru
Write-Host ("    msiexec exit {0}" -f $p.ExitCode)
if ($p.ExitCode -ne 0) { throw "install failed with $($p.ExitCode)" }

$exe = Join-Path $InstallDir 'spass.exe'
if (-not (Test-Path $exe)) { throw "the compiler is not at $exe" }

Write-Host '==> running the installed compiler' -ForegroundColor Cyan
$env:PATH = "$InstallDir;$env:PATH"
$v = & $exe --version 2>&1 | Out-String
Write-Host ("    version: {0}" -f $v.Trim())
if ($v -notmatch '0\.1\.0') { throw "unexpected version: $v" }

$ErrorActionPreference = 'Continue'
$s = & $exe daemon --status 2>&1 | Out-String
$rc = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
Write-Host ("    daemon --status: {0} (exit {1})" -f $s.Trim(), $rc)
if ($rc -ne 0) { throw "daemon --status must exit 0" }
# Whether a runner is up is machine state this test does not own, so the
# check is that the installed compiler answers with a status at all. The
# "not running" wording is verify-runner.ps1's job, and it controls the
# daemon itself.
if ($s -notmatch 'not running' -and $s -notmatch 'daemon:') {
    throw "daemon --status printed nothing recognisable: $s"
}

# The deeppack artifact is the one that needs the runtime DLL next to it.
$ErrorActionPreference = 'Continue'
Push-Location $Probe
try {
    $b = & $exe build --deeppack 2>&1 | Out-String
    $brc = $LASTEXITCODE
} finally { Pop-Location }
$ErrorActionPreference = 'Stop'
Write-Host ("    build --deeppack: {0}" -f ($b.Trim() -split "`n" | Where-Object { $_ -match 'deeppack|manifest|Build' } | Select-Object -Last 1))
if ($brc -ne 0) { throw "build --deeppack failed with $brc`n$b" }
$dll = Join-Path $Probe 'build\spass_module.dll'
$abi = Join-Path $Probe 'build\spass_module.abi.json'
if (-not (Test-Path $dll)) { throw "no deeppack DLL at $dll" }
if (-not (Test-Path $abi)) { throw "no deeppack manifest at $abi" }
Write-Host ("    ok   {0} ({1} bytes) + {2}" -f (Split-Path -Leaf $dll), (Get-Item $dll).Length, (Split-Path -Leaf $abi)) -ForegroundColor Green

# The `.saple` file association: a package is opened by the runner.
Write-Host '==> checking the file association' -ForegroundColor Cyan
$assocAfter = AssocState
Write-Host ("    ext     : {0}" -f $assocAfter.Ext)
Write-Host ("    command : {0}" -f $assocAfter.Command)
if ($assocAfter.Ext -ne 'spassfile.saple') { throw "the .saple extension is not bound to the package class ($($assocAfter.Ext))" }
if ($assocAfter.Command -notmatch 'spass\.exe"?\s+runner') { throw "the open command does not run the runner: $($assocAfter.Command)" }
if ($assocAfter.Command -notmatch [regex]::Escape($InstallDir)) { throw "the open command does not point at the install: $($assocAfter.Command)" }
Write-Host '    ok   .saple opens through the runner' -ForegroundColor Green

Write-Host '==> uninstalling' -ForegroundColor Cyan
$p = Start-Process msiexec -ArgumentList @('/x', $ProductCode, '/qn', '/norestart') -Wait -PassThru
Write-Host ("    msiexec exit {0}" -f $p.ExitCode)
if ($p.ExitCode -ne 0) { throw "uninstall failed with $($p.ExitCode)" }

Write-Host '==> checking the machine' -ForegroundColor Cyan
Start-Sleep -Seconds 1
if (Test-Path $InstallDir) { throw "the install folder is still there: $InstallDir" }
Write-Host '    ok   the install folder is gone' -ForegroundColor Green
$assocFinal = AssocState
Write-Host ("    ext     : {0}" -f $assocFinal.Ext)
Write-Host ("    command : {0}" -f $assocFinal.Command)
# The installer owns the association, so the uninstall has to take it
# away. A stale registration left by an earlier aborted run points at an
# install folder that no longer exists, and removing it is the right
# thing to do - so the invariant is "gone", not "byte-identical".
if (Test-Path $CommandKey) { throw "the open command key is still there: $CommandKey" }
if ($assocFinal.Command) { throw "the open command survived the uninstall: $($assocFinal.Command)" }
Write-Host '    ok   the file association is gone' -ForegroundColor Green
$pathAfter = MachinePath
if ((Normalise-Path $pathAfter) -ne (Normalise-Path $pathBefore)) {
    Write-Host '    FAIL the user PATH changed' -ForegroundColor Red
    Write-Host "      before: $pathBefore"
    Write-Host "      after : $pathAfter"
    exit 1
}
Write-Host '    ok   the user PATH is unchanged' -ForegroundColor Green

# The build above starts a runner of its own (the spasio channels are
# runner-owned). A runner that was not running before the test must not
# be left running after it.
if (-not $daemonWasUp -and (Test-Path $DevSpass)) {
    & $DevSpass daemon --stop 2>&1 | Out-Null
    Write-Host '    ok   the runner the test started is stopped' -ForegroundColor Green
}

Write-Host 'msi accepted' -ForegroundColor Green
