# Runs spike S4 (CEF browser source) on Windows and collects everything into one folder
# and one zip. It asks no questions: it checks the build tools, builds the spike (the first
# build downloads the CEF runtime, a few hundred MB), runs it three ways and records the
# machine's details.
#
# Start it with run-s4.bat (double-click), or from PowerShell:
#   powershell -NoProfile -ExecutionPolicy Bypass -File spikes\s4-browser\run-s4.ps1
#
# Options:
#   -Seconds 30     length of each measured window (default 30)
#   -Skip4K         leave out the 3840x2160 run
#
# The runs (each prints one JSON line, saved as <run>.json):
#   same       1920x1080 at 60 fps, CEF's subprocesses run as s4-browser.exe
#   separate   the same, with subprocesses as s4-browser-helper.exe
#   4k         3840x2160 at 60 fps, same-exe layout (headroom check)
#
# Nothing is shown on screen: the browser renders off-screen. It runs from the checkout this
# script is in, on whatever branch is checked out.

[CmdletBinding()]
param(
    [int]$Seconds = 30,
    [switch]$Skip4K
)

$ErrorActionPreference = 'Continue'
$Utf8 = New-Object Text.UTF8Encoding $false
function Say([string]$Text, [string]$Color = 'Gray') { Write-Host $Text -ForegroundColor $Color }
function Step([string]$Text) { Write-Host ''; Write-Host "=== $Text ===" -ForegroundColor Cyan }
function Have([string]$Cmd) { [bool](Get-Command $Cmd -ErrorAction SilentlyContinue) }
function Note([string]$Text) { [IO.File]::AppendAllText($script:Notes, "$Text`r`n", $Utf8) }
function Arg([string]$A) { if ($A -match '[\s"]') { '"' + ($A -replace '"', '\"') + '"' } else { $A } }

function Describe-Exit($Code) {
    if ($Code -is [string]) { return $Code }
    $hex = '0x{0:X8}' -f ([int64]$Code -band 4294967295)
    switch ($hex) {
        '0xC0000005' { return "$Code ($hex, access violation)" }
        '0xC0000409' { return "$Code ($hex, fail-fast / stack buffer overrun)" }
        '0xC0000135' { return "$Code ($hex, a DLL was not found)" }
        default { if ($Code -lt 0) { return "$Code ($hex)" } else { return "$Code" } }
    }
}

# Runs a program with stdout and stderr in files and kills it after $TimeoutSec.
function Run-Logged([string]$Exe, [string[]]$Arguments, [string]$StdOut, [string]$StdErr, [int]$TimeoutSec) {
    Say "> $Exe $($Arguments -join ' ')" 'DarkGray'
    $p = Start-Process -FilePath $Exe -ArgumentList (($Arguments | ForEach-Object { Arg $_ }) -join ' ') `
        -RedirectStandardOutput $StdOut -RedirectStandardError $StdErr -NoNewWindow -PassThru
    $null = $p.Handle  # without this, Windows PowerShell can lose the exit code
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = ''
    while (-not $p.WaitForExit(5000)) {
        $line = Get-Content $StdErr -Tail 1 -ErrorAction SilentlyContinue
        if ($line -and $line -ne $last) { Say "  $line" 'DarkGray'; $last = $line }
        if ((Get-Date) -gt $deadline) {
            Say "  still running after $TimeoutSec s; stopping it" 'Yellow'
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
            return 'timeout'
        }
    }
    $p.WaitForExit()
    return $p.ExitCode
}

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Repo = Split-Path -Parent (Split-Path -Parent $here)
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$Out = Join-Path $here "s4-out\$stamp"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$script:Notes = Join-Path $Out 'notes.md'
Note "# S4 run $stamp"

# ---------------------------------------------------------------------------------------
Step 'Checking the build tools'

# The CEF bindings build CEF's C++ wrapper with CMake and Ninja, so cargo needs the MSVC
# environment (cl.exe, the SDK) in this shell, not only rustc's own detection.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = $null
if (Test-Path $vswhere) {
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
}
if ([string]::IsNullOrWhiteSpace($vs)) {
    throw 'The MSVC C++ build tools were not found. Run spikes\run-spikes.bat once (it installs them), or install "Desktop development with C++".'
}
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
Say "Loading the MSVC environment from $vcvars"
cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2]) }
}
$env:Path = "$env:Path;" + (Join-Path $env:USERPROFILE '.cargo\bin')

foreach ($tool in @(@('cmake', 'Kitware.CMake'), @('ninja', 'Ninja-build.Ninja'))) {
    if (Have $tool[0]) { continue }
    if (-not (Have 'winget')) { throw "$($tool[0]) is missing and winget isn't available to install it." }
    Say "Installing $($tool[1]) with winget" 'Yellow'
    winget install --id $tool[1] -e --accept-package-agreements --accept-source-agreements | Out-Host
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
                [Environment]::GetEnvironmentVariable('Path', 'User') + ";$env:Path"
    if (-not (Have $tool[0])) { throw "$($tool[0]) is still missing; open a new terminal and run this again." }
}
if (-not (Have 'cargo')) { throw 'cargo was not found. Run spikes\run-spikes.bat once (it installs Rust).' }
foreach ($tool in @('cargo', 'cl', 'cmake', 'ninja')) {
    $path = (Get-Command $tool).Source
    $ver = switch ($tool) {
        'cl' { (cmd /c 'cl 2>&1' | Select-Object -First 1) }
        default { (& $tool --version 2>&1 | Select-Object -First 1) }
    }
    Say "$tool : $ver ($path)"
    Note "- $tool : $ver"
}

# ---------------------------------------------------------------------------------------
Step 'Machine'

$os = Get-CimInstance Win32_OperatingSystem
Note "- Windows: $($os.Caption) $($os.Version) (build $([Environment]::OSVersion.Version.Build))"
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
Note "- CPU: $($cpu.Name), $($cpu.NumberOfLogicalProcessors) logical"
foreach ($gpu in Get-CimInstance Win32_VideoController) {
    Note "- GPU: $($gpu.Name), driver $($gpu.DriverVersion), $($gpu.CurrentHorizontalResolution)x$($gpu.CurrentVerticalResolution) @ $($gpu.CurrentRefreshRate) Hz"
}
Push-Location $Repo
$commit = (git rev-parse --short HEAD 2>$null); $branch = (git rev-parse --abbrev-ref HEAD 2>$null)
Pop-Location
Note "- Checkout: $branch at $commit"
Get-Content $script:Notes | Out-Host

# ---------------------------------------------------------------------------------------
Step 'Building spike S4 (the first build downloads CEF; this can take a while)'

Push-Location $here
try {
    $start = Get-Date
    cargo build --release 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath (Join-Path $Out 'build.txt') |
        Where-Object { $_ -match '^(error|warning)|Finished|Compiling (cef|s4)' } | Out-Host
    $built = ($LASTEXITCODE -eq 0)
    Note "- Build: $(if ($built) { 'ok' } else { 'FAILED' }) in $([int]((Get-Date) - $start).TotalSeconds) s"
} finally { Pop-Location }

$release = Join-Path $here 'target\release'
if ($built) {
    Get-ChildItem $release -File | Where-Object { $_.Extension -in '.exe', '.dll', '.pak', '.dat', '.bin', '.json' } |
        Sort-Object Length -Descending | ForEach-Object { '{0,12:N0}  {1}' -f $_.Length, $_.Name } |
        Set-Content (Join-Path $Out 'runtime-files.txt') -Encoding UTF8
    $exe = Join-Path $release 's4-browser.exe'
    $runs = @(
        @{ Name = 'same'; Args = @('--helper', 'same') },
        @{ Name = 'separate'; Args = @('--helper', 'separate') }
    )
    if (-not $Skip4K) { $runs += @{ Name = '4k'; Args = @('--helper', 'same', '--size', '3840x2160') } }

    foreach ($run in $runs) {
        Step "Run: $($run.Name)"
        $json = Join-Path $Out "$($run.Name).json"
        $err = Join-Path $Out "$($run.Name)-stderr.txt"
        $log = Join-Path $Out "$($run.Name)-cef.log"
        $runArgs = @($run.Args) + @('--seconds', "$Seconds", '--json', '--cef-log', $log)
        Push-Location $release
        try {
            $code = Run-Logged $exe $runArgs $json $err ($Seconds + 120)
        } finally { Pop-Location }
        Note "- Run $($run.Name): exit $(Describe-Exit $code)"
        $line = Get-Content $json -ErrorAction SilentlyContinue | Select-Object -Last 1
        if ($line) { Say $line 'Green' } else { Say 'no JSON output; see the stderr file' 'Yellow' }
    }
}

# ---------------------------------------------------------------------------------------
Step 'Packing the results'

$zip = Join-Path $here "s4-out\s4-results-$stamp.zip"
Compress-Archive -Path (Join-Path $Out '*') -DestinationPath $zip -Force
Say ''
Say "Results folder: $Out" 'Green'
Say "Zip:            $zip" 'Green'
Say 'Send the zip back (or paste the three JSON lines and notes.md).' 'Green'
