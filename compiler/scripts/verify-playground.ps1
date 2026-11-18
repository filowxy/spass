# Verifies playground rendering by asserting framebuffer pixels.

[CmdletBinding()]
param(
    [int] $Frames = 60
)

$ErrorActionPreference = 'Stop'
# The script lives in <repo>/compiler/scripts, so the repo root is three
# levels up: scripts -> compiler -> repo.
$RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$Spass = Join-Path $RepoRoot 'compiler\target\release\spass.exe'
$Project = Join-Path $RepoRoot 'examples\playground'

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

function Invoke-Run {
    param([string[]] $ArgList, [string] $WorkingDir, [int] $TimeoutMs = 120000)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Spass
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

Write-Host '==> building the playground example' -ForegroundColor Cyan
Push-Location $Project
try {
    $b = Invoke-Run -ArgList @('build') -WorkingDir $Project
} finally { Pop-Location }
if ($b.ExitCode -ne 0) { Write-Host $b.Output -ForegroundColor Red; exit 1 }

$build = Join-Path $Project 'build'
$ppm = Join-Path $build 'frame.ppm'

# --- the frame ------------------------------------------------------------
Write-Host '==> rendering one frame (headless)' -ForegroundColor Cyan
$env:SPASS_PLAYGROUND_FRAMES = '1'
$env:SPASS_PLAYGROUND_PPM = $ppm
Push-Location $Project
try {
    $r = Invoke-Run -ArgList @('run', '--direct', 'app') -WorkingDir $Project
} finally { Pop-Location }
if ($r.ExitCode -ne 0) { Write-Host $r.Output -ForegroundColor Red; exit 1 }
if (-not (Test-Path $ppm)) { Write-Host "error: no frame at $ppm" -ForegroundColor Red; exit 1 }

$bytes = [System.IO.File]::ReadAllBytes($ppm)
$header = [System.Text.Encoding]::ASCII.GetString($bytes[0..14])
$w = 320; $h = 240; $off = 15
if ($header -ne "P6`n$w $h`n255`n") {
    Write-Host "error: unexpected PPM header: $header" -ForegroundColor Red
    exit 1
}

function Get-Pixel($x, $y) {
    $i = 15 + ($y * 320 + $x) * 3
    return "$($bytes[$i]),$($bytes[$i+1]),$($bytes[$i+2])"
}

# The expected colours, computed from the source values in the example
# with the runtime's `v * 255 + 0.5` quantisation.
#
# The translucent square is a blend: alpha 0.5 of pure red over the
# background (20,23,31). The runtime blends with
# `(src * a + dst * (255 - a)) / 255` and a = 128.
#
# The label plate is a blend too: alpha 0.85 of (26,28,36) over the
# background, which gives (25,27,35).
$checks = @(
    @{ What = 'background';        Pixel = (Get-Pixel 5 5);       Want = '20,23,31' }
    @{ What = 'rectangle fill';    Pixel = (Get-Pixel 60 50);     Want = '51,153,242' }
    @{ What = 'rectangle outline'; Pixel = (Get-Pixel 20 20);     Want = '255,255,255' }
    @{ What = 'disc';              Pixel = (Get-Pixel 220 60);    Want = '242,115,51' }
    @{ What = 'opaque panel';      Pixel = (Get-Pixel 160 180);   Want = '51,153,242' }
    @{ What = 'translucent blend'; Pixel = (Get-Pixel 258 13);    Want = '137,11,15' }
    @{ What = 'ellipse';           Pixel = (Get-Pixel 55 185);    Want = '89,204,115' }
    @{ What = 'triangle';          Pixel = (Get-Pixel 268 180);   Want = '242,191,64' }
    @{ What = 'label plate';       Pixel = (Get-Pixel 148 4);     Want = '25,27,35' }
)

$fail = 0
foreach ($c in $checks) {
    if ($c.Pixel -eq $c.Want) {
        Write-Host ("    ok   {0,-18} {1}" -f $c.What, $c.Pixel) -ForegroundColor Green
    } else {
        Write-Host ("    FAIL {0,-18} got {1}, want {2}" -f $c.What, $c.Pixel, $c.Want) -ForegroundColor Red
        $fail++
    }
}

# The 3D layer: the ground grid and the lit box.
$counts = @{}
for ($y = 0; $y -lt $h; $y++) {
    for ($x = 0; $x -lt $w; $x++) {
        $k = Get-Pixel $x $y
        if ($counts.ContainsKey($k)) { $counts[$k]++ } else { $counts[$k] = 1 }
    }
}
# The grid is drawn in (0.25, 0.28, 0.32) and the box in (0.85, 0.30,
# 0.25) modulated by the directional light, so the box shows up as
# several shades of the same hue.
$gridPixels = 0
if ($counts.ContainsKey('64,71,82')) { $gridPixels = $counts['64,71,82'] }
# Every colour the 2D overlay draws. What is left over belongs to the
# 3D layer: the box, the sphere and the cylinder.
$overlayColours = @('20,23,31', '51,153,242', '242,115,51', '255,255,255',
                    '64,71,82', '137,11,15', '89,204,115', '242,191,64', '25,27,35')
$boxPixels = 0
foreach ($k in $counts.Keys) { if ($overlayColours -notcontains $k) { $boxPixels += $counts[$k] } }

if ($gridPixels -gt 100) {
    Write-Host ("    ok   {0,-18} {1} px" -f '3D ground grid', $gridPixels) -ForegroundColor Green
} else {
    Write-Host ("    FAIL {0,-18} only {1} px" -f '3D ground grid', $gridPixels) -ForegroundColor Red
    $fail++
}
if ($boxPixels -gt 100) {
    Write-Host ("    ok   {0,-18} {1} px" -f '3D lit box', $boxPixels) -ForegroundColor Green
} else {
    Write-Host ("    FAIL {0,-18} only {1} px" -f '3D lit box', $boxPixels) -ForegroundColor Red
    $fail++
}

# The sphere and the cylinder are shaded per triangle, so they have no
# single colour to assert. They do have a shape though: the sphere is
# blue where it is lit, the cylinder is warm.
$spherePixels = 0
$cylinderPixels = 0
$labelGlyphs = 0
for ($y = 0; $y -lt $h; $y++) {
    for ($x = 0; $x -lt $w; $x++) {
        $k = Get-Pixel $x $y
        $p = $k.Split(',') | ForEach-Object { [int]$_ }
        if ($x -ge 185 -and $x -lt 220 -and $y -ge 95 -and $y -lt 145) {
            # Blue, and not the grid or the background.
            if ($p[2] -gt 60 -and $p[2] -gt $p[1] -and $p[1] -gt $p[0] -and $k -ne '64,71,82') { $spherePixels++ }
        }
        if ($x -ge 92 -and $x -lt 128 -and $y -ge 100 -and $y -lt 122) {
            # Warm, and not the grid.
            if ($p[0] -gt 60 -and $p[0] -gt $p[1] -and $p[1] -gt $p[2] -and $k -ne '64,71,82') { $cylinderPixels++ }
        }
        if ($x -ge 144 -and $x -lt 240 -and $y -lt 24 -and $k -eq '255,255,255') { $labelGlyphs++ }
    }
}
if ($spherePixels -gt 800) {
    Write-Host ("    ok   {0,-18} {1} px" -f '3D sphere', $spherePixels) -ForegroundColor Green
} else {
    Write-Host ("    FAIL {0,-18} only {1} px" -f '3D sphere', $spherePixels) -ForegroundColor Red
    $fail++
}
if ($cylinderPixels -gt 300) {
    Write-Host ("    ok   {0,-18} {1} px" -f '3D cylinder', $cylinderPixels) -ForegroundColor Green
} else {
    Write-Host ("    FAIL {0,-18} only {1} px" -f '3D cylinder', $cylinderPixels) -ForegroundColor Red
    $fail++
}
if ($labelGlyphs -gt 200) {
    Write-Host ("    ok   {0,-18} {1} px" -f '2D label glyphs', $labelGlyphs) -ForegroundColor Green
} else {
    Write-Host ("    FAIL {0,-18} only {1} px" -f '2D label glyphs', $labelGlyphs) -ForegroundColor Red
    $fail++
}

# The 2D overlay is its own layer: the opaque panel covers the ground
# grid, so no grid pixel may survive inside it. The example draws the
# overlay BEFORE the 3D scene, so this only holds if the layers are
# composited rather than sharing one buffer.
$panelGrid = 0
$panelBox = 0
for ($y = 150; $y -lt 210; $y++) {
    for ($x = 130; $x -lt 190; $x++) {
        $k = Get-Pixel $x $y
        if ($k -eq '64,71,82') { $panelGrid++ }
        elseif ($overlayColours -notcontains $k) { $panelBox++ }
    }
}
if ($panelGrid -eq 0 -and $panelBox -eq 0) {
    Write-Host ("    ok   {0,-18} {1} 3D px" -f '2D over 3D', 0) -ForegroundColor Green
} else {
    Write-Host ("    FAIL {0,-18} {1} grid px, {2} box px survive under the panel" -f '2D over 3D', $panelGrid, $panelBox) -ForegroundColor Red
    $fail++
}

# --- the frame cost -------------------------------------------------------
Write-Host ("==> measuring the frame cost ({0} frames)" -f $Frames) -ForegroundColor Cyan
$env:SPASS_PLAYGROUND_PPM = ''
$env:SPASS_PLAYGROUND_FRAMES = '1'
Push-Location $Project
try { Invoke-Run -ArgList @('run', '--direct', 'app') -WorkingDir $Project | Out-Null } finally { Pop-Location }

$env:SPASS_PLAYGROUND_FRAMES = [string]($Frames + 1)
$sw = [System.Diagnostics.Stopwatch]::StartNew()
Push-Location $Project
try { $many = Invoke-Run -ArgList @('run', '--direct', 'app') -WorkingDir $Project } finally { Pop-Location }
$sw.Stop()
if ($many.ExitCode -ne 0) { Write-Host $many.Output -ForegroundColor Red; exit 1 }
$perFrame = $sw.Elapsed.TotalMilliseconds / $Frames
Write-Host ("    {0} frames in {1} ms -> {2} ms/frame ({3} fps)" -f $Frames, [math]::Round($sw.Elapsed.TotalMilliseconds), [math]::Round($perFrame, 2), [math]::Round(1000 / $perFrame)) -ForegroundColor Green
Remove-Item Env:\SPASS_PLAYGROUND_FRAMES -ErrorAction SilentlyContinue
Remove-Item Env:\SPASS_PLAYGROUND_PPM -ErrorAction SilentlyContinue

if ($fail -gt 0) { Write-Host "$fail playground check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host 'playground verified' -ForegroundColor Green
