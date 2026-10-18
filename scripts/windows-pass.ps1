<#
.SYNOPSIS
The Windows hardware pass: runs every open Windows hardware check from one script, writes
one report and prints what to send back.

.DESCRIPTION
The checks, what each one passes on and the issues they answer are in
docs/runbooks/windows-hardware-pass.md. This script runs them in order. Each check runs
from the commit it needs (main, a wave or an open PR's branch), checked out into its own
git worktree under -WorkDir, so your own checkout, its branch and its files are never
touched. Each worktree keeps its build, so a second run is quick.

What it never does without asking you first, at that step: change a system setting,
register or unregister anything, run anything as administrator, reset the GPU, install a
tool, or read a stream key. Decline and that step is recorded as skipped. -Unattended
declines every such step and every question, so only the automatic checks run.

Steps that need your eyes (the Camera app showing frames, YouTube Studio going live) ask
a pass, fail or skip question where they happen.

At the end it writes report.md and results.json into the run folder, zips them with the
logs (redacted of the run's tokens; recordings and token files stay out), and prints the
zip's path and a summary to paste back.

Runs on Windows PowerShell 5.1 and PowerShell 7.

.PARAMETER Only
Run only these checks, by id (-List shows them). Example: -Only render,nv12

.PARAMETER Skip
Leave these checks out, by id.

.PARAMETER List
Print the checks, the commit each one runs from and what it needs, then exit.

.PARAMETER Unattended
Answer every question with skip and decline every step that needs a yes.

.PARAMETER NoBuild
Use the builds already in the worktrees; don't fetch or build.

.PARAMETER WorkDir
Where the worktrees and their builds live. Default %LOCALAPPDATA%\JStream-windows-pass.

.PARAMETER OutDir
Where this run's report and logs go. Default a new folder under -WorkDir\runs.

.PARAMETER YouTubeUrl
The YouTube ingest server URL (rtmps://..., without the key), for the youtube check.

.PARAMETER YouTubeKeyFile
The file holding the YouTube stream key, for the youtube check. Only its path is passed
on; the key is read only if you agree to search the logs for it.
#>
[CmdletBinding()]
param(
    [string[]]$Only,
    [string[]]$Skip,
    [switch]$List,
    [switch]$Unattended,
    [switch]$NoBuild,
    [string]$WorkDir,
    [string]$OutDir,
    [string]$YouTubeUrl,
    [string]$YouTubeKeyFile
)

Set-StrictMode -Version 3
# 'Continue', not 'Stop': in Windows PowerShell 5.1, 'Stop' turns a native program's
# stderr (cargo's progress) into a terminating error.
$ErrorActionPreference = 'Continue'

if (-not (($PSVersionTable.PSEdition -eq 'Desktop') -or $IsWindows)) {
    Write-Error 'The Windows hardware pass runs on Windows only'; exit 2
}

$Repo = Split-Path -Parent $PSScriptRoot
if (-not $WorkDir) { $WorkDir = Join-Path $env:LOCALAPPDATA 'JStream-windows-pass' }
$Utf8 = New-Object System.Text.UTF8Encoding $false
# From a .bat or cmd, "-Only a,b" arrives as one string.
$Only = @($Only | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim().ToLower() } | Where-Object { $_ })
$Skip = @($Skip | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim().ToLower() } | Where-Object { $_ })

# --- The commits the checks run from ---------------------------------------------------

# name = the refspec fetched from origin. A PR's head stays fetchable after it merges.
$Sources = [ordered]@{
    'main'  = 'main'
    'm1-3'  = 'wave/m1-3'
    'pr574' = 'pull/574/head'
    'pr726' = 'pull/726/head'
    'pr685' = 'pull/685/head'
    'pr573' = 'pull/573/head'
}

# --- The checks --------------------------------------------------------------------------

# Id, the source it runs from, the issues it answers, what it needs, and its function
# (Test-<Fn>, below). In run order: grouped by source, so each source builds once.
$Tasks = @(
    @{ Id = 'smoke';          Src = 'main';  Issues = 'go-live runbook'; Fn = 'Smoke';         Title = 'Go-live smoke test and graceful stop';          Needs = 'ffmpeg' }
    @{ Id = 'render';         Src = 'main';  Issues = '#336';            Fn = 'Render';        Title = 'Render pixel tests on the hardware adapter';    Needs = '' }
    @{ Id = 'nv12';           Src = 'main';  Issues = '#492';            Fn = 'Nv12';          Title = 'NV12 encode targets into Media Foundation';     Needs = 'ffprobe' }
    @{ Id = 'media';          Src = 'main';  Issues = '#382';            Fn = 'Media';         Title = 'Looping and MP3 media sound';                   Needs = 'ffmpeg, ffprobe' }
    @{ Id = 'browser';        Src = 'main';  Issues = '#341';            Fn = 'Browser';       Title = 'Browser page check, refused requests';          Needs = 'MSVC tools, CMake, Ninja' }
    @{ Id = 'youtube';        Src = 'main';  Issues = '#337 (part)';     Fn = 'YouTube';       Title = 'YouTube live without OAuth';                    Needs = '-YouTubeUrl, -YouTubeKeyFile, you in YouTube Studio' }
    @{ Id = 'preview';        Src = 'main';  Issues = '#380';            Fn = 'Preview';       Title = 'Canvas H.264 preview in the desktop app';       Needs = 'trunk, wasm-bindgen, you' }
    @{ Id = 'vcam-owner';     Src = 'm1-3';  Issues = '#546 (#540)';     Fn = 'VcamOwner';     Title = 'Virtual camera owner and DACL checks';          Needs = 'administrator (UAC), Camera app, you' }
    @{ Id = 'token-acl';      Src = 'm1-3';  Issues = '#546 (#548)';     Fn = 'TokenAcl';      Title = 'User-only token and log files';                 Needs = '' }
    @{ Id = 'crash';          Src = 'm1-3';  Issues = '#557, #546 (#662)'; Fn = 'Crash';       Title = 'Crash reports: Windows build and tests';        Needs = '' }
    @{ Id = 'vcam-register';  Src = 'm1-3';  Issues = '#563';            Fn = 'VcamRegister';  Title = 'Opt-in virtual camera registration';            Needs = 'administrator (UAC), Camera app, you' }
    @{ Id = 'gpu-reset';      Src = 'm1-3';  Issues = '#576, #620';      Fn = 'GpuReset';      Title = 'Mid-run GPU fallback and device-loss recovery'; Needs = 'dxcap (Graphics Tools), administrator (UAC)' }
    @{ Id = 'private-files';  Src = 'pr574'; Issues = '#546 (#574)';     Fn = 'PrivateFiles';  Title = 'User-only audit log and collection file';       Needs = '' }
    @{ Id = 'webcam';         Src = 'pr726'; Issues = '#673, #726';      Fn = 'Webcam';        Title = 'MJPEG webcam and its paired microphone';        Needs = 'a webcam (with a microphone), ffprobe, you' }
    @{ Id = 'cursor';         Src = 'pr685'; Issues = '#690 (part)';     Fn = 'Cursor';        Title = 'CursorCapture::Hidden unit tests';              Needs = '' }
    @{ Id = 'clap';           Src = 'pr573'; Issues = '#573 (#342 re-run)'; Fn = 'Clap';       Title = 'CLAP plugin host with the unnamed shared block'; Needs = 'a .clap effect in Common Files\CLAP' }
)

# Open hardware items this pass can't run, and why. They go into the report as is.
$NotRun = @(
    '#333: A3 on a Windows 11 Pro N VM. The maintainer decided not to set one up.'
    '#335 step 5: the graceful stop with CEF running waits on the fix for #436.'
    '#443: hardware encoders on Intel and NVIDIA. This machine has an AMD GPU.'
    '#357: the transition node''s per-tick cost. No bench measures it yet.'
    '#557 and #546 (#662): a crash report from a real panic. Neither binary can be made to panic on purpose, so only the crash crate''s own tests run.'
    '#690: the WGC captures with the pointer hidden. Only the unit tests can run until the daemon has show_cursor (#682).'
    '#546: audit.jsonl rotation past 8 MiB and the .devices file after a real pairing. Both are by hand, in the runbook.'
)

if ($List) {
    '{0,-15} {1,-7} {2,-20} {3}' -f 'id', 'source', 'issues', 'needs'
    foreach ($t in $Tasks) { '{0,-15} {1,-7} {2,-20} {3}' -f $t.Id, $t.Src, $t.Issues, $t.Needs }
    ''
    'Sources: ' + (($Sources.Keys | ForEach-Object { "$_ = origin/$($Sources[$_])" }) -join ', ')
    exit 0
}
$unknown = @($Only + $Skip | Where-Object { $id = $_; -not ($Tasks | Where-Object { $_.Id -eq $id }) })
if ($unknown) { Write-Error "unknown check: $($unknown -join ', ') (see -List)"; exit 2 }
$Selected = @($Tasks | Where-Object { (-not $Only -or $Only -contains $_.Id) -and -not ($Skip -contains $_.Id) })
if ($YouTubeKeyFile) {
    if (-not (Test-Path -LiteralPath $YouTubeKeyFile -PathType Leaf)) { Write-Error "-YouTubeKeyFile: no such file: $YouTubeKeyFile"; exit 2 }
    $YouTubeKeyFile = (Resolve-Path -LiteralPath $YouTubeKeyFile).Path
}

# --- Output ------------------------------------------------------------------------------

if (-not $OutDir) { $OutDir = Join-Path $WorkDir ('runs\' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path
$Transcript = Join-Path $OutDir 'console.log'
try { Start-Transcript -LiteralPath $Transcript -Append | Out-Null } catch {}

function Say([string]$Text, [string]$Color = 'Gray') { Write-Host $Text -ForegroundColor $Color }
function Section([string]$Text) { Write-Host ''; Write-Host "=== $Text ===" -ForegroundColor Cyan }

$Results = New-Object System.Collections.Generic.List[object]
$script:Task = $null
# Tokens the run's daemons wrote: redacted from everything that goes into the zip.
$script:Secrets = New-Object System.Collections.Generic.List[string]

function Add-Result([string]$Status, [string]$Check, [string]$Detail) {
    $id = '(run)'
    if ($script:Task) { $id = $script:Task.Id }
    $Results.Add([pscustomobject]@{ Task = $id; Status = $Status; Check = $Check; Detail = $Detail })
    $color = @{ PASS = 'Green'; FAIL = 'Red'; WARN = 'Yellow'; SKIP = 'DarkGray' }[$Status]
    Write-Host ('{0,-4} {1}: {2}' -f $Status, $Check, $Detail) -ForegroundColor $color
}
function Pass([string]$Check, [string]$Detail) { Add-Result 'PASS' $Check $Detail }
function Fail([string]$Check, [string]$Detail) { Add-Result 'FAIL' $Check $Detail }
function Warn([string]$Check, [string]$Detail) { Add-Result 'WARN' $Check $Detail }
function Skip([string]$Check, [string]$Detail) { Add-Result 'SKIP' $Check $Detail }
# PASS when $Ok, else FAIL.
function Check([bool]$Ok, [string]$Check, [string]$Detail) {
    if ($Ok) { Pass $Check $Detail } else { Fail $Check $Detail }
}

# A check only a person can judge: asks pass, fail or skip, and records the answer.
function Ask([string]$Check, [string]$Question) {
    if ($Unattended) { Skip $Check 'needs a person (-Unattended)'; return }
    Write-Host ''
    Write-Host "  ? $Question" -ForegroundColor White
    while ($true) {
        $a = (Read-Host '    [p]ass, [f]ail or [s]kip').Trim().ToLower()
        if ($a -in @('p', 'pass')) { Pass $Check "you: $Question"; return }
        if ($a -in @('s', 'skip')) { Skip $Check 'skipped by you'; return }
        if ($a -in @('f', 'fail')) {
            $why = Read-Host '    What did you see? (one line, Enter for none)'
            Fail $Check "you: $why"; return
        }
    }
}

# A step that changes something on this machine: runs only on a yes.
function Confirm([string]$What) {
    if ($Unattended) { return $false }
    Write-Host ''
    Write-Host "  ! $What" -ForegroundColor Yellow
    $a = (Read-Host '    Go ahead? [y/N]').Trim().ToLower()
    $a -in @('y', 'yes')
}

# Something you do by hand, then press Enter.
function Prompt-Do([string]$What) {
    if ($Unattended) { return $false }
    Write-Host ''
    Write-Host "  > $What" -ForegroundColor White
    $a = Read-Host '    Press Enter when done, or type s to skip'
    $a.Trim().ToLower() -ne 's'
}

function Write-Text([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path, $Text, $Utf8) }

# --- Native processes --------------------------------------------------------------------

# Quotes one argument the way the C runtime splits a command line.
function Format-Arg([string]$a) {
    if ($a -ne '' -and $a -notmatch '[\s"]') { return $a }
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('"')
    $slashes = 0
    foreach ($c in $a.ToCharArray()) {
        if ($c -eq '\') { $slashes++; continue }
        if ($c -eq '"') { [void]$sb.Append([char]'\', 2 * $slashes + 1) }
        elseif ($slashes) { [void]$sb.Append([char]'\', $slashes) }
        $slashes = 0
        [void]$sb.Append($c)
    }
    [void]$sb.Append([char]'\', 2 * $slashes)
    [void]$sb.Append('"')
    $sb.ToString()
}

# Starts a process with its own hidden console and piped stdout and stderr.
function Start-Native([string]$File, [string[]]$Arguments, [string]$Dir) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $File
    $psi.Arguments = ($Arguments | ForEach-Object { Format-Arg $_ }) -join ' '
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    if ($Dir) { $psi.WorkingDirectory = $Dir }
    [System.Diagnostics.Process]::Start($psi)
}

# Runs a process to completion. Returns its exit code, stdout and stderr; a timeout kills
# it and gives code 'timeout'.
function Invoke-Native([string]$File, [string[]]$Arguments, [int]$TimeoutSec = 120, [string]$Dir) {
    $p = Start-Native $File $Arguments $Dir
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    $code = 'timeout'
    if ($p.WaitForExit($TimeoutSec * 1000)) { $p.WaitForExit(); $code = $p.ExitCode } else { try { $p.Kill() } catch {} }
    [pscustomobject]@{ Code = $code; Out = $out.Result; Err = $err.Result }
}

# Runs a long command (a build, a test run) with stdout then stderr in $Log, printing its
# newest stderr line every 15 s. Returns the exit code, or 'timeout'.
function Invoke-Logged([string]$File, [string[]]$Arguments, [string]$Log, [string]$Dir, [int]$TimeoutSec = 7200) {
    Say "> $File $($Arguments -join ' ')   (log: $Log)" 'DarkGray'
    $out = "$Log.out"; $err = "$Log.err"
    if (-not $Dir) { $Dir = $OutDir }
    $p = Start-Process -FilePath $File -ArgumentList (($Arguments | ForEach-Object { Format-Arg $_ }) -join ' ') `
        -WorkingDirectory $Dir -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
    # Windows PowerShell 5.1 only reports ExitCode for a process whose handle was taken.
    $null = $p.Handle
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = ''
    $code = 'timeout'
    while ($true) {
        if ($p.WaitForExit(15000)) { $p.WaitForExit(); $code = $p.ExitCode; break }
        if ((Get-Date) -gt $deadline) { try { $p.Kill() } catch {}; break }
        $line = Get-Content -LiteralPath $err -Tail 1 -ErrorAction SilentlyContinue
        if ($line -and $line -ne $last) { $last = $line; Say "  ... $($line.Trim())" 'DarkGray' }
    }
    $text = ''
    foreach ($f in @($out, $err)) {
        if (Test-Path -LiteralPath $f) { $text += [IO.File]::ReadAllText($f); Remove-Item -Force -LiteralPath $f -ErrorAction SilentlyContinue }
    }
    Write-Text $Log $text
    $code
}

function Stop-Native($p) {
    if ($p -and -not $p.HasExited) { try { $p.Kill() } catch {}; [void]$p.WaitForExit(10000) }
}

# Sends Ctrl-C to a process started by Start-Native (it has its own console), from a
# helper process that attaches to that console. This is the daemon's graceful stop.
function Send-CtrlC([int]$TargetPid) {
    $code = @"
Add-Type -Namespace JStreamPass -Name Con -MemberDefinition '
[DllImport("kernel32.dll")] public static extern bool FreeConsole();
[DllImport("kernel32.dll")] public static extern bool AttachConsole(uint p);
[DllImport("kernel32.dll")] public static extern bool SetConsoleCtrlHandler(System.IntPtr h, bool add);
[DllImport("kernel32.dll")] public static extern bool GenerateConsoleCtrlEvent(uint e, uint g);'
[void][JStreamPass.Con]::FreeConsole()
if (-not [JStreamPass.Con]::AttachConsole($TargetPid)) { exit 3 }
[void][JStreamPass.Con]::SetConsoleCtrlHandler([IntPtr]::Zero, `$true)
if (-not [JStreamPass.Con]::GenerateConsoleCtrlEvent(0, 0)) { exit 4 }
Start-Sleep -Milliseconds 500
exit 0
"@
    $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
    $r = Invoke-Native (Get-PowerShellExe) @('-NoProfile', '-NonInteractive', '-EncodedCommand', $enc) 60
    $r.Code -eq 0
}

function Get-PowerShellExe { (Get-Process -Id $PID).Path }

# Runs PowerShell commands as administrator (one UAC prompt), with their output in $Log.
# Returns the exit code, or $null when the prompt was declined.
$script:ElevatedN = 0
function Invoke-Elevated([string]$Commands, [string]$Log) {
    $script:ElevatedN++
    $file = Join-Path $OutDir "elevated-$($script:ElevatedN).ps1"
    $quotedLog = "'" + ($Log -replace "'", "''") + "'"
    Write-Text $file "`$ErrorActionPreference = 'Continue'`r`n& {`r`n$Commands`r`n} *>&1 | Out-File -LiteralPath $quotedLog -Encoding utf8`r`nexit `$LASTEXITCODE`r`n"
    try {
        $p = Start-Process -FilePath (Get-PowerShellExe) -Verb RunAs -Wait -PassThru `
            -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Format-Arg $file))
        return $p.ExitCode
    } catch {
        return $null
    }
}

function Find-Tool([string]$Name) {
    $c = Get-Command $Name -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $null
}

# --- Files and ACLs ------------------------------------------------------------------------

$UserSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$SidNames = @{ 'S-1-5-18' = 'SYSTEM'; 'S-1-5-32-544' = 'Administrators'; 'S-1-5-19' = 'LOCAL SERVICE'; 'S-1-5-32-545' = 'Users'; 'S-1-1-0' = 'Everyone' }
$SidNames[$UserSid] = 'you'

# The file's DACL as text, and whether it is protected and grants only $Allowed SIDs.
function Test-Acl([string]$Path, [string[]]$Allowed) {
    $acl = Get-Acl -LiteralPath $Path
    $rules = @($acl.Access)
    $inherited = @($rules | Where-Object { $_.IsInherited })
    $sids = @($rules | ForEach-Object {
        try { $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value } catch { "$($_.IdentityReference)" }
    } | Select-Object -Unique)
    $extra = @($sids | Where-Object { $Allowed -notcontains $_ })
    $names = ($sids | ForEach-Object { if ($SidNames.ContainsKey($_)) { $SidNames[$_] } else { $_ } }) -join ', '
    [pscustomobject]@{
        Ok = $acl.AreAccessRulesProtected -and -not $inherited -and -not $extra
        Text = "grants: $names; protected: $($acl.AreAccessRulesProtected); inherited entries: $($inherited.Count)"
    }
}

# Checks a file is user-only: a protected DACL for SYSTEM and you alone.
function Check-UserOnly([string]$Check, [string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { Fail $Check "missing: $Path"; return }
    $a = Test-Acl $Path @('S-1-5-18', $UserSid)
    Check $a.Ok $Check "$Path $($a.Text)"
    & icacls $Path 2>&1 | Out-File -Append -LiteralPath (Join-Path $OutDir 'icacls.log') -Encoding utf8
}

function Search-Logs([string]$Dir, [string]$Pattern) {
    if (-not (Test-Path -LiteralPath $Dir)) { return @() }
    @(Get-ChildItem -LiteralPath $Dir -Recurse -File -ErrorAction SilentlyContinue |
        Select-String -Pattern $Pattern -ErrorAction SilentlyContinue)
}

# --- Sources: one worktree per commit ------------------------------------------------------

$script:SourceInfo = @{}
$script:Built = @{}

# The worktree for a source, fetched and checked out at the source's current commit.
function Get-Source([string]$Name) {
    if ($script:SourceInfo.ContainsKey($Name)) { return $script:SourceInfo[$Name].Dir }
    $dir = Join-Path $WorkDir "src\$Name"
    $ref = "refs/jstream-pass/$Name"
    if (-not $NoBuild) {
        Say "Fetching origin/$($Sources[$Name]) for $Name"
        & git -C $Repo fetch --quiet origin "+$($Sources[$Name]):$ref"
        if ($LASTEXITCODE -ne 0) { throw "git fetch of $($Sources[$Name]) failed" }
        if (Test-Path -LiteralPath (Join-Path $dir '.git')) {
            & git -C $dir checkout --quiet --force --detach $ref
        } else {
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dir) | Out-Null
            & git -C $Repo worktree add --quiet --force --detach $dir $ref
        }
        if ($LASTEXITCODE -ne 0) { throw "could not check out $ref into $dir" }
    } elseif (-not (Test-Path -LiteralPath $dir)) {
        throw "-NoBuild, but $dir doesn't exist yet"
    }
    $commit = (& git -C $dir rev-parse HEAD).Trim()
    $script:SourceInfo[$Name] = [pscustomobject]@{ Name = $Name; Ref = "origin/$($Sources[$Name])"; Commit = $commit; Dir = $dir }
    Say "$Name is at $commit in $dir"
    $dir
}

# Runs cargo in a source's worktree. Returns the exit code.
function Invoke-Cargo([string]$Src, [string[]]$Arguments, [string]$LogName, [int]$TimeoutSec = 7200) {
    $dir = Get-Source $Src
    Invoke-Logged (Find-Tool 'cargo') $Arguments (Join-Path $OutDir "$LogName.log") $dir $TimeoutSec
}

# Builds release packages in a source once per run. Returns $true when they're there.
function Build([string]$Src, [string[]]$Packages, [string[]]$Extra = @()) {
    $key = "$Src|$($Packages -join ',')|$($Extra -join ' ')"
    if ($script:Built.ContainsKey($key)) { return $script:Built[$key] }
    if ($NoBuild) { $script:Built[$key] = $true; return $true }
    $cargoArgs = @('build', '--release') + ($Packages | ForEach-Object { '-p', $_ }) + $Extra
    $code = Invoke-Cargo $Src $cargoArgs "build-$Src-$($Packages[0])"
    $ok = $code -eq 0
    if ($ok) { Pass 'build' "$Src`: cargo $($cargoArgs -join ' ')" } else { Fail 'build' "$Src`: cargo $($cargoArgs -join ' ') exited $code" }
    $script:Built[$key] = $ok
    $ok
}

# The test names a cargo test run would run (`-- --list`), so a check can name the
# tests a passing run covered: with --nocapture their `... ok` lines get interleaved.
function Get-TestNames([string]$Src, [string[]]$CargoArgs, [string]$LogName) {
    [void](Invoke-Cargo $Src (@('test', '--release') + $CargoArgs + @('--', '--list')) $LogName)
    @(Get-Content -LiteralPath (Join-Path $OutDir "$LogName.log") -ErrorAction SilentlyContinue |
        Where-Object { $_ -match '^(\S+): test$' } | ForEach-Object { $Matches[1] })
}

function Bin([string]$Src, [string]$Name) { Join-Path (Get-Source $Src) "target\release\$Name.exe" }

# --- The daemon ---------------------------------------------------------------------------

Add-Type -AssemblyName System.Net.Http
$handler = New-Object System.Net.Http.HttpClientHandler
$handler.UseProxy = $false
$Http = New-Object System.Net.Http.HttpClient($handler)
$Http.Timeout = [TimeSpan]::FromSeconds(30)
$script:DaemonN = 0

# Starts a daemon with its own token file and log folder (unless $Arguments name them).
# Returns the daemon, or one whose Url is $null with its exit code and stderr when it
# didn't come up.
function Start-Daemon([string]$Src, [string[]]$Arguments = @(), [string]$Exe) {
    $script:DaemonN++
    $dir = Join-Path $OutDir ("daemon-{0:d2}" -f $script:DaemonN)
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $tokenFile = Join-Path $dir 'token'
    $logDir = Join-Path $dir 'logs'
    $a = @($Arguments)
    if ($a -notcontains '--token-file') { $a += @('--token-file', $tokenFile) } else { $tokenFile = $a[[array]::IndexOf($a, '--token-file') + 1] }
    if ($a -notcontains '--log-dir') { $a += @('--log-dir', $logDir) } else { $logDir = $a[[array]::IndexOf($a, '--log-dir') + 1] }
    if (-not $Exe) { $Exe = Bin $Src 'jstream-daemon' }
    $p = Start-Native $Exe $a
    $err = $p.StandardError.ReadToEndAsync()
    $d = [pscustomobject]@{
        Src = $Src; Proc = $p; Dir = $dir; TokenFile = $tokenFile; LogDir = $logDir; Token = $null
        WsUrl = $null; McpUrl = $null; Lines = @(); OutTask = $null; ErrTask = $err; Session = $null; Id = 0; Code = $null
    }
    $deadline = (Get-Date).AddSeconds(90)
    while (-not $d.WsUrl -and (Get-Date) -lt $deadline) {
        $read = $p.StandardOutput.ReadLineAsync()
        $left = [Math]::Max(0, [int](($deadline - (Get-Date)).TotalMilliseconds))
        if (-not $read.Wait($left)) { break }
        $line = $read.Result
        if ($null -eq $line) { break }
        $d.Lines += $line
        if ($line -match '^mcp endpoint: (\S+)') { $d.McpUrl = $Matches[1] }
        if ($line -match '^listening on (ws://\S+)') { $d.WsUrl = $Matches[1] }
    }
    if (-not $d.WsUrl) {
        if (-not $p.WaitForExit(5000)) { Stop-Native $p }
        $d.Code = $p.ExitCode
        Write-Text (Join-Path $dir 'stderr.txt') $err.Result
        return $d
    }
    $d.OutTask = $p.StandardOutput.ReadToEndAsync()
    $d.Token = (Get-Content -LiteralPath $tokenFile -Raw).Trim()
    if ($script:Secrets -notcontains $d.Token) { $script:Secrets.Add($d.Token) }
    $d
}

# Stops a daemon. With -Graceful, by Ctrl-C, and reports whether it printed `stopped`
# and exited 0; otherwise it's ended.
function Stop-Daemon($d, [switch]$Graceful) {
    if (-not $d -or -not $d.Proc) { return $null }
    if ($d.Session) { try { [void](Send-Mcp $d ([System.Net.Http.HttpMethod]::Delete) $null) } catch {}; $d.Session = $null }
    # Not $graceful: names ignore case, so that would be the -Graceful switch.
    $result = $null
    if (-not $d.Proc.HasExited) {
        if ($Graceful) {
            $sent = Send-CtrlC $d.Proc.Id
            $exited = $d.Proc.WaitForExit(20000)
            if ($exited) { $d.Proc.WaitForExit() }
            $out = ''
            if ($exited -and $d.OutTask) { $out = $d.OutTask.Result }
            $result = [pscustomobject]@{ Sent = $sent; Exited = $exited; Code = $(if ($exited) { $d.Proc.ExitCode } else { $null }); Stopped = ($out -match '(?m)^stopped') }
        }
        Stop-Native $d.Proc
    }
    $out = ''
    if ($d.OutTask -and $d.OutTask.IsCompleted) { $out = $d.OutTask.Result }
    Write-Text (Join-Path $d.Dir 'stdout.txt') ((($d.Lines) -join "`r`n") + "`r`n" + $out)
    if ($d.ErrTask.IsCompleted) { Write-Text (Join-Path $d.Dir 'stderr.txt') $d.ErrTask.Result }
    $result
}

function Send-Mcp($d, [System.Net.Http.HttpMethod]$HttpMethod, [string]$Body) {
    $msg = New-Object System.Net.Http.HttpRequestMessage($HttpMethod, $d.McpUrl)
    [void]$msg.Headers.TryAddWithoutValidation('Authorization', "Bearer $($d.Token)")
    [void]$msg.Headers.TryAddWithoutValidation('Accept', 'application/json, text/event-stream')
    if ($d.Session) {
        [void]$msg.Headers.TryAddWithoutValidation('Mcp-Session-Id', $d.Session)
        [void]$msg.Headers.TryAddWithoutValidation('MCP-Protocol-Version', '2025-11-25')
    }
    if ($Body) { $msg.Content = New-Object System.Net.Http.StringContent($Body, [System.Text.Encoding]::UTF8, 'application/json') }
    $Http.SendAsync($msg).GetAwaiter().GetResult()
}

function Invoke-Mcp($d, [string]$Method, $Params) {
    $d.Id++
    $body = @{ jsonrpc = '2.0'; id = $d.Id; method = $Method; params = $Params } | ConvertTo-Json -Depth 30 -Compress
    $resp = Send-Mcp $d ([System.Net.Http.HttpMethod]::Post) $body
    $content = $resp.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    if (-not $resp.IsSuccessStatusCode) { throw "MCP $Method answered HTTP $([int]$resp.StatusCode): $content" }
    $values = $null
    if ($resp.Headers.TryGetValues('Mcp-Session-Id', [ref]$values)) { $d.Session = @($values)[0] }
    if ($content -match '(?m)^data:\s*(\{.*\})\s*$') { $content = $Matches[1] }
    $content | ConvertFrom-Json
}

# Calls one MCP tool. Returns its structured result; throws with the error otherwise.
function Invoke-Tool($d, [string]$Tool, $Arguments) {
    if (-not $d.Session) {
        $init = Invoke-Mcp $d 'initialize' @{ protocolVersion = '2025-11-25'; capabilities = @{}; clientInfo = @{ name = 'jstream-windows-pass'; version = '1' } }
        if (-not $init.PSObject.Properties['result']) { throw "MCP initialize failed: $($init | ConvertTo-Json -Compress -Depth 8)" }
    }
    $r = Invoke-Mcp $d 'tools/call' @{ name = $Tool; arguments = $Arguments }
    if (-not $r.PSObject.Properties['result'] -or $r.result.isError) {
        throw "$Tool failed: $($r | ConvertTo-Json -Compress -Depth 12)"
    }
    $r.result.structuredContent
}
function Invoke-Op($d, [string]$Tool, [hashtable]$Op) { Invoke-Tool $d $Tool @{ op = $Op } }
function Invoke-Batch($d, [object[]]$Edits) { Invoke-Tool $d 'batch' @{ edits = $Edits } }

function Get-OutputState($d, [string]$Output) {
    # A query's structured result is { kind: "query", result: { type, ... } }.
    try { (Invoke-Op $d 'output' @{ type = 'output.status'; output_id = $Output }).result.result.state } catch { $null }
}
function Wait-OutputState($d, [string]$Output, [string]$Want, [int]$TimeoutSec = 30) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = $null
    do {
        $last = Get-OutputState $d $Output
        if ($last -eq $Want) { return $last }
        if ($last -eq 'failed') { return $last }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    $last
}

# A scene on program holding one source, filling a 1920x1080 canvas.
function New-ProgramScene($d, [hashtable]$Kind, [hashtable]$Settings) {
    $create = @{ type = 'source.create'; source_id = 'src'; name = 'Source'; kind = $Kind }
    if ($Settings) { $create.settings = $Settings }
    Invoke-Batch $d @(
        @{ type = 'canvas.set_size'; width = 1920; height = 1080 }
        @{ type = 'scene.create'; scene_id = 'main'; name = 'Main' }
        $create
        @{ type = 'item.add'; scene_id = 'main'; item_id = 'item'; source_id = 'src'
           geometry = @{ center = @{ x = 960; y = 540 }; size = @{ width = 1920; height = 1080 }; fit_mode = 'fit' } }
    ) | Out-Null
}

# Records the program for $Seconds into $Path. Returns the state the output ended in.
function Invoke-Recording($d, [string]$Path, [int]$Seconds, [scriptblock]$While) {
    Invoke-Op $d 'output' @{ type = 'output.create'; output_id = 'rec'; kind = @{ type = 'record'; path = $Path; container = 'mp4' } } | Out-Null
    Invoke-Op $d 'output' @{ type = 'output.start'; output_id = 'rec' } | Out-Null
    $s = Wait-OutputState $d 'rec' 'active' 30
    if ($s -ne 'active') { return "never active (last: $s)" }
    if ($While) { & $While } else { Start-Sleep -Seconds $Seconds }
    $s = Get-OutputState $d 'rec'
    if ($s -ne 'active') { return "left active (state: $s)" }
    Invoke-Op $d 'output' @{ type = 'output.stop'; output_id = 'rec' } | Out-Null
    $s = Wait-OutputState $d 'rec' 'stopped' 30
    if ($s -ne 'stopped') { return "never stopped (last: $s)" }
    'ok'
}

# ffprobe's view of a file: streams and format, as an object.
function Get-Probe([string]$Path) {
    $ffprobe = Find-Tool 'ffprobe'
    if (-not $ffprobe -or -not (Test-Path -LiteralPath $Path)) { return $null }
    $r = Invoke-Native $ffprobe @('-v', 'error', '-show_entries',
        'stream=codec_type,codec_name,channels,color_space,color_range,color_primaries,color_transfer:format=duration',
        '-of', 'json', $Path) 60
    try { $r.Out | ConvertFrom-Json } catch { $null }
}

# Runs one daemon-based check body, recording an exception as a failure, and always
# stopping the daemon.
function With-Daemon($d, [scriptblock]$Body) {
    try { & $Body }
    catch { Fail 'script' $_.Exception.Message }
    finally { [void](Stop-Daemon $d) }
}

function Need-Daemon($d) {
    if ($d.WsUrl) { return $true }
    $why = ($d.ErrTask.Result -split "`n" | Where-Object { $_ -match 'error' } | Select-Object -First 2) -join ' '
    Fail 'daemon' "did not start (exit $($d.Code)): $why"
    $false
}

# --- The checks -----------------------------------------------------------------------------

function Test-Smoke {
    if (-not (Build 'main' @('jstream-daemon', 'jstream-cli'))) { return }
    $src = Get-Source 'main'
    $dir = Join-Path $OutDir 'smoke'
    $log = Join-Path $OutDir 'smoke.log'
    $code = Invoke-Logged (Get-PowerShellExe) @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
        (Join-Path $src 'scripts\smoke.ps1'), '-Release', '-NoBuild', '-OutDir', $dir) $log $src 600
    $summary = (Get-Content -LiteralPath $log | Where-Object { $_ -match '^(PASS|FAIL|WARN|SKIP) ' }) -join '; '
    switch ($code) {
        0 { Pass 'smoke.ps1' "passed: $summary" }
        2 { Fail 'smoke.ps1' "refused to run (exit 2); see $log" }
        default { Fail 'smoke.ps1' "exit $code`: $summary" }
    }
    if (Test-Path -LiteralPath (Join-Path $dir 'token')) { $script:Secrets.Add((Get-Content -LiteralPath (Join-Path $dir 'token') -Raw).Trim()) }

    # Runbook step 7: Ctrl-C stops the daemon gracefully.
    $d = Start-Daemon 'main'
    if (-not (Need-Daemon $d)) { return }
    $g = Stop-Daemon $d -Graceful
    if (-not $g) { Fail 'graceful stop' 'the daemon had already exited'; return }
    Check ($g.Exited -and $g.Code -eq 0 -and $g.Stopped) 'graceful stop' "Ctrl-C sent: $($g.Sent); exited: $($g.Exited); exit code: $($g.Code); printed stopped: $($g.Stopped)"
}

function Test-Render {
    $log = Join-Path $OutDir 'render-tests.log'
    $code = Invoke-Cargo 'main' @('test', '--release', '-p', 'jstream-render', '--', '--nocapture') 'render-tests'
    $text = Get-Content -LiteralPath $log -Raw
    $adapters = @([regex]::Matches($text, 'jstream-render test adapter: ([^\r\n]+)') | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
    $soft = @($adapters | Where-Object { $_ -match 'Cpu|Basic Render|WARP|llvmpipe|lavapipe' })
    $skipped = @([regex]::Matches($text, 'skipping GPU test')).Count
    Check ($code -eq 0) 'cargo test -p jstream-render' "exit $code; $(@([regex]::Matches($text, 'test result: ok')).Count) suites ok"
    Check ($adapters.Count -gt 0 -and -not $soft -and $skipped -eq 0) 'hardware adapter' "adapters: $($adapters -join ' | '); GPU tests skipped: $skipped"
    $pre = @(Get-TestNames 'main' @('-p', 'jstream-render') 'render-list' | Where-Object { $_ -match 'premultipl' })
    Check ($code -eq 0 -and $pre.Count -ge 3) 'premultiplied-import tests' "$($pre.Count) in the passing run: $($pre -join ', ')"
}

function Test-Nv12 {
    if (-not (Build 'main' @('jstream-daemon', 'jstream-cli'))) { return }
    $log = Join-Path $OutDir 'nv12-convert.log'
    $code = Invoke-Cargo 'main' @('test', '--release', '-p', 'jstream-render', '--', 'convert', '--nocapture') 'nv12-convert'
    $text = Get-Content -LiteralPath $log -Raw
    $skipped = $text -match 'skipping the native test'
    Check ($code -eq 0 -and -not $skipped -and (Get-TestNames 'main' @('-p', 'jstream-render') 'render-list' | Where-Object { $_ -match 'native_nv12_texture_matches_the_planes_path' })) 'native NV12 plane-view test' "exit $code; skipped: $skipped"

    # The webcam tests belong to the webcam check, and the virtual-camera test needs the DLL
    # registered (vcam-owner runs it). The media-file tests play a generated clip.
    $log = Join-Path $OutDir 'nv12-hw-tests.log'
    $hwArgs = @('test', '--release', '-p', 'jstream-platform-windows', '--features', 'hw-tests', '--',
        '--ignored', '--skip', 'frame_server_serves_the_published_frames', '--skip', 'video_device')
    $clip = $null
    if (Find-Tool 'ffmpeg') { New-TestMedia (Join-Path $OutDir 'media'); $clip = Join-Path $OutDir 'media\longsound.mp4' }
    if ($clip) { $env:JSTREAM_TEST_MEDIA = $clip } else { $hwArgs += @('--skip', 'media_file') }
    try { $code = Invoke-Cargo 'main' $hwArgs 'nv12-hw-tests' }
    finally { Remove-Item Env:\JSTREAM_TEST_MEDIA -ErrorAction SilentlyContinue }
    $text = Get-Content -LiteralPath $log -Raw
    $failed = @([regex]::Matches($text, '(?m)^test (\S+) \.\.\. FAILED') | ForEach-Object { $_.Groups[1].Value })
    $summary = ''; if ($text -match 'test result: \w+\. (\d+) passed; (\d+) failed') { $summary = "$($Matches[1]) passed, $($Matches[2]) failed; " }
    Check ($code -eq 0) 'platform-windows hw-tests' "exit $code; $($summary)failed: $(if ($failed) { $failed -join ', ' } else { 'none' }) (webcam and virtual-camera tests run in their own checks)"

    $d = Start-Daemon 'main'
    if (-not (Need-Daemon $d)) { return }
    With-Daemon $d {
        New-ProgramScene $d @{ type = 'builtin'; kind = 'display' } @{ target = '' }
        $file = Join-Path $OutDir 'nv12.mp4'
        Say 'Recording the primary display for 15 s; move some windows around.'
        $r = Invoke-Recording $d $file 15
        Check ($r -eq 'ok') 'recording output' $r
        $hits = Search-Logs $d.LogDir 'the video encoder takes (GPU|CPU) frames'
        $gpu = @($hits | Where-Object { $_.Line -match 'takes GPU frames' })
        Check ($gpu.Count -gt 0) 'encoder takes GPU frames' $(if ($hits) { $hits[0].Line.Trim() } else { 'no encoder line in the log' })
        Skip 'readbacks stay 0' 'OutputStats::readbacks isn''t visible outside the engine; the log line above is the evidence'
        $probe = Get-Probe $file
        if ($probe) {
            $v = @($probe.streams | Where-Object { $_.codec_type -eq 'video' })[0]
            $props = "codec $($v.codec_name), space $($v.color_space), range $($v.color_range), primaries $($v.color_primaries), transfer $($v.color_transfer)"
            Check ($v.codec_name -eq 'h264' -and $v.color_space -eq 'bt709' -and $v.color_range -eq 'tv') 'BT.709 limited range' $props
            $pts = @(& (Find-Tool 'ffprobe') -v error -select_streams v -show_entries frame=best_effort_timestamp_time -of csv=p=0 $file |
                Where-Object { $_ -match '^\d' } | ForEach-Object { [double]$_ })
            $back = 0; for ($i = 1; $i -lt $pts.Count; $i++) { if ($pts[$i] -le $pts[$i - 1]) { $back++ } }
            Check ($pts.Count -gt 100 -and $back -eq 0) 'frames in order' "$($pts.Count) frames, $back out of order or repeated timestamps"
        } else { Skip 'recorded file' 'no ffprobe, or no file' }
        if (Test-Path -LiteralPath $file) {
            if (-not $Unattended) { Invoke-Item -LiteralPath $file }
            Ask 'picture' "nv12.mp4 is open: does it play, with right colours and no stale or out-of-order frames?"
        }
    }
}

# Test media for #382: generated with ffmpeg, matching the files of #382's re-run.
function New-TestMedia([string]$Dir) {
    $ffmpeg = Find-Tool 'ffmpeg'
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $tone = 'sine=frequency=440:sample_rate=44100'
    $jobs = @(
        @{ File = 'audio.m4a'; Args = @('-f', 'lavfi', '-i', "$tone`:duration=5", '-ac', '1', '-c:a', 'aac') }
        @{ File = 'audio.mp3'; Args = @('-f', 'lavfi', '-i', "$tone`:duration=5", '-ac', '1', '-c:a', 'libmp3lame') }
        @{ File = 'longsound.mp4'; Args = @('-f', 'lavfi', '-i', 'testsrc=duration=3:size=640x360:rate=30', '-f', 'lavfi', '-i', "$tone`:duration=8",
            '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac') }
        @{ File = 'surround51.mp4'; Args = @('-f', 'lavfi', '-i', 'testsrc=duration=6:size=640x360:rate=30', '-f', 'lavfi', '-i',
            "aevalsrc=$((1..6 | ForEach-Object { 'sin(440*2*PI*t)' }) -join '|'):c=5.1:d=6:s=48000",
            '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-ac', '6') }
        @{ File = 'sync.mp4'; Args = @('-f', 'lavfi', '-i', "color=black:size=640x360:rate=30:duration=1,drawbox=color=white:t=fill:enable='lt(t,0.1)'",
            '-f', 'lavfi', '-i', "aevalsrc='if(lt(t,0.1),sin(1000*2*PI*t),0)':d=1:s=48000",
            '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac') }
    )
    foreach ($j in $jobs) {
        $path = Join-Path $Dir $j.File
        if (Test-Path -LiteralPath $path) { continue }
        $r = Invoke-Native $ffmpeg (@('-hide_banner', '-nostdin', '-loglevel', 'error', '-y') + $j.Args + @($path)) 60
        if ($r.Code -ne 0) { throw "ffmpeg could not make $($j.File): $($r.Err.Trim())" }
    }
}

# Silences under -40 dB lasting 20 ms or more, away from the recording's first second and
# last half second.
function Get-Holes([string]$Path, [double]$Duration) {
    $r = Invoke-Native (Find-Tool 'ffmpeg') @('-hide_banner', '-nostdin', '-i', $Path, '-af', 'silencedetect=n=-40dB:d=0.02', '-f', 'null', '-') 120
    $holes = @()
    $start = $null
    foreach ($line in ($r.Err -split "`n")) {
        if ($line -match 'silence_start: (-?[\d.]+)') { $start = [double]$Matches[1] }
        if ($line -match 'silence_end: ([\d.]+) \| silence_duration: ([\d.]+)') {
            if ($start -ge 1.0 -and [double]$Matches[1] -le $Duration - 0.5) { $holes += ('{0:n3}s at {1:n2}s' -f [double]$Matches[2], $start) }
            $start = $null
        }
    }
    $holes
}

function Get-MeanVolume([string]$Path) {
    $r = Invoke-Native (Find-Tool 'ffmpeg') @('-hide_banner', '-nostdin', '-ss', '1', '-i', $Path, '-af', 'volumedetect', '-f', 'null', '-') 120
    if ($r.Err -match 'mean_volume: (-?[\d.]+) dB') { return [double]$Matches[1] }
    $null
}

function Test-Media {
    if (-not (Find-Tool 'ffmpeg') -or -not (Find-Tool 'ffprobe')) { Skip 'media' 'ffmpeg and ffprobe must be on PATH'; return }
    if (-not (Build 'main' @('jstream-daemon', 'jstream-cli'))) { return }
    $media = Join-Path $OutDir 'media'
    New-TestMedia $media
    $volumes = @{}
    foreach ($name in @('audio.m4a', 'audio.mp3', 'longsound.mp4', 'surround51.mp4', 'sync.mp4')) {
        $d = Start-Daemon 'main'
        if (-not (Need-Daemon $d)) { return }
        $file = Join-Path $OutDir "media-$($name -replace '\.', '-').mp4"
        With-Daemon $d {
            $path = (Join-Path $media $name) -replace '\\', '/'
            New-ProgramScene $d @{ type = 'builtin'; kind = 'media' } @{ file = @{ type = 'asset'; root = 'absolute'; path = $path }; loop = $true }
            $r = Invoke-Recording $d $file 12
            Check ($r -eq 'ok') "$name recording" $r
        }
        $probe = Get-Probe $file
        if (-not $probe) { Fail "$name analysis" 'no recording'; continue }
        $dur = [double]$probe.format.duration
        if ($name -eq 'sync.mp4') {
            $r = Invoke-Native (Find-Tool 'ffmpeg') @('-hide_banner', '-nostdin', '-i', $file, '-vf', 'blackdetect=d=0.01:pix_th=0.10', '-an', '-f', 'null', '-') 120
            # blackdetect closes the last black run at the end of the file: that's not a flash.
            $ends = @([regex]::Matches($r.Err, 'black_end:([\d.]+)') | ForEach-Object { [double]$_.Groups[1].Value } | Where-Object { $_ -lt $dur - 0.1 })
            $gaps = @(); for ($i = 1; $i -lt $ends.Count; $i++) { $gaps += [Math]::Round($ends[$i] - $ends[$i - 1], 3) }
            $bad = @($gaps | Where-Object { [Math]::Abs($_ - 1.0) -gt 0.035 })
            Check ($gaps.Count -ge 8 -and -not $bad) 'sync clip flashes 1.000 s apart' "$($gaps.Count) gaps: $($gaps -join ' ')"
            continue
        }
        $holes = @(Get-Holes $file $dur)
        Check (-not $holes) "$name has no holes" $(if ($holes) { "silences: $($holes -join ', ')" } else { "none below -40 dB for 20 ms in $([Math]::Round($dur, 1)) s" })
        $volumes[$name] = Get-MeanVolume $file
        if ($name -eq 'surround51.mp4') {
            $a = @($probe.streams | Where-Object { $_.codec_type -eq 'audio' })[0]
            Check ($a -and $a.channels -eq 2) 'surround51.mp4 mixed to stereo' "recorded channels: $(if ($a) { $a.channels } else { 'no audio' })"
        }
    }
    if ($volumes['audio.m4a'] -ne $null -and $volumes['audio.mp3'] -ne $null) {
        $diff = [Math]::Abs($volumes['audio.m4a'] - $volumes['audio.mp3'])
        Check ($diff -le 3) 'audio.mp3 level matches audio.m4a' ("mean {0} dB against {1} dB" -f $volumes['audio.mp3'], $volumes['audio.m4a'])
    }
}

# Loads the MSVC environment into this process, as spikes/s4-browser/run-s4.ps1 does.
function Enter-VsEnv {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { return $false }
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ([string]::IsNullOrWhiteSpace($vs)) { return $false }
    $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
    # vcvars64.bat calls vswhere by name.
    $env:Path = "$(Split-Path -Parent $vswhere);$env:Path"
    cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
        if ($_ -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2]) }
    }
    $true
}

function Test-Browser {
    $saved = [Environment]::GetEnvironmentVariables()
    try {
        if (-not (Enter-VsEnv)) { Skip 'page check' 'the MSVC C++ build tools were not found (vswhere)'; return }
        $missing = @('cmake', 'ninja' | Where-Object { -not (Find-Tool $_) })
        if ($missing) { Skip 'page check' "not on PATH: $($missing -join ', ')"; return }
        $code = Invoke-Cargo 'main' @('build', '--release', '-p', 'jstream-browser-helper', '--features', 'cef') 'browser-helper-build'
        if ($code -ne 0) { Fail 'build' "jstream-browser-helper exited $code"; return }
        $log = Join-Path $OutDir 'page-check.log'
        $code = Invoke-Cargo 'main' @('run', '--release', '-p', 'jstream-browser', '--features', 'cef,helper', '--example', 'page_check') 'page-check'
        $text = Get-Content -LiteralPath $log -Raw
        if ($code -ne 0) { Fail 'page_check' "exit $code; see $log"; return }
        $fps = $null; if ($text -match '([\d.]+) frames a second') { $fps = [double]$Matches[1] }
        Check ($fps -ge 55 -and $fps -le 65) 'about 60 frames a second' "$fps"
        $halves = @([regex]::Matches($text, 'audio \(([^)]+)\):[^\r\n]*?(\d+) frames lost[^\r\n]*\r?\n\s+left: ([\d.]+) Hz[^\r\n]*\r?\n\s+right: ([\d.]+) Hz'))
        foreach ($h in $halves) {
            $name = $h.Groups[1].Value; $lost = [int]$h.Groups[2].Value
            $left = [double]$h.Groups[3].Value; $right = [double]$h.Groups[4].Value
            # page_check's first listener stays attached until the second one opens, about a
            # second after the CSS change, so its right channel reads a little above 660 Hz.
            $rightOk = [Math]::Abs($right - 880) -le 5
            $want = '880'
            if ($name -notmatch 'CSS') { $rightOk = $right -ge 655 -and $right -le 720; $want = '660, up to 720 for the overlap' }
            Check ($lost -eq 0 -and [Math]::Abs($left - 440) -le 5 -and $rightOk) "audio, $name" "left $left Hz, right $right Hz (want 440 and $want), $lost frames lost"
        }
        if ($halves.Count -ne 2) { Fail 'audio' "expected two audio reports, found $($halves.Count)" }
        if ($text -match 'helper exited in ([\d.]+) ms') { Check ([double]$Matches[1] -lt 10000) 'helper exits within 10 s' "$($Matches[1]) ms" }
        else { Fail 'helper exits within 10 s' 'no exit line' }
        if ($text -match 'requests refused: (\d+)') {
            $n = [int]$Matches[1]
            if ($n -eq 1) { Pass 'requests refused: 1' 'closes #341' } else { Warn "requests refused: $n" 'the built-in page expects 1; leave #341 open' }
        } else { Fail 'requests refused' 'no line' }
        Ask 'renderer integrity' 'Optional, in Process Explorer while page_check ran: did the CEF renderer processes run at Low, Untrusted or AppContainer integrity?'
    } finally {
        foreach ($k in @([Environment]::GetEnvironmentVariables().Keys)) { if (-not $saved.Contains($k)) { [Environment]::SetEnvironmentVariable($k, $null) } }
        foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
    }
}

function Test-YouTube {
    if (-not $YouTubeUrl -and -not $Unattended) {
        $YouTubeUrl = (Read-Host '  YouTube ingest URL (rtmps://..., no key; Enter to skip)').Trim()
        if ($YouTubeUrl) { $script:YouTubeKeyFile = (Read-Host '  Path of the file holding the stream key').Trim().Trim('"') }
    }
    $keyFile = $script:YouTubeKeyFile
    if (-not $YouTubeUrl -or -not $keyFile -or -not (Test-Path -LiteralPath $keyFile -PathType Leaf)) {
        Skip 'youtube' 'needs -YouTubeUrl and -YouTubeKeyFile'; return
    }
    if (-not (Prompt-Do 'In YouTube Studio, turn on auto-start and auto-stop for the stream, and keep Studio open.')) { Skip 'youtube' 'skipped by you'; return }
    if (-not (Build 'main' @('jstream-daemon', 'jstream-cli'))) { return }
    $src = Get-Source 'main'
    $dir = Join-Path $OutDir 'youtube'
    $log = Join-Path $OutDir 'youtube.log'
    Say 'Going live for 90 s. Watch Studio.'
    $code = Invoke-Logged (Get-PowerShellExe) @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $src 'scripts\smoke.ps1'),
        '-Release', '-NoBuild', '-Seconds', '90', '-RtmpUrl', $YouTubeUrl, '-KeyFile', $keyFile, '-OutDir', $dir) $log $src 600
    Check ($code -eq 0) 'smoke.ps1 to YouTube' "exit $code; see $log"
    if (Test-Path -LiteralPath (Join-Path $dir 'token')) { $script:Secrets.Add((Get-Content -LiteralPath (Join-Path $dir 'token') -Raw).Trim()) }
    Ask 'goes live by itself' 'Within a minute of going live, did Studio show the stream receiving data, and did the broadcast go live by itself?'
    Ask 'ends by itself' 'After the output stopped, did Studio end the broadcast by itself?'
    if (Confirm 'Search this run''s logs for the stream key? This reads the key file in memory and prints only file names, never the key.') {
        $key = (Get-Content -LiteralPath $keyFile -Raw).Trim()
        $found = @(Get-ChildItem -LiteralPath $dir -Recurse -File | Where-Object { $_.Extension -ne '.flv' } |
            Where-Object { (Select-String -LiteralPath $_.FullName -SimpleMatch -Pattern $key -Quiet) } | ForEach-Object { $_.FullName })
        $key = $null
        Check (-not $found) 'stream key never logged' $(if ($found) { "found in: $($found -join ', ')" } else { 'not in any log or the audit log' })
    } else { Skip 'stream key never logged' 'declined' }
}

function Test-Preview {
    $trunk = Find-Tool 'trunk'
    $wb = Find-Tool 'wasm-bindgen'
    if (-not $trunk -or -not $wb) {
        if (Confirm 'Install trunk 0.21.14 and wasm-bindgen-cli 0.2.129 with cargo install (the versions the desktop app docs pin)?') {
            if (-not $trunk) { [void](Invoke-Logged (Find-Tool 'cargo') @('install', 'trunk', '--version', '0.21.14', '--locked') (Join-Path $OutDir 'install-trunk.log') $OutDir) }
            if (-not $wb) { [void](Invoke-Logged (Find-Tool 'cargo') @('install', 'wasm-bindgen-cli', '--version', '0.2.129', '--locked') (Join-Path $OutDir 'install-wasm-bindgen.log') $OutDir) }
            $trunk = Find-Tool 'trunk'
        }
        if (-not $trunk -or -not (Find-Tool 'wasm-bindgen')) { Skip 'preview' 'needs trunk and wasm-bindgen'; return }
    }
    $src = Get-Source 'main'
    $ui = Join-Path $src 'crates\jstream-ui'
    $code = Invoke-Logged $trunk @('build', '--release') (Join-Path $OutDir 'trunk-build.log') $ui
    if ($code -ne 0) { Fail 'UI build' "trunk build exited $code"; return }
    if (-not (Build 'main' @('jstream-desktop'))) { return }
    $app = Start-Native (Bin 'main' 'jstream') @('--ui-dir', (Join-Path $ui 'dist'))
    try {
        Say 'The desktop app is open. Add a display source to a scene so the canvas has something to preview.'
        Ask 'preview shows' 'Does the live preview show under the canvas boxes (WebView2), with H.264 in the frames?'
        Ask 'follows edits' 'Does it follow committed edits and scene switches?'
        Ask 'backlog recovers' 'After a slow moment (drag a big window around), does it recover at the next keyframe, within 2 s?'
        Ask 'JPEG fallback' 'If the decoder fails, does the canvas fall back to JPEG rather than showing "No preview"? (skip if you can''t force a failure)'
    } finally { Stop-Native $app }
}

function Test-VcamOwner {
    if (-not (Build 'm1-3' @('jstream-vcam-windows') @('--example', 'vcam_smoke'))) { return }
    if (-not (Build 'm1-3' @('jstream-vcam-windows'))) { return }
    $dll = Join-Path (Get-Source 'm1-3') 'target\release\jstream_vcam_windows.dll'
    $smoke = Join-Path (Get-Source 'm1-3') 'target\release\examples\vcam_smoke.exe'
    $test = 'C:\JStreamVcamTest'
    if (-not (Confirm "Register the test camera: as administrator, stop Frame Server, copy the DLL to $test and regsvr32 it (one UAC prompt; it's removed at the end)?")) {
        Skip 'vcam_smoke' 'registration declined'; return
    }
    $log = Join-Path $OutDir 'vcam-register.log'
    $code = Invoke-Elevated @"
New-Item -ItemType Directory -Force -Path '$test' | Out-Null
net stop FrameServer
Copy-Item -Force -LiteralPath '$dll' -Destination '$test\jstream_vcam_windows.dll'
regsvr32 /s '$test\jstream_vcam_windows.dll'
"@ $log
    if ($null -eq $code) { Skip 'vcam_smoke' 'the administrator prompt was declined'; return }
    try {
        Say 'Starting the producer for 60 s. Open the Camera app and pick "JStream Virtual Camera".'
        $r = Invoke-Native $smoke @('--seconds', '60') 120
        Write-Text (Join-Path $OutDir 'vcam-smoke-1.txt') ($r.Out + $r.Err)
        $verdict = ($r.Out -split "`n" | Where-Object { $_ -match 'verdict' } | Select-Object -First 1)
        Check ($r.Code -eq 0 -and $verdict -match 'reached|pass|ok') 'vcam_smoke' "exit $($r.Code); $verdict"
        Ask 'producer frames' 'Did the Camera app show dark teal with a moving white bar (not the colour bars)?'
        Say 'Restarting the producer while Frame Server still holds the old control block. Keep the Camera app open.'
        $r = Invoke-Native $smoke @('--seconds', '15') 60
        Write-Text (Join-Path $OutDir 'vcam-smoke-2.txt') ($r.Out + $r.Err)
        Check ($r.Code -eq 0) 'producer restarts' "exit $($r.Code); $(($r.Out -split "`n" | Where-Object { $_ -match 'verdict' } | Select-Object -First 1))"
        $code = Invoke-Cargo 'm1-3' @('test', '--release', '-p', 'jstream-platform-windows', '--features', 'hw-tests', '--',
            '--ignored', 'frame_server_serves_the_published_frames') 'vcam-hw-test'
        Check ($code -eq 0) 'virtual-camera hw-test' "exit $code (frame_server_serves_the_published_frames)"
    } finally {
        $vlog = Join-Path $OutDir 'vcam.log'
        $code = Invoke-Elevated @"
Copy-Item -Force -LiteralPath 'C:\Windows\ServiceProfiles\LocalService\AppData\Local\JStream\vcam\vcam.log' -Destination '$vlog'
regsvr32 /s /u '$test\jstream_vcam_windows.dll'
"@ (Join-Path $OutDir 'vcam-unregister.log')
        if ($null -eq $code) { Warn 'cleanup' "declined: run regsvr32 /u $test\jstream_vcam_windows.dll as administrator" }
    }
    if (Test-Path -LiteralPath $vlog) {
        $bad = @(Select-String -LiteralPath $vlog -Pattern 'OpenSharedHandleByName|owner|refused' | Where-Object { $_.Line -match 'fail|error|refused' })
        Check (-not $bad) 'vcam.log clean' $(if ($bad) { ($bad | Select-Object -First 3 | ForEach-Object { $_.Line.Trim() }) -join ' | ' } else { 'no OpenSharedHandleByName, owner or refused failure' })
    } else { Skip 'vcam.log clean' 'could not copy vcam.log' }
}

function Test-TokenAcl {
    if (-not (Build 'm1-3' @('jstream-daemon', 'jstream-cli'))) { return }
    $pub = Join-Path $env:PUBLIC ('jstream-pass-' + (Get-Date -Format 'yyyyMMddHHmmss'))
    New-Item -ItemType Directory -Force -Path $pub | Out-Null
    try {
        $token = Join-Path $pub 'jstream-token'
        $logs = Join-Path $pub 'logs'
        $d = Start-Daemon 'm1-3' @('--token-file', $token, '--log-dir', $logs)
        if (-not (Need-Daemon $d)) { return }
        Stop-Daemon $d | Out-Null
        Check-UserOnly 'token file in Public' $token
        # audit.jsonl is jstream-core's, made user-only by #574: the private-files check.
        foreach ($f in @(Get-ChildItem -LiteralPath $logs -File -Filter 'jstream-daemon*.log' -ErrorAction SilentlyContinue)) { Check-UserOnly "log file $($f.Name)" $f.FullName }
        if (-not @(Get-ChildItem -LiteralPath $logs -File -Filter 'jstream-daemon*.log' -ErrorAction SilentlyContinue)) { Fail 'log files' "no jstream-daemon log in $logs" }

        & icacls $token /grant '*S-1-1-0:R' | Out-Null
        $d = Start-Daemon 'm1-3' @('--token-file', $token, '--log-dir', $logs)
        if ($d.WsUrl) { Stop-Daemon $d | Out-Null; Fail 'loose token refused' 'the daemon started with Everyone:R on its token' }
        else { Check ($d.Code -eq 1 -and $d.ErrTask.Result -match 'may be accessible to other users') 'loose token refused' "exit $($d.Code): $(($d.ErrTask.Result -split "`n" | Where-Object { $_ -match 'accessible' } | Select-Object -First 1))" }

        $existing = Join-Path $env:APPDATA 'JStream\token'
        if (Test-Path -LiteralPath $existing) {
            $d = Start-Daemon 'm1-3' @('--token-file', $existing, '--log-dir', (Join-Path $OutDir 'appdata-token-logs'))
            Check ([bool]$d.WsUrl) 'existing APPDATA token accepted' "$existing (inherited SYSTEM, Administrators and you)"
            Stop-Daemon $d | Out-Null
        } else { Skip 'existing APPDATA token accepted' "no $existing" }
    } finally { Remove-Item -Recurse -Force -LiteralPath $pub -ErrorAction SilentlyContinue }
}

function Test-Crash {
    $code = Invoke-Cargo 'm1-3' @('test', '--release', '-p', 'jstream-crash') 'crash-tests'
    Check ($code -eq 0) 'jstream-crash tests' "exit $code (a child process panics and writes its report)"
    $code = Invoke-Cargo 'm1-3' @('test', '--release', '-p', 'jstream-desktop') 'desktop-tests'
    Check ($code -eq 0) 'jstream-desktop builds and tests on Windows' "exit $code (#557's first box)"
    Skip 'crash report from a real panic' 'neither binary can be made to panic on purpose yet'
}

function Test-VcamRegister {
    if (-not (Build 'm1-3' @('jstream-daemon', 'jstream-cli', 'jstream-vcam-windows'))) { return }
    $rel = Join-Path (Get-Source 'm1-3') 'target\release'
    $portable = Join-Path $OutDir 'portable'
    New-Item -ItemType Directory -Force -Path $portable | Out-Null
    Copy-Item -Force (Join-Path $rel 'jstream-daemon.exe'), (Join-Path $rel 'jstream_vcam_windows.dll') $portable
    $exe = Join-Path $portable 'jstream-daemon.exe'
    $pf = Join-Path $env:ProgramFiles 'JStream\vcam'

    $d = Start-Daemon 'm1-3' @() $exe
    if (-not (Need-Daemon $d)) { return }
    Stop-Daemon $d | Out-Null
    $hint = Search-Logs $d.LogDir 'virtual camera'
    Check ([bool]($hint | Where-Object { $_.Line -match 'optional' })) 'startup says registering is optional' $(if ($hint) { $hint[0].Line.Trim() } else { 'no virtual camera line in the log' })

    if (-not (Confirm "Register the virtual camera with jstream-daemon --register-vcam? It copies the DLL to $pf and registers it machine-wide (one UAC prompt). The last steps unregister it again.")) {
        Skip 'register' 'declined'; return
    }
    $r = Invoke-Native $exe @('--register-vcam') 300
    Write-Text (Join-Path $OutDir 'register-1.txt') ($r.Out + $r.Err)
    Check ($r.Code -eq 0 -and $r.Out -match 'virtual camera: registered') 'register' "exit $($r.Code): $($r.Out.Trim())"
    $allowed = @('S-1-5-18', 'S-1-5-32-544', 'S-1-5-19', 'S-1-5-32-545')
    foreach ($p in @($pf, (Join-Path $pf 'jstream_vcam_windows.dll'))) {
        if (Test-Path -LiteralPath $p) { $a = Test-Acl $p $allowed; Check $a.Ok "ACL of $p" $a.Text } else { Fail "ACL of $p" 'missing' }
    }
    $reg = (& reg query 'HKLM\SOFTWARE\Classes\CLSID' /s /f 'jstream_vcam_windows.dll' /d 2>&1) -join "`n"
    Check ($reg -match [regex]::Escape($pf)) 'InprocServer32 names the copy' (($reg -split "`n" | Where-Object { $_ -match 'REG_' } | Select-Object -First 2) -join ' | ')
    $r = Invoke-Native $exe @('--register-vcam') 120
    Check ($r.Code -eq 0 -and $r.Out -match 'already registered') 'second register is a no-op' $r.Out.Trim()

    $d = Start-Daemon 'm1-3' @() $exe
    if (Need-Daemon $d) {
        With-Daemon $d {
            New-ProgramScene $d @{ type = 'builtin'; kind = 'display' } @{ target = '' }
            Invoke-Op $d 'output' @{ type = 'output.create'; output_id = 'vcam'; kind = @{ type = 'virtual_cam' } } | Out-Null
            Invoke-Op $d 'output' @{ type = 'output.start'; output_id = 'vcam' } | Out-Null
            $s = Wait-OutputState $d 'vcam' 'active' 30
            Check ($s -eq 'active') 'virtual_cam output' "state: $s"
            Ask 'camera shows the program' 'Open the Camera app and pick "JStream Virtual Camera". Does it show your screen?'
            Ask 'Frame Server loads the copy' "Optional, in Process Explorer: does Frame Server's svchost have $pf\jstream_vcam_windows.dll loaded, not the portable folder's DLL?"
        }
    }

    if (Confirm 'Replace the portable folder''s DLL with a different build (a copy with 16 extra bytes), check the warning, and register again (one UAC prompt)?') {
        $pdll = Join-Path $portable 'jstream_vcam_windows.dll'
        $fs = [IO.File]::Open($pdll, 'Append'); $fs.Write((New-Object byte[] 16), 0, 16); $fs.Dispose()
        $d = Start-Daemon 'm1-3' @() $exe
        if (Need-Daemon $d) {
            Stop-Daemon $d | Out-Null
            $w = Search-Logs $d.LogDir 'differs'
            Check ([bool]$w) 'startup warns the copy differs' $(if ($w) { $w[0].Line.Trim() } else { 'no warning' })
        }
        $r = Invoke-Native $exe @('--register-vcam') 300
        Check ($r.Code -eq 0 -and $r.Out -match 'registered') 'register replaces the copy' $r.Out.Trim()
    } else { Skip 'replaced DLL' 'declined' }

    if (Confirm 'regsvr32 the portable folder''s DLL as administrator, to check the "registered outside" warning (one UAC prompt; the next step unregisters)?') {
        $code = Invoke-Elevated "regsvr32 /s '$(Join-Path $portable 'jstream_vcam_windows.dll')'" (Join-Path $OutDir 'regsvr32-portable.log')
        if ($null -ne $code) {
            $d = Start-Daemon 'm1-3' @() $exe
            if (Need-Daemon $d) {
                Stop-Daemon $d | Out-Null
                $w = Search-Logs $d.LogDir 'outside'
                Check ([bool]$w) 'startup warns it is registered outside Program Files' $(if ($w) { $w[0].Line.Trim() } else { 'no warning' })
            }
        } else { Skip 'registered outside' 'the administrator prompt was declined' }
    } else { Skip 'registered outside' 'declined' }

    $r = Invoke-Native $exe @('--unregister-vcam') 300
    Write-Text (Join-Path $OutDir 'unregister.txt') ($r.Out + $r.Err)
    $reg = (& reg query 'HKLM\SOFTWARE\Classes\CLSID' /s /f 'jstream_vcam_windows.dll' /d 2>&1) -join "`n"
    Check ($r.Code -eq 0 -and -not (Test-Path -LiteralPath (Join-Path $env:ProgramFiles 'JStream')) -and $reg -notmatch 'jstream_vcam_windows') 'unregister' "exit $($r.Code): $($r.Out.Trim()) $($r.Err.Trim())"

    if (Prompt-Do 'Next, --register-vcam runs once more: click No on its UAC prompt.') {
        $r = Invoke-Native $exe @('--register-vcam') 300
        Check ($r.Code -eq 1 -and ($r.Out + $r.Err) -match 'the administrator prompt was declined') 'declined prompt' "exit $($r.Code): $(($r.Out + $r.Err).Trim())"
        if ($r.Code -eq 0) {
            # The prompt got a yes: undo it, so the run leaves nothing registered.
            Say 'That registered the camera again; unregistering it (click Yes this time).' 'Yellow'
            $u = Invoke-Native $exe @('--unregister-vcam') 300
            Check ($u.Code -eq 0) 'unregister again' "exit $($u.Code): $($u.Out.Trim()) $($u.Err.Trim())"
        }
    } else { Skip 'declined prompt' 'skipped by you' }
}

function Test-GpuReset {
    $code = Invoke-Cargo 'm1-3' @('test', '--release', '-p', 'jstream-core', '--features', 'hw-tests', '--lib', 'windows_gpu', '--', '--ignored', '--nocapture') 'core-windows-gpu'
    Check ($code -eq 0) 'core windows_gpu hw-tests' "exit $code"
    $dxcap = Find-Tool 'dxcap'
    if (-not $dxcap) { Skip 'forced TDR' 'dxcap is missing: add the Graphics Tools optional feature'; return }
    if (-not (Build 'm1-3' @('jstream-daemon', 'jstream-cli'))) { return }
    if (-not (Confirm 'Force GPU driver resets with dxcap -forcetdr (as administrator: one UAC prompt each; the screen goes black for a moment). First one reset, then three within a minute?')) {
        Skip 'forced TDR' 'declined'; return
    }
    $d = Start-Daemon 'm1-3'
    if (-not (Need-Daemon $d)) { return }
    $file = Join-Path $OutDir 'gpu-reset.mp4'
    With-Daemon $d {
        New-ProgramScene $d @{ type = 'builtin'; kind = 'display' } @{ target = '' }
        $r = Invoke-Recording $d $file 0 {
            Say 'Recording; keep something moving on screen.'
            Start-Sleep -Seconds 8
            $c = Invoke-Elevated "& '$dxcap' -forcetdr" (Join-Path $OutDir 'tdr-1.log')
            if ($null -eq $c) { Skip 'forced TDR' 'the administrator prompt was declined'; return }
            Start-Sleep -Seconds 15
            $s = Get-OutputState $d 'rec'
            Check ($s -eq 'active') 'output stays active through one reset' "state: $s"
            $lines = Search-Logs $d.LogDir 'GPU device was lost|fell back to CPU capture|capturing CPU frames'
            Check ([bool]$lines) 'one warn names the loss or fallback' (($lines | ForEach-Object { $_.Line.Trim() } | Select-Object -First 3) -join ' | ')
            Ask 'GPU memory back' 'Optional: in Task Manager, is the GPU memory back near where it was before the run (no leak)?'
        }
        Check ($r -eq 'ok' -or $r -match 'left active') 'recording through the reset' $r
        $probe = Get-Probe $file
        if ($probe) { Check ([double]$probe.format.duration -gt 15) 'the recording plays through the switch' "$([Math]::Round([double]$probe.format.duration, 1)) s" }
        $removed = Search-Logs $d.LogDir 'DXGI_ERROR_DEVICE_REMOVED|device removed'
        Check (-not $removed) 'no device-removed errors after the switch' "$($removed.Count) lines"

        Invoke-Op $d 'output' @{ type = 'output.delete'; output_id = 'rec' } | Out-Null
        $file3 = Join-Path $OutDir 'gpu-reset-3.mp4'
        Invoke-Op $d 'output' @{ type = 'output.create'; output_id = 'rec3'; kind = @{ type = 'record'; path = $file3; container = 'mp4' } } | Out-Null
        Invoke-Op $d 'output' @{ type = 'output.start'; output_id = 'rec3' } | Out-Null
        if ((Wait-OutputState $d 'rec3' 'active' 30) -eq 'active') {
            for ($i = 1; $i -le 3; $i++) {
                Start-Sleep -Seconds 8
                [void](Invoke-Elevated "& '$dxcap' -forcetdr" (Join-Path $OutDir "tdr-3-$i.log"))
            }
            Start-Sleep -Seconds 10
            $s = Get-OutputState $d 'rec3'
            $why = Search-Logs $d.LogDir 'times within'
            Check ($s -eq 'failed' -and $why) 'third loss in a minute fails the output' "state: $s; $(if ($why) { $why[-1].Line.Trim() } else { 'no loss-count error in the log' })"
            Check (-not $d.Proc.HasExited) 'the daemon keeps running' "running: $(-not $d.Proc.HasExited)"
        } else { Fail 'third loss in a minute fails the output' 'the second recording never became active' }
    }
}

function Test-PrivateFiles {
    $code = Invoke-Cargo 'pr574' @('test', '--release', '-p', 'jstream-core', '--lib', 'private_file') 'private-file-tests'
    Check ($code -eq 0) 'new_files_pass_the_private_file_check' "exit $code"
    if (-not (Build 'pr574' @('jstream-daemon', 'jstream-cli'))) { return }
    $dir = Join-Path $env:PUBLIC ('jstream-pass-' + (Get-Date -Format 'yyyyMMddHHmmss'))
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    try {
        $coll = Join-Path $dir 'collection.json'
        $logs = Join-Path $dir 'logs'
        $d = Start-Daemon 'pr574' @('--collection', $coll, '--log-dir', $logs)
        if (-not (Need-Daemon $d)) { return }
        With-Daemon $d { Invoke-Batch $d @(@{ type = 'scene.create'; scene_id = 'main'; name = 'Main' }) | Out-Null; Start-Sleep -Seconds 3 }
        $audit = @(Get-ChildItem -LiteralPath $logs -Recurse -Filter 'audit.jsonl' -ErrorAction SilentlyContinue)[0]
        if ($audit) { Check-UserOnly 'audit.jsonl' $audit.FullName } else { Fail 'audit.jsonl' "none under $logs" }
        Check-UserOnly 'collection file' $coll
        if ($audit) {
            & icacls $audit.FullName /grant 'Users:R' | Out-Null
            $d = Start-Daemon 'pr574' @('--collection', $coll, '--log-dir', $logs)
            if ($d.WsUrl) { Stop-Daemon $d | Out-Null; Fail 'loose audit log refused' 'the daemon started' }
            else { Check ($d.Code -eq 1 -and $d.ErrTask.Result -match 'audit') 'loose audit log refused' "exit $($d.Code): $(($d.ErrTask.Result -split "`n" | Where-Object { $_ -match 'error' } | Select-Object -First 1))" }
        }
    } finally { Remove-Item -Recurse -Force -LiteralPath $dir -ErrorAction SilentlyContinue }
}

function Test-Webcam {
    $code = Invoke-Cargo 'pr726' @('test', '--release', '-p', 'jstream-platform-windows', '--features', 'hw-tests', '--', '--ignored', 'video_device', '--nocapture') 'webcam-hw-tests'
    $text = Get-Content -LiteralPath (Join-Path $OutDir 'webcam-hw-tests.log') -Raw
    $failed = @([regex]::Matches($text, '(?m)^test (\S+) \.\.\. FAILED') | ForEach-Object { $_.Groups[1].Value })
    Check ($code -eq 0) 'video-device hw-tests (MJPEG included)' "exit $code; failed: $(if ($failed) { $failed -join ', ' } else { 'none' })"
    if (-not (Build 'pr726' @('jstream-daemon', 'jstream-cli'))) { return }
    $d = Start-Daemon 'pr726'
    if (-not (Need-Daemon $d)) { return }
    $file = Join-Path $OutDir 'webcam.mp4'
    With-Daemon $d {
        New-ProgramScene $d @{ type = 'builtin'; kind = 'webcam' } @{ device = '' }
        Say 'Recording the first webcam for 10 s. Say something into its microphone.'
        $r = Invoke-Recording $d $file 10
        Check ($r -eq 'ok') 'webcam recording' $r
        $fmt = Search-Logs $d.LogDir 'webcam|video device|MJPEG|format'
        Pass 'format picked' $(if ($fmt) { ($fmt | Select-Object -First 2 | ForEach-Object { $_.Line.Trim() }) -join ' | ' } else { 'no format line in the log' })
    }
    $probe = Get-Probe $file
    if ($probe) {
        $v = @($probe.streams | Where-Object { $_.codec_type -eq 'video' })
        $a = @($probe.streams | Where-Object { $_.codec_type -eq 'audio' })
        Check ($v.Count -eq 1) 'webcam video recorded' "$($v.Count) video streams"
        if (-not $Unattended) { Invoke-Item -LiteralPath $file }
        Ask 'webcam colours' 'webcam.mp4 is open: is the picture right, with natural colours?'
        Ask 'paired microphone (#726)' 'Is your voice in the recording, from the webcam''s own microphone?'
        Pass 'audio stream' "$($a.Count) audio streams in the recording"
    }
    if (Confirm 'Run the interactive unplug test? It asks you to unplug the webcam mid-stream.') {
        $env:JSTREAM_UNPLUG_TEST = '1'
        try { $code = Invoke-Cargo 'pr726' @('test', '--release', '-p', 'jstream-platform-windows', '--features', 'hw-tests', '--', '--ignored', 'unplug', '--nocapture') 'webcam-unplug' }
        finally { Remove-Item Env:\JSTREAM_UNPLUG_TEST }
        Check ($code -eq 0) 'unplug mid-stream' "exit $code"
    } else { Skip 'unplug mid-stream' 'declined' }
    Skip 'corrupt or truncated MJPEG frame' 'needs a camera that sends one; not forced here'
}

function Test-Cursor {
    $code = Invoke-Cargo 'pr685' @('test', '--release', '-p', 'jstream-platform-windows', '--', '--nocapture') 'cursor-tests'
    $text = Get-Content -LiteralPath (Join-Path $OutDir 'cursor-tests.log') -Raw
    $ran = [bool](Get-TestNames 'pr685' @('-p', 'jstream-platform-windows') 'cursor-list' | Where-Object { $_ -match 'only_a_hidden_cursor_is_left_out' })
    Check ($code -eq 0 -and $ran) 'platform-windows tests' "exit $code; only_a_hidden_cursor_is_left_out in the run: $ran"
    Skip 'captures with the pointer hidden' 'needs the daemon''s show_cursor (#682) or a capture harness'
}

function Test-Clap {
    $clapDir = Join-Path $env:CommonProgramFiles 'CLAP'
    $plugin = @(Get-ChildItem -LiteralPath $clapDir -Filter '*.clap' -Recurse -ErrorAction SilentlyContinue)[0]
    if (-not $plugin) { Skip 'clap' "no .clap file in $clapDir"; return }
    if (-not (Build 'pr573' @('jstream-daemon', 'jstream-plugin-host', 'jstream-cli'))) { return }
    $host_ = Bin 'pr573' 'jstream-plugin-host'
    $scan = Join-Path $OutDir 'clap-scan.bin'
    $p = Start-Process -FilePath $host_ -ArgumentList @('scan', (Format-Arg $plugin.FullName)) -NoNewWindow -Wait -PassThru -RedirectStandardOutput $scan
    $name = [IO.Path]::GetFileNameWithoutExtension($plugin.Name)
    $bytes = [IO.File]::ReadAllBytes($scan)
    $hasName = [Text.Encoding]::UTF8.GetString($bytes) -match [regex]::Escape(($name -split ' ')[0])
    Check ($p.ExitCode -eq 0 -and $bytes.Length -gt 0) 'scan one file' "$($plugin.Name): exit $($p.ExitCode), $($bytes.Length) bytes, names the plugin: $hasName"
    $r = Invoke-Native $host_ @('scan', (Join-Path $env:SystemRoot 'System32\kernel32.dll')) 60
    Check ($r.Code -eq 1) 'scan kernel32.dll fails cleanly' "exit $($r.Code)"
    $d = Start-Daemon 'pr573'
    if (-not (Need-Daemon $d)) { return }
    try {
        # The scan runs in the background; wait for its line.
        $deadline = (Get-Date).AddSeconds(60)
        do {
            Start-Sleep -Seconds 1
            $s = Search-Logs $d.LogDir 'scanned the CLAP plugins|CLAP plugins are unavailable|could not be scanned'
        } while (-not ($s | Where-Object { $_.Line -match 'scanned the CLAP plugins|unavailable' }) -and (Get-Date) -lt $deadline)
        $found = $s | Where-Object { $_.Line -match 'found=(\d+)' } | ForEach-Object { [int]$Matches[1] } | Select-Object -First 1
        $detail = ($s | ForEach-Object { $_.Line.Trim() } | Select-Object -First 3) -join ' | '
        if (-not $detail) {
            $hosts = @(Get-CimInstance Win32_Process -Filter "Name = 'jstream-plugin-host.exe'" -ErrorAction SilentlyContinue | ForEach-Object { $_.CommandLine })
            $detail = "no scan line in the log after 60 s; plugin hosts still running: $(if ($hosts) { $hosts -join ' | ' } else { 'none' })"
        }
        Check ($found -ge 1 -and -not ($s | Where-Object { $_.Line -match 'could not be scanned' })) 'daemon scan' $detail
        $kinds = Invoke-Op $d 'kind' @{ type = 'kind.list' }
        $clap = @(($kinds | ConvertTo-Json -Depth 30 -Compress) | Select-String -Pattern '"(clap/[^"]+)"' -AllMatches | ForEach-Object { $_.Matches } | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
        Check ($clap.Count -ge 1) 'kind.list has the plugin' ($clap -join ', ')
        if ($clap) {
            $desc = Invoke-Op $d 'kind' @{ type = 'kind.describe'; kind_id = $clap[0] }
            $json = $desc | ConvertTo-Json -Depth 30 -Compress
            Write-Text (Join-Path $OutDir 'clap-describe.json') $json
            Check ($json -match 'default') 'kind.describe lists parameters' "$($json.Length) bytes; see clap-describe.json"
        }
        $left = @(Get-CimInstance Win32_Process -Filter "Name = 'jstream-plugin-host.exe'" -ErrorAction SilentlyContinue | ForEach-Object { $_.CommandLine })
        Check (-not $left) 'no plugin host left after the scan' $(if ($left) { "$($left.Count) running: $($left -join ' | ')" } else { 'none running' })
    } catch { Fail 'script' $_.Exception.Message }
    $g = Stop-Daemon $d -Graceful
    Start-Sleep -Seconds 2
    $left = @(Get-Process -Name 'jstream-plugin-host' -ErrorAction SilentlyContinue)
    Check (-not $left -and $g -and $g.Exited) 'none after Ctrl-C either' "$($left.Count) running; daemon exit $(if ($g) { $g.Code } else { 'n/a' })"
}

# --- Run ---------------------------------------------------------------------------------------

function Get-MachineInfo {
    $os = Get-CimInstance Win32_OperatingSystem
    $gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) (driver $($_.DriverVersion))" })
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    [ordered]@{
        os = "$($os.Caption) $($os.Version) build $($os.BuildNumber)"
        gpu = $gpus -join '; '
        cpu = $cpu
        powershell = "$($PSVersionTable.PSEdition) $($PSVersionTable.PSVersion)"
        rust = ((& rustc --version 2>$null) -join '')
    }
}

$started = Get-Date
$Machine = Get-MachineInfo
Section 'JStream Windows hardware pass'
Say "Run folder: $OutDir"
Say "Machine: $($Machine.os); $($Machine.gpu)"
Say "Checks: $(($Selected | ForEach-Object { $_.Id }) -join ', ')"
if (-not (Find-Tool 'cargo')) { Write-Error 'cargo is not on PATH'; exit 2 }

$interrupted = $true
try {
    foreach ($t in $Selected) {
        $script:Task = $t
        Section "$($t.Id): $($t.Title) [$($t.Issues); from $($t.Src)]"
        try {
            [void](Get-Source $t.Src)
            & "Test-$($t.Fn)"
        } catch {
            Fail 'script' $_.Exception.Message
        }
    }
    $interrupted = $false
} finally {
    $script:Task = $null
    if ($interrupted) { Warn 'run' 'stopped before every check ran' }

    # report.md: what ran, from where, and every result.
    $md = New-Object System.Text.StringBuilder
    [void]$md.AppendLine('# JStream Windows hardware pass')
    [void]$md.AppendLine('')
    foreach ($k in $Machine.Keys) { [void]$md.AppendLine("- **$k**: $($Machine[$k])") }
    [void]$md.AppendLine("- **took**: $([Math]::Round(((Get-Date) - $started).TotalMinutes)) min")
    [void]$md.AppendLine('')
    [void]$md.AppendLine('| source | ref | commit |')
    [void]$md.AppendLine('|---|---|---|')
    foreach ($s in $script:SourceInfo.Values) { [void]$md.AppendLine("| $($s.Name) | $($s.Ref) | $($s.Commit) |") }
    foreach ($t in $Selected) {
        $rs = @($Results | Where-Object { $_.Task -eq $t.Id })
        if (-not $rs) { continue }
        [void]$md.AppendLine('')
        [void]$md.AppendLine("## $($t.Id): $($t.Title) ($($t.Issues))")
        [void]$md.AppendLine('')
        [void]$md.AppendLine('| result | check | detail |')
        [void]$md.AppendLine('|---|---|---|')
        foreach ($r in $rs) { [void]$md.AppendLine("| $($r.Status) | $($r.Check) | $(($r.Detail -replace '\|', '/') -replace '[\r\n]+', ' ') |") }
    }
    [void]$md.AppendLine('')
    [void]$md.AppendLine('## Not in this pass')
    [void]$md.AppendLine('')
    foreach ($n in $NotRun) { [void]$md.AppendLine("- $n") }
    $report = Join-Path $OutDir 'report.md'
    Write-Text $report $md.ToString()
    Write-Text (Join-Path $OutDir 'results.json') (@{ machine = $Machine; sources = @($script:SourceInfo.Values | Select-Object Name, Ref, Commit); results = $Results } | ConvertTo-Json -Depth 6)

    try { Stop-Transcript | Out-Null } catch {}

    # The zip: text files only, with the run's tokens redacted. Token files, key files and
    # recordings stay out.
    $stage = Join-Path $OutDir '.zip-stage'
    Remove-Item -Recurse -Force -LiteralPath $stage -ErrorAction SilentlyContinue
    $textExt = @('.md', '.json', '.log', '.txt', '.jsonl')
    foreach ($f in @(Get-ChildItem -LiteralPath $OutDir -Recurse -File | Where-Object {
            $textExt -contains $_.Extension -and $_.FullName -notlike "$stage*" -and $_.Name -notmatch 'token' })) {
        $rel = $f.FullName.Substring($OutDir.Length).TrimStart('\')
        $dest = Join-Path $stage $rel
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
        $text = [IO.File]::ReadAllText($f.FullName)
        foreach ($s in $script:Secrets) { if ($s) { $text = $text.Replace($s, '***') } }
        Write-Text $dest $text
    }
    $zip = "$OutDir.zip"
    Remove-Item -Force -LiteralPath $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
    Remove-Item -Recurse -Force -LiteralPath $stage -ErrorAction SilentlyContinue

    $count = { param($s) @($Results | Where-Object { $_.Status -eq $s }).Count }
    $fails = & $count 'FAIL'
    Write-Host ''
    Write-Host '================ Send this back ================' -ForegroundColor Cyan
    Write-Host "Attach this zip in the JStream project chat: $zip"
    Write-Host 'and paste these lines with it:'
    Write-Host ''
    Write-Host ("windows-pass: {0} pass, {1} fail, {2} warn, {3} skip; report {4}" -f (& $count 'PASS'), $fails, (& $count 'WARN'), (& $count 'SKIP'), $report)
    foreach ($t in $Selected) {
        $rs = @($Results | Where-Object { $_.Task -eq $t.Id })
        if (-not $rs) { continue }
        $bad = @($rs | Where-Object { $_.Status -in @('FAIL', 'WARN') } | ForEach-Object { "$($_.Status) $($_.Check)" })
        $state = 'PASS'
        if (@($rs | Where-Object { $_.Status -eq 'FAIL' })) { $state = 'FAIL' }
        elseif (-not @($rs | Where-Object { $_.Status -eq 'PASS' })) { $state = 'SKIP' }
        Write-Host ("  {0,-14} {1,-4} {2}" -f $t.Id, $state, ($bad -join '; '))
    }
    Write-Host '================================================' -ForegroundColor Cyan
}
if ($fails) { exit 1 }
exit 0
