# Runs the M0 spikes S1 (GPU interop), S2 (virtual camera) and S3 (media clock) on
# Windows and collects everything to paste back into one folder and one zip.
#
# Start it with run-spikes.bat (double-click), or from PowerShell:
#   powershell -NoProfile -ExecutionPolicy Bypass -File spikes\run-spikes.ps1
#
# Options:
#   -Only S1,S3     run only some spikes
#   -S3Minutes 60   length of the S3 drift run (default 60; the README asks for an hour)
#   -RepoDir PATH   where to clone the repo if this script isn't inside a checkout
#
# The script asks before installing anything. It never needs admin, except for S2's
# optional machine-wide (HKLM) step, which asks for one UAC prompt.

[CmdletBinding()]
param(
    [string[]]$Only = @('S1', 'S2', 'S3'),
    [int]$S3Minutes = 60,
    [string]$RepoDir = (Join-Path $env:USERPROFILE 'JStream')
)

# 'Continue', not 'Stop': in Windows PowerShell 5.1, 'Stop' turns any stderr output of a
# native program (cargo progress, the spikes' status lines) into a terminating error.
$ErrorActionPreference = 'Continue'
# From the .bat, "-Only S1,S2" arrives as one string; split it.
$Only = @($Only | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim().ToUpper() } | Where-Object { $_ })
$RepoUrl = 'https://github.com/jeffmhopkins/JStream.git'

function Say([string]$Text, [string]$Color = 'Gray') { Write-Host $Text -ForegroundColor $Color }
function Step([string]$Text) { Write-Host ''; Write-Host "=== $Text ===" -ForegroundColor Cyan }
function Ask([string]$Question) {
    $answer = Read-Host $Question
    Add-Content -Path $script:Notes -Value "- **$Question** $answer"
    return $answer
}
function YesNo([string]$Question, [bool]$Default = $true) {
    $hint = if ($Default) { '[Y/n]' } else { '[y/N]' }
    $a = Read-Host "$Question $hint"
    if ([string]::IsNullOrWhiteSpace($a)) { return $Default }
    return $a.Trim().ToLower().StartsWith('y')
}
function Pause-For([string]$Text) { Say $Text 'Yellow'; [void](Read-Host 'Press Enter when ready') }
# Runs a spike, shows its output live, and saves its stdout (the JSON report) as UTF-8.
function Run-Saving([string]$File, [scriptblock]$Command) {
    & $Command | Tee-Object -Variable captured | Out-Host
    $captured | Set-Content -Path $File -Encoding UTF8
}

# Keeps the PC (and display) awake while this script runs, so S3's hour isn't cut short.
Add-Type -Namespace JStream -Name Power -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
'@
function Keep-Awake([bool]$On) {
    # ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED, or ES_CONTINUOUS to clear.
    if ($On) { [void][JStream.Power]::SetThreadExecutionState(0x80000003) }
    else { [void][JStream.Power]::SetThreadExecutionState(0x80000000) }
}

# ---------------------------------------------------------------------------------------
Step 'Checking prerequisites'

function Have([string]$Cmd) { [bool](Get-Command $Cmd -ErrorAction SilentlyContinue) }
function Winget-Install([string]$Id, [string]$Extra = '') {
    if (-not (Have 'winget')) { throw "winget isn't available. Install $Id by hand, then run this again." }
    $wingetArgs = "install --id $Id -e --accept-package-agreements --accept-source-agreements $Extra"
    Say "winget $wingetArgs"
    $p = Start-Process winget -ArgumentList $wingetArgs -Wait -PassThru -NoNewWindow
    if ($p.ExitCode -ne 0) { Say "winget exited with $($p.ExitCode); continuing to check." 'Yellow' }
    # Pick up PATH changes from the installer.
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
                [Environment]::GetEnvironmentVariable('Path', 'User') + ';' +
                (Join-Path $env:USERPROFILE '.cargo\bin')
}

$build = [Environment]::OSVersion.Version.Build
Say "Windows build $build"
if ($build -lt 22000) { Say 'S2 needs Windows 11 (build 22000+). S2 will be skipped.' 'Yellow'; $Only = $Only | Where-Object { $_ -ne 'S2' } }

if (-not (Have 'git')) {
    if (YesNo 'Git is missing. Install it with winget?') { Winget-Install 'Git.Git' } else { throw 'Git is required.' }
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$hasVc = $false
if (Test-Path $vswhere) {
    $vc = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $hasVc = -not [string]::IsNullOrWhiteSpace($vc)
}
if (-not $hasVc) {
    Say 'The MSVC C++ build tools (needed by Rust on Windows) were not found.' 'Yellow'
    if (YesNo 'Install Visual Studio 2022 Build Tools with the C++ workload (several GB, takes a while)?') {
        Winget-Install 'Microsoft.VisualStudio.2022.BuildTools' '--override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
    } else { throw 'MSVC build tools are required to build the spikes.' }
}

if (-not (Have 'cargo')) {
    $env:Path += ';' + (Join-Path $env:USERPROFILE '.cargo\bin')
}
if (-not (Have 'rustup')) {
    if (YesNo 'Rust (rustup) is missing. Install it with winget?') { Winget-Install 'Rustlang.Rustup' } else { throw 'Rust is required.' }
}
if (-not (Have 'cargo')) { throw 'cargo still not found. Open a new terminal (so PATH refreshes) and run this again.' }

# ---------------------------------------------------------------------------------------
Step 'Finding the JStream checkout'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$candidate = Split-Path -Parent $here
if (Test-Path (Join-Path $candidate 'spikes\s1-gpu\Cargo.toml')) {
    $Repo = $candidate
} elseif (Test-Path (Join-Path $RepoDir '.git')) {
    $Repo = $RepoDir
} else {
    Say "Cloning $RepoUrl to $RepoDir"
    git clone $RepoUrl $RepoDir
    $Repo = $RepoDir
}
Push-Location $Repo
try {
    if (YesNo 'Update the checkout to the latest main first (git pull)?') {
        git checkout main
        git pull --ff-only
    }
    $commit = (git rev-parse --short HEAD)
} finally { Pop-Location }
Say "Using $Repo at $commit"

# rust-toolchain.toml pins the toolchain; install exactly that one.
Set-Location $Repo
$pinned = (Select-String -Path (Join-Path $Repo 'rust-toolchain.toml') -Pattern 'channel\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
Say "Installing/checking Rust $pinned"
rustup toolchain install $pinned --profile minimal --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw "rustup couldn't install $pinned" }

# ---------------------------------------------------------------------------------------
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$Out = Join-Path $Repo "spike-results\$stamp"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$script:Notes = Join-Path $Out 'notes.md'
Set-Content -Path $Notes -Value "# Spike run $stamp`n`nRepo commit: $commit`n"

Step 'Collecting system info'
$sys = [ordered]@{
    windows   = (Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber)
    cpu       = (Get-CimInstance Win32_Processor | Select-Object -First 1 Name).Name
    gpus      = @(Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion, CurrentHorizontalResolution, CurrentVerticalResolution, CurrentRefreshRate)
    audio     = @(Get-CimInstance Win32_SoundDevice | Select-Object Name, Manufacturer)
    laptop    = [bool](Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue)
    rustc     = (rustc --version)
}
$sys | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $Out 'system.json')
Say "Saved system.json (GPUs: $(($sys.gpus | ForEach-Object Name) -join ', '))"

# ---------------------------------------------------------------------------------------
if ($Only -contains 'S1') {
    Step 'S1: GPU interop (capture -> wgpu -> NV12 -> hardware H.264), ~30 s'
    Add-Content $Notes "`n## S1`n"
    Push-Location (Join-Path $Repo 'spikes\s1-gpu')
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        $exe = '.\target\release\s1-gpu.exe'
        Pause-For 'Start something moving at 60 fps on your PRIMARY display (a 60 fps video in fullscreen, or a game).'
        Run-Saving (Join-Path $Out 's1.json') { & $exe --seconds 30 --json --out (Join-Path $Out 's1-out.h264') }
        Say "S1 exit code: $LASTEXITCODE"
        Add-Content $Notes "- exit code: $LASTEXITCODE"
        [void](Ask 'What was on screen during the run (video/game/still desktop)?')
        [void](Ask 'Display resolution and refresh rate? Laptop with two GPUs?')
        Say "Now open $(Join-Path $Out 's1-out.h264') in VLC (or any player that reads raw H.264)." 'Yellow'
        [void](Ask 'Does s1-out.h264 play correctly (right colours, no green/pink cast, no tearing, smooth)?')
        if (YesNo 'Also run once at the display''s native size?' $false) {
            Run-Saving (Join-Path $Out 's1-native.json') { & $exe --seconds 30 --json --size native --out (Join-Path $Out 's1-native.h264') }
        }
    } catch {
        Say "S1 failed: $_" 'Red'; Add-Content $Notes "- S1 failed: $_"
    } finally { Pop-Location }
}

# ---------------------------------------------------------------------------------------
if ($Only -contains 'S2') {
    Step 'S2: virtual camera (Rust COM media source loaded by Frame Server)'
    Add-Content $Notes "`n## S2`n"
    Push-Location (Join-Path $Repo 'spikes\s2-vcam')
    $exe = Join-Path (Get-Location) 'target\release\s2-vcam.exe'
    $usedHklm = $false
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed (if the DLL is in use, run "net stop FrameServer" elevated and retry)' }

        Say 'Step 1: per-user registration (no admin).' 'Cyan'
        & $exe --register hkcu
        Say 'The producer starts now. The Camera app will open: switch cameras until you see "JStream S2", watch a few seconds,' 'Yellow'
        Say 'then CLOSE the Camera app, come back here and press Enter to stop the producer.' 'Yellow'
        Start-Process 'microsoft.windows.camera:' -ErrorAction SilentlyContinue
        Run-Saving (Join-Path $Out 's2-hkcu.json') { & $exe --json }
        [void](Ask 'HKCU: was "JStream S2" listed, and what did it show (teal + moving bar / colour bars / black / error)?')

        $hkcuWorked = YesNo 'Did the HKCU attempt show the teal picture with the moving bar?' $false
        Add-Content $Notes "- HKCU fully worked: $hkcuWorked"
        if (-not $hkcuWorked) {
            & $exe --unregister
            if (YesNo 'Try machine-wide (HKLM) registration? This needs one UAC prompt.') {
                $cmd = "net stop FrameServer; & '$exe' --register hklm; Start-Sleep -Seconds 3"
                $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
                Start-Process powershell -Verb RunAs -Wait -ArgumentList "-NoProfile -EncodedCommand $enc"
                $usedHklm = $true
                Say 'Same again: Camera app, find "JStream S2", close the app, then press Enter here.' 'Yellow'
                Start-Process 'microsoft.windows.camera:' -ErrorAction SilentlyContinue
                Run-Saving (Join-Path $Out 's2-hklm.json') { & $exe --json }
                [void](Ask 'HKLM: was "JStream S2" listed, and what did it show?')
            }
        }
        [void](Ask 'Anything unusual (UAC, SmartScreen, antivirus alert, build errors)? Optional')
    } catch {
        Say "S2 failed: $_" 'Red'; Add-Content $Notes "- S2 failed: $_"
    } finally {
        Say 'Cleaning up the S2 registration.'
        if ($usedHklm) {
            $cmd = "& '$exe' --unregister; Start-Sleep -Seconds 2"
            $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
            Start-Process powershell -Verb RunAs -Wait -ArgumentList "-NoProfile -EncodedCommand $enc"
        } elseif (Test-Path $exe) { & $exe --unregister }
        Copy-Item 'C:\ProgramData\JStreamS2\*.log' $Out -ErrorAction SilentlyContinue
        Pop-Location
    }
}

# ---------------------------------------------------------------------------------------
if ($Only -contains 'S3') {
    Step "S3: media clock and audio drift (2-minute smoke test, then $S3Minutes minutes)"
    Add-Content $Notes "`n## S3`n"
    Push-Location (Join-Path $Repo 'spikes\s3-clock')
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        $exe = '.\target\release\s3-clock.exe'
        Pause-For 'Start audio playing (a long playlist or video) and leave it playing. Don''t unplug or change audio devices during S3.'
        Keep-Awake $true
        Say 'Smoke test (2 minutes)...'
        Run-Saving (Join-Path $Out 's3-smoke.json') { & $exe --minutes 2 --json --csv (Join-Path $Out 's3-smoke.csv') }
        $smoke = Get-Content (Join-Path $Out 's3-smoke.json') -Raw
        if ($smoke -match '"error"') { Say 'The smoke test reported an error; see s3-smoke.json.' 'Yellow' }
        if (YesNo "Run the full $S3Minutes-minute measurement now? (Ctrl+C stops early and still saves results)") {
            Say "Running for $S3Minutes minutes. The PC is kept awake. Progress prints every 10 s."
            Run-Saving (Join-Path $Out 's3.json') { & $exe --minutes $S3Minutes --json --csv (Join-Path $Out 'drift.csv') }
        }
        [void](Ask 'Which mic (USB/built-in/headset) and which output (motherboard/USB DAC/HDMI)? Same interface?')
        [void](Ask 'What played during S3, and did you hear clicks, dropouts or audio stopping?')
    } catch {
        Say "S3 failed: $_" 'Red'; Add-Content $Notes "- S3 failed: $_"
    } finally {
        Keep-Awake $false
        Pop-Location
    }
}

# ---------------------------------------------------------------------------------------
Step 'Packaging results'
$zip = "$Out.zip"
# The raw H.264 files can be large; leave them out of the zip (they're only for you to watch).
$files = Get-ChildItem $Out -File | Where-Object { $_.Extension -ne '.h264' }
Compress-Archive -Path $files.FullName -DestinationPath $zip -Force
Say "Results folder: $Out" 'Green'
Say "Zip to send back: $zip" 'Green'
Say 'Attach the zip in the chat (or paste notes.md plus the .json files).' 'Green'
Start-Process explorer.exe "/select,`"$zip`""
