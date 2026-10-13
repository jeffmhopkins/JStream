# Runs the M0 spikes S1 (GPU interop), S2 (virtual camera) and S3 (media clock) on
# Windows and collects everything into one folder and one zip. It asks no questions: it
# installs what's missing, drives the spikes itself and records the machine's details.
#
# Start it with run-spikes.bat (double-click), or from PowerShell:
#   powershell -NoProfile -ExecutionPolicy Bypass -File spikes\run-spikes.ps1
#
# Options:
#   -Only S1,S3     which spikes to run (default S1,S3; S2 is answered, see below)
#   -S3Minutes 60   length of the S3 drift run (default 60; the README asks for an hour)
#   -RepoDir PATH   where to clone the repo if this script isn't inside a checkout
#   -S3Silent       don't play the soundtrack during S3
#
# What runs by itself:
#   S1  An animated test page plays full screen in Edge (kiosk mode) while S1 captures and
#       encodes it, at 1920x1080 and at the display's native size. The first few MB of each
#       H.264 output go into the zip so the picture can be checked without you.
#   S2  Only with -Only S2. Registers the camera per user, then machine-wide (a UAC
#       prompt, and one more to remove it at the end), and each time a small Edge window opens "JStream S2" as a webcam while
#       the producer runs. The producer's report says whether frames reached it.
#   S3  A quiet generated soundtrack (soft chime arpeggio, about -24 dBFS) loops on the
#       default output for the whole measurement, since real playback is the realistic
#       case (s3-clock README). -S3Silent leaves only the spike's own silent keep-alive.
#       A 2-minute smoke run, then the full run. The PC is kept awake.
#
# Ctrl+C stops everything; the results gathered so far are still zipped.

[CmdletBinding()]
param(
    [string[]]$Only = @('S1', 'S3'),
    [int]$S3Minutes = 60,
    [string]$RepoDir = (Join-Path $env:USERPROFILE 'JStream'),
    [switch]$S3Silent
)

# 'Continue', not 'Stop': in Windows PowerShell 5.1, 'Stop' turns any stderr output of a
# native program (cargo progress, the spikes' status lines) into a terminating error.
$ErrorActionPreference = 'Continue'
# From the .bat, "-Only S1,S2" arrives as one string; split it.
$Only = @($Only | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim().ToUpper() } | Where-Object { $_ })
$RepoDir = [IO.Path]::GetFullPath($RepoDir)
$RepoUrl = 'https://github.com/jeffmhopkins/JStream.git'
$Utf8 = New-Object Text.UTF8Encoding $false

function Say([string]$Text, [string]$Color = 'Gray') { Write-Host $Text -ForegroundColor $Color }
function Step([string]$Text) { Write-Host ''; Write-Host "=== $Text ===" -ForegroundColor Cyan }
# Appends a line to notes.md (the run's summary).
function Note([string]$Text) { [IO.File]::AppendAllText($script:Notes, "$Text`r`n", $Utf8) }
function Write-Text([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path, $Text, $Utf8) }
# Quotes a path for a single-quoted PowerShell string (doubles any apostrophe).
function Quote([string]$Path) { "'" + ($Path -replace "'", "''") + "'" }
# Quotes one argument for a Windows command line.
function Arg([string]$A) { if ($A -match '[\s"]') { '"' + ($A -replace '"', '\"') + '"' } else { $A } }

# Runs a program with stdout and stderr in files, prints its newest stderr line every
# $Every seconds, and kills it after $TimeoutSec. Returns the exit code ('timeout' if killed).
function Run-Logged([string]$Exe, [string[]]$Arguments, [string]$StdOut, [string]$StdErr,
                    [int]$TimeoutSec, [int]$Every = 10) {
    Say "> $Exe $($Arguments -join ' ')" 'DarkGray'
    $p = Start-Process -FilePath $Exe -ArgumentList (($Arguments | ForEach-Object { Arg $_ }) -join ' ') `
        -RedirectStandardOutput $StdOut -RedirectStandardError $StdErr -NoNewWindow -PassThru
    $null = $p.Handle  # without this, Windows PowerShell can lose the exit code
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = ''
    while (-not $p.WaitForExit($Every * 1000)) {
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

# Exit codes of a crashed process, readable.
function Describe-Exit($Code) {
    if ($Code -is [string]) { return $Code }
    $hex = '0x{0:X8}' -f ([int64]$Code -band 4294967295)
    switch ($hex) {
        '0xC0000005' { return "$Code ($hex, access violation)" }
        '0xC0000409' { return "$Code ($hex, fail-fast / stack buffer overrun)" }
        '0xC00000FD' { return "$Code ($hex, stack overflow)" }
        '0xC0000374' { return "$Code ($hex, heap corruption)" }
        default { if ($Code -lt 0) { return "$Code ($hex)" } else { return "$Code" } }
    }
}

# Keeps the PC (and display) awake while this script runs, so S3's hour isn't cut short.
Add-Type -Namespace JStream -Name Power -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
'@
function Keep-Awake([bool]$On) {
    # ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED, or ES_CONTINUOUS to clear.
    # (A hex literal like 0x80000003 is a negative Int32 in PowerShell; parse it unsigned.)
    $flags = if ($On) { '80000003' } else { '80000000' }
    [void][JStream.Power]::SetThreadExecutionState([Convert]::ToUInt32($flags, 16))
}

# S3's soundtrack: a soft looping chime arpeggio, written as a 48 kHz 16-bit stereo WAV.
Add-Type -TypeDefinition @'
using System; using System.IO;
namespace JStream {
public static class Soundtrack {
    public static void Write(string path) {
        const int rate = 48000, seconds = 24, beat = rate / 2;
        double[] notes = { 261.63, 329.63, 392.00, 523.25, 392.00, 329.63,   // C E G C G E
                           220.00, 261.63, 329.63, 440.00, 329.63, 261.63 }; // A C E A E C
        int frames = rate * seconds;
        using (var w = new BinaryWriter(File.Create(path))) {
            int bytes = frames * 4;
            w.Write("RIFF".ToCharArray()); w.Write(36 + bytes); w.Write("WAVE".ToCharArray());
            w.Write("fmt ".ToCharArray()); w.Write(16); w.Write((short)1); w.Write((short)2);
            w.Write(rate); w.Write(rate * 4); w.Write((short)4); w.Write((short)16);
            w.Write("data".ToCharArray()); w.Write(bytes);
            for (int i = 0; i < frames; i++) {
                double v = 0;
                // Each note rings for two beats, so consecutive notes overlap.
                for (int k = 0; k < 2; k++) {
                    int n = i / beat - k; if (n < 0) continue;
                    double t = (double)(i - n * beat) / rate;
                    double f = notes[n % notes.Length];
                    v += Math.Sin(2 * Math.PI * f * t) * Math.Exp(-3.0 * t) * Math.Min(1.0, t * 200);
                }
                short s = (short)(v * 0.06 * 32767);
                w.Write(s); w.Write(s);
            }
        }
    }
}
}
'@

# Edge (or Chrome), driven with its own throwaway profile so it never touches yours.
function Find-Browser {
    foreach ($p in @(
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe'),
        (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
        (Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'),
        (Join-Path $env:LOCALAPPDATA 'Google\Chrome\Application\chrome.exe'))) {
        if ($p -and (Test-Path $p)) { return $p }
    }
    return $null
}
function Start-Browser([string]$UserDir, [string[]]$Extra) {
    $browser = Find-Browser
    if (-not $browser) { Say 'Neither Edge nor Chrome was found.' 'Yellow'; return $false }
    New-Item -ItemType Directory -Force -Path $UserDir | Out-Null
    $common = @("--user-data-dir=$UserDir", '--no-first-run', '--no-default-browser-check',
                '--disable-sync', '--disable-features=msEdgeFirstRunExperience,Translate',
                '--autoplay-policy=no-user-gesture-required', '--enable-logging', '--v=0')
    Start-Process -FilePath $browser -ArgumentList ((@($common) + @($Extra) | ForEach-Object { Arg $_ }) -join ' ') | Out-Null
    return $true
}
function Stop-Browser([string]$UserDir) {
    Get-CimInstance Win32_Process -Filter "Name='msedge.exe' OR Name='chrome.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($UserDir) } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
}
# The page's console.log lines, which the browser writes to chrome_debug.log with --enable-logging.
function Page-Log([string]$UserDir, [string]$Tag) {
    $log = Join-Path $UserDir 'chrome_debug.log'
    if (-not (Test-Path $log)) { return @() }
    return @(Get-Content $log -ErrorAction SilentlyContinue | Where-Object { $_ -match [regex]::Escape($Tag) })
}
# Copies the first $Bytes of a file (enough of an H.264 stream to decode a few seconds).
function Copy-Head([string]$From, [string]$To, [int]$Bytes) {
    if (-not (Test-Path $From)) { return }
    $in = [IO.File]::OpenRead($From)
    try {
        $buf = New-Object byte[] ([Math]::Min([int64]$Bytes, $in.Length))
        $n = $in.Read($buf, 0, $buf.Length)
        $out = [IO.File]::Create($To)
        try { $out.Write($buf, 0, $n) } finally { $out.Close() }
    } finally { $in.Close() }
}

# ---------------------------------------------------------------------------------------
Step 'Checking prerequisites (anything missing is installed with winget)'

function Have([string]$Cmd) { [bool](Get-Command $Cmd -ErrorAction SilentlyContinue) }
function Refresh-Path {
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
                [Environment]::GetEnvironmentVariable('Path', 'User') + ';' +
                (Join-Path $env:USERPROFILE '.cargo\bin')
}
function Winget-Install([string]$Id, [string]$Extra = '') {
    if (-not (Have 'winget')) { throw "winget isn't available. Install $Id by hand, then run this again." }
    $wingetArgs = "install --id $Id -e --accept-package-agreements --accept-source-agreements $Extra"
    Say "winget $wingetArgs"
    $p = Start-Process winget -ArgumentList $wingetArgs -Wait -PassThru -NoNewWindow
    if ($p.ExitCode -ne 0) { Say "winget exited with $($p.ExitCode); continuing to check." 'Yellow' }
    Refresh-Path
}

$build = [Environment]::OSVersion.Version.Build
Say "Windows build $build"
if ($build -lt 22000 -and $Only -contains 'S2') { Say 'S2 needs Windows 11 (build 22000+); skipping it.' 'Yellow'; $Only = @($Only | Where-Object { $_ -ne 'S2' }) }

if (-not (Have 'git')) { Winget-Install 'Git.Git' }
if (-not (Have 'git')) { throw 'Git is still missing after installing it. Open a new terminal and run this again.' }

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$hasVc = $false
if (Test-Path $vswhere) {
    $vc = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $hasVc = -not [string]::IsNullOrWhiteSpace($vc)
}
if (-not $hasVc) {
    Say 'Installing the MSVC C++ build tools (needed by Rust on Windows; several GB).' 'Yellow'
    Winget-Install 'Microsoft.VisualStudio.2022.BuildTools' '--override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
}

if (-not (Have 'cargo')) { Refresh-Path }
if (-not (Have 'rustup')) { Winget-Install 'Rustlang.Rustup' }
if (-not (Have 'cargo')) { throw 'cargo still not found. Open a new terminal (so PATH refreshes) and run this again.' }

# ---------------------------------------------------------------------------------------
Step 'Finding and updating the JStream checkout'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$candidate = Split-Path -Parent $here
if (Test-Path (Join-Path $candidate 'spikes\s1-gpu\Cargo.toml')) {
    $Repo = $candidate
} elseif (Test-Path (Join-Path $RepoDir '.git')) {
    $Repo = $RepoDir
} else {
    Say "Cloning $RepoUrl to $RepoDir"
    git clone $RepoUrl $RepoDir
    if ($LASTEXITCODE -ne 0) { throw "git clone of $RepoUrl failed (exit $LASTEXITCODE)" }
    $Repo = $RepoDir
}
Push-Location $Repo
try {
    git checkout main 2>&1 | Out-Host
    git pull --ff-only 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Say 'git pull failed; using the checkout as it is.' 'Yellow' }
    $commit = (git rev-parse --short HEAD)
} finally { Pop-Location }
Say "Using $Repo at $commit"

# rust-toolchain.toml pins the toolchain; install exactly that one.
Set-Location $Repo
$pinned = (Select-String -Path (Join-Path $Repo 'rust-toolchain.toml') -Pattern 'channel\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
Say "Installing/checking Rust $pinned"
rustup toolchain install $pinned --profile minimal --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw "rustup couldn't install $pinned" }

# Builds a spike, logging the output to <name>-build.txt. Returns $true on success.
function Build-Spike([string]$Dir, [string]$Name) {
    Push-Location $Dir
    try {
        Say "Building $Name (cargo build --release)..."
        cargo build --release 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath (Join-Path $Out "$Name-build.txt") |
            Where-Object { $_ -match '^(error|warning: unused)|Finished|Compiling (s\d)' } | Out-Host
        return ($LASTEXITCODE -eq 0)
    } finally { Pop-Location }
}

# ---------------------------------------------------------------------------------------
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$Out = Join-Path $Repo "spike-results\$stamp"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$script:Notes = Join-Path $Out 'notes.md'
Write-Text $Notes "# Spike run $stamp`r`n`r`nRepo commit: $commit. Spikes: $($Only -join ', '). Run unattended by run-spikes.ps1.`r`n"
$Scratch = Join-Path $env:TEMP "jstream-spikes-$stamp"
New-Item -ItemType Directory -Force -Path $Scratch | Out-Null

try {
Keep-Awake $true
Step 'Collecting system info'
Add-Type -AssemblyName System.Windows.Forms
$sys = [ordered]@{
    windows  = (Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber)
    cpu      = (Get-CimInstance Win32_Processor | Select-Object -First 1 Name).Name
    gpus     = @(Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion, CurrentHorizontalResolution, CurrentVerticalResolution, CurrentRefreshRate)
    screens  = @([System.Windows.Forms.Screen]::AllScreens | ForEach-Object {
                   [ordered]@{ name = $_.DeviceName; primary = $_.Primary; bounds = "$($_.Bounds.Width)x$($_.Bounds.Height) at $($_.Bounds.X),$($_.Bounds.Y)"; bits = $_.BitsPerPixel } })
    monitors = @(Get-CimInstance -Namespace root\wmi WmiMonitorID -ErrorAction SilentlyContinue | ForEach-Object {
                   (($_.UserFriendlyName | Where-Object { $_ -ne 0 } | ForEach-Object { [char]$_ }) -join '') })
    audio_endpoints = @(Get-PnpDevice -Class AudioEndpoint -Status OK -ErrorAction SilentlyContinue | ForEach-Object FriendlyName)
    cameras  = @(Get-PnpDevice -Class Camera, Image -Status OK -ErrorAction SilentlyContinue | ForEach-Object FriendlyName)
    laptop   = [bool](Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue)
    power_plan = ((powercfg /getactivescheme) -join ' ')
    browser  = (Find-Browser)
    rustc    = (rustc --version)
}
Write-Text (Join-Path $Out 'system.json') ($sys | ConvertTo-Json -Depth 4)
Say "Saved system.json (GPUs: $(($sys.gpus | ForEach-Object Name) -join ', '))"

# ---------------------------------------------------------------------------------------
if ($Only -contains 'S1') {
    Step 'S1: GPU interop (capture -> wgpu -> NV12 -> hardware H.264), 2 runs of 30 s'
    Note "`r`n## S1`r`n"
    $dir = Join-Path $Repo 'spikes\s1-gpu'
    $s1Profile = Join-Path $Scratch 's1-browser'
    try {
        if (-not (Build-Spike $dir 's1')) { throw 'cargo build failed; see s1-build.txt' }
        $exe = Join-Path $dir 'target\release\s1-gpu.exe'

        # Something moving at 60 fps on the primary display: a full-screen animated page.
        $page = Join-Path $Scratch 's1-motion.html'
        Write-Text $page @'
<!doctype html><html><head><meta charset="utf-8"><title>S1 motion</title>
<style>html,body{margin:0;height:100%;background:#000;overflow:hidden}canvas{display:block;width:100vw;height:100vh}</style></head>
<body><canvas id="c"></canvas><script>
const c=document.getElementById('c'),g=c.getContext('2d');let n=0;
function size(){c.width=innerWidth*devicePixelRatio;c.height=innerHeight*devicePixelRatio}
addEventListener('resize',size);size();
function draw(t){const w=c.width,h=c.height;n++;
 // Colour bars (checks the BT.709 conversion), a sweeping bar and a frame counter.
 const bars=['#c0c0c0','#c0c000','#00c0c0','#00c000','#c000c0','#c00000','#0000c0'];
 bars.forEach((b,i)=>{g.fillStyle=b;g.fillRect(i*w/7,0,w/7+1,h*0.6)});
 const grad=g.createLinearGradient(0,0,w,0);grad.addColorStop(0,'#000');grad.addColorStop(1,'#fff');
 g.fillStyle=grad;g.fillRect(0,h*0.6,w,h*0.15);
 g.fillStyle='#101010';g.fillRect(0,h*0.75,w,h*0.25);
 const x=(t/1000*0.5%1)*w;g.fillStyle='#ffffff';g.fillRect(x,h*0.6,w/40,h*0.4);
 g.fillStyle='#ff8000';g.beginPath();g.arc(w/2+Math.cos(t/400)*w/4,h*0.3+Math.sin(t/400)*h/5,h/12,0,7);g.fill();
 g.fillStyle='#fff';g.font=(h/12|0)+'px monospace';g.fillText('frame '+n,w*0.05,h*0.92);
 requestAnimationFrame(draw)}
requestAnimationFrame(draw);
</script></body></html>
'@
        $shown = Start-Browser $s1Profile @('--kiosk', ([Uri]$page).AbsoluteUri, '--edge-kiosk-type=fullscreen', '--window-position=0,0')
        Note "- Motion source: full-screen animated page in $(if ($shown) { Split-Path -Leaf (Find-Browser) } else { 'nothing (no browser found)' })"
        Start-Sleep -Seconds 8

        foreach ($run in @(@{ name = 's1'; size = '1920x1080' }, @{ name = 's1-native'; size = 'native' })) {
            $h264 = Join-Path $Scratch "$($run.name).h264"
            $code = Run-Logged $exe @('--seconds', '30', '--json', '--size', $run.size, '--out', $h264) `
                (Join-Path $Out "$($run.name).json") (Join-Path $Out "$($run.name).stderr.txt") 150
            $desc = Describe-Exit $code
            Say "$($run.name) ($($run.size)): exit $desc" $(if ($code -eq 0) { 'Green' } else { 'Red' })
            Note "- $($run.name) ($($run.size)): exit code $desc"
            if (Test-Path $h264) {
                Note "  - H.264 output: $((Get-Item $h264).Length) bytes; first 6 MB in $($run.name)-head.h264"
                Copy-Head $h264 (Join-Path $Out "$($run.name)-head.h264") 6MB
            }
            Start-Sleep -Seconds 2
        }
    } catch {
        Say "S1 failed: $_" 'Red'; Note "- S1 failed: $_"
    } finally {
        Stop-Browser $s1Profile
    }
}

# ---------------------------------------------------------------------------------------
if ($Only -contains 'S2') {
    Step 'S2: virtual camera (per user, then machine-wide)'
    Note "`r`n## S2`r`n"
    $dir = Join-Path $Repo 'spikes\s2-vcam'
    $exe = Join-Path $dir 'target\release\s2-vcam.exe'
    $usedHklm = $false

    # A small window that opens "JStream S2" as a webcam and logs what it sees.
    $page = Join-Path $Scratch 's2-consumer.html'
    Write-Text $page @'
<!doctype html><html><head><meta charset="utf-8"><title>S2 consumer</title></head>
<body style="margin:0;background:#222;color:#eee;font:14px sans-serif">
<video id="v" autoplay muted playsinline width="480" height="270"></video><div id="s"></div>
<canvas id="k" width="64" height="36" style="display:none"></canvas><script>
const log=m=>{console.log('S2PAGE '+m);document.getElementById('s').textContent=m};
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{
 try{(await navigator.mediaDevices.getUserMedia({video:true})).getTracks().forEach(t=>t.stop())}
 catch(e){log('first getUserMedia: '+e.name+' '+e.message)}
 let cam=null;
 for(let i=0;i<15&&!cam;i++){
  const d=(await navigator.mediaDevices.enumerateDevices()).filter(x=>x.kind==='videoinput');
  if(i===0)log('cameras: '+JSON.stringify(d.map(x=>x.label)));
  cam=d.find(x=>/JStream/i.test(x.label));if(!cam)await sleep(2000)}
 if(!cam){log('RESULT no JStream camera listed');return}
 let s;try{s=await navigator.mediaDevices.getUserMedia({video:{deviceId:{exact:cam.deviceId}}})}
 catch(e){log('RESULT open failed: '+e.name+' '+e.message);return}
 const v=document.getElementById('v');v.srcObject=s;
 const st=s.getVideoTracks()[0].getSettings();log('opened '+cam.label+' '+st.width+'x'+st.height+'@'+st.frameRate);
 const k=document.getElementById('k').getContext('2d',{willReadFrequently:true});let last=null,changes=0,frames=0;
 for(let i=0;i<30;i++){await sleep(500);if(v.readyState<2)continue;frames++;
  k.drawImage(v,0,0,64,36);const p=k.getImageData(0,0,64,36).data;
  const mid=[p[(18*64+2)*4],p[(18*64+2)*4+1],p[(18*64+2)*4+2]];
  let sum=0;for(let j=0;j<p.length;j+=16)sum=(sum*31+p[j])>>>0;
  if(last!==null&&sum!==last)changes++;last=sum;
  if(i%6===0)log('sample '+i+' pixel(2,18)='+mid.join(',')+' size='+v.videoWidth+'x'+v.videoHeight)}
 log('RESULT frames_seen='+frames+' picture_changes='+changes);
 s.getTracks().forEach(t=>t.stop());
})();
</script></body></html>
'@

    # One attempt: start the producer, open the consumer page, wait, collect.
    function S2-Attempt([string]$Mode) {
        $userDir = Join-Path $Scratch "s2-browser-$Mode"
        $json = Join-Path $Out "s2-$Mode.json"
        $p = Start-Process -FilePath $exe -ArgumentList '--json --seconds 45' -NoNewWindow -PassThru `
            -RedirectStandardOutput $json -RedirectStandardError (Join-Path $Out "s2-$Mode.stderr.txt")
        $null = $p.Handle
        Start-Sleep -Seconds 6
        $opened = Start-Browser $userDir @("--app=$(([Uri]$page).AbsoluteUri)", '--use-fake-ui-for-media-stream',
                                           '--window-position=40,40', '--window-size=520,360')
        if (-not $opened) { Start-Process 'microsoft.windows.camera:' -ErrorAction SilentlyContinue }
        if (-not $p.WaitForExit(90000)) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
        Stop-Browser $userDir
        $pageLines = Page-Log $userDir 'S2PAGE'
        [IO.File]::WriteAllLines((Join-Path $Out "s2-$Mode-page.txt"), [string[]]@($pageLines), $Utf8)
        $report = $null
        try { $report = Get-Content $json -Raw | ConvertFrom-Json } catch { }
        $shared = if ($report) { [int64]$report.consumer.frames_shared_this_run } else { 0 }
        $loaded = $report -and $report.frame_server.loaded
        $result = ($pageLines | Where-Object { $_ -match 'RESULT' } | Select-Object -Last 1)
        Note "- $Mode : exit $(Describe-Exit $p.ExitCode); Frame Server loaded the DLL: $loaded; frames shared: $shared; vcam start: $(if ($report) { $report.vcam.start.hr } else { '?' })"
        Note "  - consumer page: $(if ($result) { $result -replace '^.*S2PAGE ', '' } else { 'no result logged' })"
        Say "S2 $Mode : Frame Server loaded: $loaded, frames shared: $shared" $(if ($shared -gt 0) { 'Green' } else { 'Yellow' })
        return ($shared -gt 0)
    }

    try {
        if (-not (Build-Spike $dir 's2')) { throw 'cargo build failed (if the DLL is in use, run "net stop FrameServer" elevated and retry); see s2-build.txt' }
        & $exe --unregister | Out-Null
        & $exe --register hkcu 2>&1 | Out-File -Encoding utf8 (Join-Path $Out 's2-register-hkcu.txt')
        [void](S2-Attempt 'hkcu')
        & $exe --unregister | Out-Null

        Say 'Machine-wide registration needs administrator rights: approve the UAC prompt (and the one at the end that removes it).' 'Yellow'
        $log = Join-Path $Out 's2-register-hklm.txt'
        $cmd = "Stop-Service FrameServer -Force -ErrorAction SilentlyContinue; & $(Quote $exe) --register hklm *> $(Quote $log); Start-Sleep -Seconds 2"
        $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
        try {
            Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList "-NoProfile -EncodedCommand $enc" -ErrorAction Stop
            $usedHklm = $true
            [void](S2-Attempt 'hklm')
        } catch { Note "- hklm: not run (UAC declined or failed: $_)" }
    } catch {
        Say "S2 failed: $_" 'Red'; Note "- S2 failed: $_"
    } finally {
        if ($usedHklm) {
            $cmd = "& $(Quote $exe) --unregister; Start-Sleep -Seconds 1"
            $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
            try {
                Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList "-NoProfile -EncodedCommand $enc" -ErrorAction Stop
            } catch {
                Say "Removing the machine-wide registration failed (UAC declined?): $_" 'Red'
                Note "- cleanup: removing the HKLM registration FAILED ($_); run 's2-vcam --unregister' as administrator"
            }
        } elseif (Test-Path $exe) { & $exe --unregister | Out-Null }
        # What is still registered after cleanup (should be nothing).
        if (Test-Path $exe) { & $exe --status 2>&1 | ForEach-Object { "$_" } | Out-File -Encoding utf8 (Join-Path $Out 's2-status-after.txt') }
        Copy-Item 'C:\ProgramData\JStreamS2\*.log' $Out -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------------------
if ($Only -contains 'S3') {
    Step "S3: media clock and audio drift (2-minute smoke run, then $S3Minutes minutes)"
    Note "`r`n## S3`r`n"
    $dir = Join-Path $Repo 'spikes\s3-clock'
    try {
        if (-not (Build-Spike $dir 's3')) { throw 'cargo build failed; see s3-build.txt' }
        $exe = Join-Path $dir 'target\release\s3-clock.exe'
        $player = $null
        if ($S3Silent) {
            Say 'Playing nothing (-S3Silent): only the spike''s silent keep-alive runs.' 'Yellow'
            Note '- playback: none (-S3Silent); only the keep-alive ran, which is not the realistic case'
        } else {
            $wav = Join-Path $Scratch 's3-soundtrack.wav'
            [JStream.Soundtrack]::Write($wav)
            $player = New-Object Media.SoundPlayer $wav
            $player.PlayLooping()
            Say 'A quiet chime loop plays on the default output during S3 (rerun with -S3Silent to skip it). Leave the audio devices as they are.' 'Yellow'
            Note '- playback: generated 24 s chime loop (48 kHz stereo, about -24 dBFS) on the default output, via System.Media.SoundPlayer, for the whole S3 run'
        }
        $code = Run-Logged $exe @('--minutes', '2', '--json', '--csv', (Join-Path $Out 's3-smoke.csv')) `
            (Join-Path $Out 's3-smoke.json') (Join-Path $Out 's3-smoke.stderr.txt') 300
        Note "- smoke (2 min): exit code $(Describe-Exit $code)"
        if ($code -eq 0) {
            Say "Full run: $S3Minutes minutes. The PC is kept awake. Progress prints every 30 s."
            $code = Run-Logged $exe @('--minutes', "$S3Minutes", '--json', '--csv', (Join-Path $Out 'drift.csv')) `
                (Join-Path $Out 's3.json') (Join-Path $Out 's3.stderr.txt') ($S3Minutes * 60 + 600) 30
            Note "- full ($S3Minutes min): exit code $(Describe-Exit $code)"
        } else {
            Say 'The smoke run failed, so the full run is skipped; see s3-smoke.stderr.txt.' 'Yellow'
            Note '- full run skipped because the smoke run failed'
        }
    } catch {
        Say "S3 failed: $_" 'Red'; Note "- S3 failed: $_"
    } finally {
        if ($player) { $player.Stop(); $player.Dispose() }
    }
}

} finally {
# ---------------------------------------------------------------------------------------
# Runs even if the script is stopped early (Ctrl+C, an error), so partial results still
# get zipped.
Keep-Awake $false
Step 'Packaging results'
$zip = "$Out.zip"
$files = Get-ChildItem $Out -File
if ($files) { Compress-Archive -Path $files.FullName -DestinationPath $zip -Force }
Remove-Item $Scratch -Recurse -Force -ErrorAction SilentlyContinue
Say "Results folder: $Out" 'Green'
Say "Zip to send back: $zip" 'Green'
Say 'Attach the zip in the chat.' 'Green'
Start-Process explorer.exe "/select,`"$zip`""
}
