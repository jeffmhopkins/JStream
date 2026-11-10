<#
.SYNOPSIS
The release pass: checks an unzipped JStream release on this Windows machine, the
automatic checks first, then the release's checks by hand, and writes one report.

.DESCRIPTION
What each check does and what passes is in docs/runbooks/windows-hardware-pass.md (The
release pass, and the alpha.5 checklist). Unlike windows-pass.ps1 it builds nothing: it
runs the release's own jstream-daemon.exe and jstream-cli.exe, from the zip you
downloaded or the folder you unzipped it to. No Rust, git or checkout is needed.

The automatic checks start a daemon of their own on a free port, with its own token, log
folder and data folder in the run folder (APPDATA points there for it), so your
settings, projects, recent list and stream keys are never read or changed. They check
the package, the versions, the kinds and the profile, snapshots of a browser page and a
display capture, saving and opening projects, the webcam's formats and controls, the
bench, and a graceful stop.

Then it walks the manual checklist: for each item it says what to do in jstream.exe and
asks pass, fail or skip. Two of them it helps with: it sends an agent's hello to the app
for the ask-before-agent item, and it runs a daemon with a PTZ controller endpoint for
the controller item and reads its counters.

At the end it writes report.md and results.json into the run folder, zips them with the
logs (with the run's tokens replaced by ***), and prints the zip's path and a summary to
paste back.

Runs on Windows PowerShell 5.1 and PowerShell 7.

.PARAMETER Zip
The release zip. Its .sha256 file, when it is beside the zip, is checked too. The zip is
unzipped into the run's work folder.

.PARAMETER Root
The folder the release was unzipped to (the one holding jstream.exe). Instead of -Zip.
With neither, the script looks for the zip in your Downloads folder.

.PARAMETER Expect
The version the release should report. Default 0.1.0-alpha.5.

.PARAMETER AutomatedOnly
Run the automatic checks only, and ask nothing.

.PARAMETER ManualOnly
Skip the automatic checks and go straight to the manual checklist.

.PARAMETER Media
A video file for the bench's media scenario. Without it the script makes one with ffmpeg
when ffmpeg is on PATH, and otherwise the bench skips that scenario.

.PARAMETER PtzController
The IP address of your PTZ controller (a joystick keyboard or a control app), for the
controller check. Asked for at that step when it isn't given.

.PARAMETER WorkDir
Where runs and the unzipped release go. Default %LOCALAPPDATA%\JStream-release-pass.
#>
[CmdletBinding()]
param(
    [string]$Zip,
    [string]$Root,
    [string]$Expect = '0.1.0-alpha.5',
    [switch]$AutomatedOnly,
    [switch]$ManualOnly,
    [string]$Media,
    [string]$PtzController,
    [string]$WorkDir
)

Set-StrictMode -Version 3
# 'Continue', not 'Stop': in Windows PowerShell 5.1, 'Stop' turns a native program's
# stderr into a terminating error.
$ErrorActionPreference = 'Continue'

if (-not (($PSVersionTable.PSEdition -eq 'Desktop') -or $IsWindows)) {
    Write-Error 'The release pass runs on Windows only'; exit 2
}
if ($AutomatedOnly -and $ManualOnly) { Write-Error '-AutomatedOnly and -ManualOnly together run nothing'; exit 2 }
if ($Zip -and $Root) { Write-Error 'give -Zip or -Root, not both'; exit 2 }

$Utf8 = New-Object System.Text.UTF8Encoding($false)
if (-not $WorkDir) { $WorkDir = Join-Path $env:LOCALAPPDATA 'JStream-release-pass' }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
$WorkDir = (Resolve-Path -LiteralPath $WorkDir).Path
$OutDir = Join-Path $WorkDir ('runs\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$Transcript = Join-Path $OutDir 'console.log'
try { Start-Transcript -LiteralPath $Transcript -Append | Out-Null } catch {}

# --- Output and questions --------------------------------------------------------------

function Say([string]$Text, [string]$Color = 'Gray') { Write-Host $Text -ForegroundColor $Color }
function Section([string]$Text) { Write-Host ''; Write-Host "=== $Text ===" -ForegroundColor Cyan }

$Results = New-Object System.Collections.Generic.List[object]
$script:Item = '(run)'
# Tokens the run read or wrote: redacted from everything that goes into the zip.
$script:Secrets = New-Object System.Collections.Generic.List[string]

function Add-Result([string]$Status, [string]$Check, [string]$Detail) {
    $Results.Add([pscustomobject]@{ Item = $script:Item; Status = $Status; Check = $Check; Detail = $Detail })
    $color = @{ PASS = 'Green'; FAIL = 'Red'; WARN = 'Yellow'; SKIP = 'DarkGray' }[$Status]
    Write-Host ('{0,-4} {1}: {2}' -f $Status, $Check, $Detail) -ForegroundColor $color
}
function Pass([string]$Check, [string]$Detail) { Add-Result 'PASS' $Check $Detail }
function Fail([string]$Check, [string]$Detail) { Add-Result 'FAIL' $Check $Detail }
function Warn([string]$Check, [string]$Detail) { Add-Result 'WARN' $Check $Detail }
function Skip([string]$Check, [string]$Detail) { Add-Result 'SKIP' $Check $Detail }
function Check([bool]$Ok, [string]$Check, [string]$Detail) {
    if ($Ok) { Pass $Check $Detail } else { Fail $Check $Detail }
}

# Every line that needs you starts with a magenta ">>> YOU:" and a beep.
function You([string]$Text) {
    Write-Host ''
    try { [Console]::Beep(880, 150) } catch {}
    Write-Host ">>> YOU: $Text" -ForegroundColor Magenta
}

# A check only a person can judge: asks pass, fail or skip, and records the answer.
function Ask([string]$Check, [string]$Question) {
    if ($AutomatedOnly) { Skip $Check 'needs a person (-AutomatedOnly)'; return }
    You $Question
    while ($true) {
        $a = (Read-Host '    [p]ass, [f]ail or [s]kip').Trim().ToLower()
        if ($a -in @('p', 'pass')) { Pass $Check 'you: pass'; return }
        if ($a -in @('s', 'skip')) { Skip $Check 'skipped by you'; return }
        if ($a -in @('f', 'fail')) {
            $why = Read-Host '    What did you see? (one line, Enter for none)'
            Fail $Check "you: $why"; return
        }
    }
}

# Something you do by hand, then press Enter. False when you type s.
function Prompt-Do([string]$What) {
    if ($AutomatedOnly) { return $false }
    You $What
    $a = Read-Host '    Press Enter when done, or type s to skip'
    $a.Trim().ToLower() -ne 's'
}

function Write-Text([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path, $Text, $Utf8) }

function Find-Tool([string]$Name) {
    $c = Get-Command $Name -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $null
}

# --- Native processes ------------------------------------------------------------------

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

# Starts a process with its own hidden console and piped stdout and stderr. $Env sets
# variables for it alone.
function Start-Native([string]$File, [string[]]$Arguments, [hashtable]$Env) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $File
    $psi.Arguments = ($Arguments | ForEach-Object { Format-Arg $_ }) -join ' '
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.WorkingDirectory = $OutDir
    if ($Env) { foreach ($k in $Env.Keys) { $psi.EnvironmentVariables[$k] = $Env[$k] } }
    [System.Diagnostics.Process]::Start($psi)
}

# Runs a process to completion. Returns its exit code ('timeout' when killed), stdout and
# stderr.
function Invoke-Native([string]$File, [string[]]$Arguments, [int]$TimeoutSec = 120) {
    $p = Start-Native $File $Arguments
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    $code = 'timeout'
    if ($p.WaitForExit($TimeoutSec * 1000)) { $p.WaitForExit(); $code = $p.ExitCode } else { try { $p.Kill() } catch {} }
    [pscustomobject]@{ Code = $code; Out = $out.Result; Err = $err.Result }
}

function Stop-Native($p) {
    if ($p -and -not $p.HasExited) { try { $p.Kill() } catch {}; [void]$p.WaitForExit(10000) }
}

function Get-PowerShellExe { (Get-Process -Id $PID).Path }

# Sends Ctrl-C to a process started by Start-Native (it has its own console), from a
# helper process that attaches to that console: the daemon's graceful stop.
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

# --- The release -------------------------------------------------------------------------

$PackageFiles = @(
    'jstream.exe', 'jstream-daemon.exe', 'jstream-cli.exe', 'jstream_vcam_windows.dll',
    'jstream-plugin-host.exe', 'README.txt', 'browser\jstream-browser-helper.exe', 'ui\index.html'
)
$ZipName = "jstream-v$Expect-windows-x64.zip"

# Finds the release: -Root, else -Zip (checked and unzipped), else the zip in Downloads.
# Returns the folder holding jstream.exe, or $null.
function Get-Release {
    if ($Root) {
        if (-not (Test-Path -LiteralPath $Root -PathType Container)) { Fail 'release' "-Root: no such folder: $Root"; return $null }
        $folder = (Resolve-Path -LiteralPath $Root).Path
        Skip 'zip checksum' 'given an unzipped folder (-Root)'
    } else {
        $file = $Zip
        if (-not $file) {
            $guess = Join-Path (Join-Path $env:USERPROFILE 'Downloads') $ZipName
            if (Test-Path -LiteralPath $guess -PathType Leaf) { $file = $guess }
            elseif (-not $AutomatedOnly) {
                You "Where is the release zip ($ZipName) or the folder you unzipped it to?"
                $file = (Read-Host '    Path').Trim().Trim('"')
            }
        }
        if (-not $file) { Fail 'release' "no -Zip or -Root, and $ZipName is not in Downloads"; return $null }
        if (Test-Path -LiteralPath $file -PathType Container) {
            $folder = (Resolve-Path -LiteralPath $file).Path
            Skip 'zip checksum' 'given an unzipped folder'
        } elseif (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
            Fail 'release' "no such file: $file"; return $null
        } else {
            $file = (Resolve-Path -LiteralPath $file).Path
            $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLower()
            $sumFile = "$file.sha256"
            if (Test-Path -LiteralPath $sumFile) {
                $want = ((Get-Content -LiteralPath $sumFile -Raw).Trim() -split '\s+')[0].ToLower()
                Check ($hash -eq $want) 'zip checksum' "sha256 $hash; the .sha256 file says $want"
            } else {
                Warn 'zip checksum' "sha256 $hash; no $([IO.Path]::GetFileName($sumFile)) beside the zip to compare with"
            }
            $folder = Join-Path $WorkDir ('release\' + [IO.Path]::GetFileNameWithoutExtension($file))
            Remove-Item -Recurse -Force -LiteralPath $folder -ErrorAction SilentlyContinue
            Say "Unzipping to $folder ..."
            Expand-Archive -LiteralPath $file -DestinationPath $folder -Force
        }
    }
    # The zip may hold one top folder.
    if (-not (Test-Path -LiteralPath (Join-Path $folder 'jstream.exe'))) {
        $inner = @(Get-ChildItem -LiteralPath $folder -Directory | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'jstream.exe') })
        if ($inner.Count -eq 1) { $folder = $inner[0].FullName }
    }
    $missing = @($PackageFiles | Where-Object { -not (Test-Path -LiteralPath (Join-Path $folder $_)) })
    $count = @(Get-ChildItem -LiteralPath $folder -Recurse -File).Count
    Check (-not $missing) 'package files' $(if ($missing) { "missing: $($missing -join ', ') in $folder" } else { "$count files in $folder, every expected one there" })
    if (-not (Test-Path -LiteralPath (Join-Path $folder 'jstream-daemon.exe'))) { return $null }
    $folder
}

# --- The daemon ---------------------------------------------------------------------------

Add-Type -AssemblyName System.Net.Http
$handler = New-Object System.Net.Http.HttpClientHandler
$handler.UseProxy = $false
$Http = New-Object System.Net.Http.HttpClient($handler)
$Http.Timeout = [TimeSpan]::FromSeconds(30)
$script:DaemonN = 0

# Starts the release's daemon with its own token file, log folder and data folder (APPDATA
# for it alone, so nothing of yours is read or written). Returns the daemon; its WsUrl is
# $null when it didn't come up.
function Start-Daemon([string[]]$Arguments = @()) {
    $script:DaemonN++
    $dir = Join-Path $OutDir ("daemon-{0:d2}" -f $script:DaemonN)
    $data = Join-Path $dir 'appdata'
    New-Item -ItemType Directory -Force -Path $data | Out-Null
    $tokenFile = Join-Path $dir 'token'
    $logDir = Join-Path $dir 'logs'
    $a = @('--token-file', $tokenFile, '--log-dir', $logDir) + $Arguments
    $p = Start-Native $script:Daemon $a @{ APPDATA = $data }
    $err = $p.StandardError.ReadToEndAsync()
    $d = [pscustomobject]@{
        Proc = $p; Dir = $dir; TokenFile = $tokenFile; LogDir = $logDir; Token = $null
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

function Need-Daemon($d) {
    if ($d.WsUrl) { return $true }
    $why = ($d.ErrTask.Result -split "`n" | Where-Object { $_ -match 'error' } | Select-Object -First 2) -join ' '
    if ($d.Code -eq 3) { $why = "Media Foundation is missing (an N edition of Windows?): $why" }
    Fail 'daemon starts' "did not start (exit $($d.Code)): $why"
    $false
}

# Stops a daemon. With -Graceful, by Ctrl-C, and returns whether it printed `stopped` and
# exited 0; otherwise it's ended.
function Stop-Daemon($d, [switch]$Graceful) {
    if (-not $d -or -not $d.Proc) { return $null }
    if ($d.Session) { try { [void](Send-Mcp $d ([System.Net.Http.HttpMethod]::Delete) $null) } catch {}; $d.Session = $null }
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
    $client = $Http
    if ($d.PSObject.Properties['Http'] -and $d.Http) { $client = $d.Http }
    $msg = New-Object System.Net.Http.HttpRequestMessage($HttpMethod, $d.McpUrl)
    [void]$msg.Headers.TryAddWithoutValidation('Authorization', "Bearer $($d.Token)")
    [void]$msg.Headers.TryAddWithoutValidation('Accept', 'application/json, text/event-stream')
    if ($d.Session) {
        [void]$msg.Headers.TryAddWithoutValidation('Mcp-Session-Id', $d.Session)
        [void]$msg.Headers.TryAddWithoutValidation('MCP-Protocol-Version', '2025-11-25')
    }
    if ($Body) { $msg.Content = New-Object System.Net.Http.StringContent($Body, [System.Text.Encoding]::UTF8, 'application/json') }
    $client.SendAsync($msg).GetAwaiter().GetResult()
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
        $init = Invoke-Mcp $d 'initialize' @{ protocolVersion = '2025-11-25'; capabilities = @{}; clientInfo = @{ name = 'jstream-release-pass'; version = '1' } }
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

# One jstream-cli call with --json against $d. Returns the parsed reply, or $null with
# $script:LastCli holding the run.
$script:LastCli = $null
function Invoke-Cli($d, [string[]]$Arguments, [int]$TimeoutSec = 120) {
    $r = Invoke-Native $script:Cli (@('--json', '--url', $d.WsUrl, '--token-file', $d.TokenFile) + $Arguments) $TimeoutSec
    $script:LastCli = $r
    if ($r.Code -ne 0) { return $null }
    $r.Out.Trim() | ConvertFrom-Json
}
function Get-CliError {
    $r = $script:LastCli
    $text = "$($r.Err)".Trim()
    try { $e = ($text | ConvertFrom-Json).error; return "$($e.message) ($($e.code), exit $($r.Code))" }
    catch { return "$text (exit $($r.Code))" }
}

# A new scene on program holding one source, filling a 1920x1080 canvas. Each call uses
# new ids, so a scene that couldn't be removed never blocks the next one.
$script:SceneN = 0
function New-ProgramScene($d, [hashtable]$Kind, [hashtable]$Settings) {
    $script:SceneN++
    $n = $script:SceneN
    $create = @{ type = 'source.create'; source_id = "src_$n"; name = "Source $n"; kind = $Kind }
    if ($Settings) { $create.settings = $Settings }
    Invoke-Batch $d @(
        @{ type = 'canvas.set_size'; width = 1920; height = 1080 }
        @{ type = 'scene.create'; scene_id = "main_$n"; name = "Main $n" }
        $create
        @{ type = 'item.add'; scene_id = "main_$n"; item_id = "item_$n"; source_id = "src_$n"
           geometry = @{ center = @{ x = 960; y = 540 }; size = @{ width = 1920; height = 1080 }; fit_mode = 'fit' } }
    ) | Out-Null
    Invoke-Op $d 'scene' @{ type = 'scene.switch'; scene_id = "main_$n" } | Out-Null
}

# Removes the last New-ProgramScene's item and source, so a capture or a page stops; its
# empty scene stays on program.
function Remove-ProgramScene($d) {
    $n = $script:SceneN
    try { Invoke-Batch $d @(@{ type = 'item.remove'; item_id = "item_$n" }, @{ type = 'source.delete'; source_id = "src_$n" }) | Out-Null }
    catch { Warn 'clean up' "item_$n and src_$n not removed: $($_.Exception.Message)" }
}

# --- Snapshots -----------------------------------------------------------------------------

Add-Type -AssemblyName System.Drawing

# The program mix as a PNG file. Returns the file, or $null with the reason in
# $script:SnapError.
$script:SnapError = $null
function Save-Snapshot($d, [string]$Path) {
    $r = Invoke-Cli $d @('video', 'snapshot')
    if (-not $r) { $script:SnapError = Get-CliError; return $null }
    $bytes = [Convert]::FromBase64String($r.result.result.png)
    [IO.File]::WriteAllBytes($Path, $bytes)
    $Path
}

# What a PNG looks like: its size, the colour at its centre, and how many of 64 sampled
# points differ from the first one by more than a little.
function Get-Look([string]$Path) {
    $bmp = [System.Drawing.Bitmap]::FromFile($Path)
    try {
        $c = $bmp.GetPixel([int]($bmp.Width / 2), [int]($bmp.Height / 2))
        $first = $bmp.GetPixel(0, 0)
        $varied = 0
        for ($i = 0; $i -lt 8; $i++) {
            for ($j = 0; $j -lt 8; $j++) {
                $p = $bmp.GetPixel([int](($bmp.Width - 1) * ($i + 0.5) / 8), [int](($bmp.Height - 1) * ($j + 0.5) / 8))
                if ([Math]::Abs($p.R - $first.R) + [Math]::Abs($p.G - $first.G) + [Math]::Abs($p.B - $first.B) -gt 24) { $varied++ }
            }
        }
        [pscustomobject]@{ Width = $bmp.Width; Height = $bmp.Height; R = $c.R; G = $c.G; B = $c.B; Varied = $varied }
    } finally { $bmp.Dispose() }
}

# --- The automatic checks --------------------------------------------------------------

$KindsExpected = @(
    # Filters (#1226, #1227) and the depth-aware 3D filters (#1231).
    'glow', 'bloom', 'vignette', 'border', 'depth_fog', 'volumetric_fog', 'depth_bloom', 'ambient_occlusion', 'floor_reflection',
    # 3D models and lights (#1231), follow pointer (#1225).
    'model', 'light', 'follow_pointer',
    # MIDI visualisation and network audio (#1222, #1223, #1224).
    'midi_piano_roll', 'midi_keyboard', 'midi_particles', 'midi_cc', 'midi_pads', 'midi_beat', 'network_audio',
    # The capture and browser kinds everything else needs.
    'browser', 'display', 'window', 'webcam', 'media'
)

function Test-Versions {
    foreach ($exe in @('jstream-daemon', 'jstream-cli')) {
        $path = Join-Path $script:Release "$exe.exe"
        $arg = '--version'
        if ($exe -eq 'jstream-cli') { $arg = 'version' }
        $r = Invoke-Native $path @($arg) 30
        $text = (("$($r.Out) $($r.Err)") -replace '\s+', ' ').Trim()
        Check ($r.Code -eq 0 -and $text -match [regex]::Escape($Expect)) "$exe version" $text
    }
}

function Test-Engine($d) {
    # kind.list: the kinds this release adds, and the ones it builds on.
    $kinds = Invoke-Op $d 'kind' @{ type = 'kind.list' }
    $json = $kinds | ConvertTo-Json -Depth 30 -Compress
    $have = @([regex]::Matches($json, '"kind_id":"([^"]+)"') | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
    $missing = @($KindsExpected | Where-Object { $have -notcontains $_ })
    Check (-not $missing) 'kind.list has the alpha.5 kinds' $(if ($missing) { "missing: $($missing -join ', ') ($($have.Count) kinds)" } else { "$($have.Count) kinds, all $($KindsExpected.Count) expected ones" })

    # profile.get: the thread model's threads (#1221), with their CPU, and the new counters.
    New-ProgramScene $d @{ type = 'builtin'; kind = 'color' } @{ color = '#2040a0ff' }
    Start-Sleep -Seconds 6
    $p = Invoke-Cli $d @('profile', 'get', '5')
    if (-not $p) { Fail 'profile.get' (Get-CliError); return }
    $prof = $p.result.result
    Write-Text (Join-Path $OutDir 'profile.json') ($prof | ConvertTo-Json -Depth 20)
    $names = @($prof.threads | ForEach-Object { if ($_.PSObject.Properties['name'] -and $_.name) { $_.name } elseif ($_.PSObject.Properties['remote_name']) { $_.remote_name } })
    $want = @('jstream-render', 'jstream-frames')
    $lack = @($want | Where-Object { $names -notcontains $_ })
    Check ($prof.thread_cpu -and -not $lack) 'profile.get threads' "thread_cpu: $($prof.thread_cpu); threads: $(($names | Select-Object -Unique) -join ', ')$(if ($lack) { "; missing: $($lack -join ', ')" })"
    $fields = @('views', 'warm', 'streams', 'ptz_controllers', 'ptz_cameras')
    $absent = @($fields | Where-Object { -not $prof.PSObject.Properties[$_] })
    Check (-not $absent) 'profile.get fields' $(if ($absent) { "missing: $($absent -join ', ')" } else { "has $($fields -join ', ')" })
    $drawer = @($prof.drawers | Where-Object { $_.drawer -eq 'program' } | Select-Object -First 1)
    if ($drawer) {
        $d0 = $drawer[0]
        $ok = ($d0.skipped -eq 0 -and $d0.over_budget -eq 0)
        $detail = "program: $($d0.frames) frames, p95 $($d0.frame_p95_ms) ms, max $($d0.frame_max_ms) ms, over budget $($d0.over_budget), skipped $($d0.skipped)"
        if ($ok) { Pass 'program frames on time' $detail } else { Warn 'program frames on time' $detail }
    } else { Warn 'program frames on time' 'no program drawer in profile.get' }
    Remove-ProgramScene $d
}

# Snapshots draw browser pages and screen captures on Windows (#1184, #1194, #1215).
function Test-Snapshots($d) {
    $html = '<html><body style="margin:0;background:#00ff00"></body></html>'
    New-ProgramScene $d @{ type = 'builtin'; kind = 'browser' } @{ html = $html; css = 'body { margin: 0; }' }
    $file = Join-Path $OutDir 'snapshot-browser.png'
    $look = $null
    $deadline = (Get-Date).AddSeconds(30)
    do {
        Start-Sleep -Seconds 2
        if (Save-Snapshot $d $file) {
            $look = Get-Look $file
            if ($look.G -gt 200 -and $look.R -lt 60 -and $look.B -lt 60) { break }
        }
    } while ((Get-Date) -lt $deadline)
    if (-not $look) { Fail 'snapshot of a browser page' "video.snapshot failed: $script:SnapError" }
    else {
        $ok = ($look.G -gt 200 -and $look.R -lt 60 -and $look.B -lt 60)
        Check $ok 'snapshot of a browser page' "$($look.Width)x$($look.Height), centre rgb($($look.R),$($look.G),$($look.B)), want the page's green; $file"
    }
    Remove-ProgramScene $d

    New-ProgramScene $d @{ type = 'builtin'; kind = 'display' } @{ target = '' }
    $file = Join-Path $OutDir 'snapshot-display.png'
    Start-Sleep -Seconds 4
    if (-not (Save-Snapshot $d $file)) { Fail 'snapshot of a display capture' "video.snapshot failed: $script:SnapError" }
    else {
        $look = Get-Look $file
        $ok = ($look.Varied -ge 4)
        Check $ok 'snapshot of a display capture' "$($look.Width)x$($look.Height), $($look.Varied) of 64 sampled points differ from the corner (a uniform picture means the capture drew nothing); $file"
    }
    Remove-ProgramScene $d
}

# Save and load projects (#1218): new, save as, open, and the recent list.
function Test-Projects($d) {
    $dir = Join-Path $OutDir 'projects'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $a = Join-Path $dir 'first.json'; $b = Join-Path $dir 'second.json'; $c = Join-Path $dir 'third.json'
    if (-not (Invoke-Cli $d @('collection', 'new', $a))) { Fail 'collection.new' (Get-CliError); return }
    Invoke-Batch $d @(@{ type = 'scene.create'; scene_id = 'saved'; name = 'Saved scene' }) | Out-Null
    if (-not (Invoke-Cli $d @('collection', 'save-as', $b))) { Fail 'collection.save_as' (Get-CliError); return }
    if (-not (Invoke-Cli $d @('collection', 'new', $c))) { Fail 'collection.new' (Get-CliError); return }
    $before = Invoke-Cli $d @('scene', 'list')
    if (-not (Invoke-Cli $d @('collection', 'open', $b))) { Fail 'collection.open' (Get-CliError); return }
    $after = Invoke-Cli $d @('scene', 'list')
    $info = Invoke-Cli $d @('collection', 'info')
    $hasIn = { param($r) ($r | ConvertTo-Json -Depth 10 -Compress) -match '"saved"' }
    $norm = { param($p) ("$p" -replace '/', '\').ToLower() }
    $recent = @()
    if ($info) { $recent = @($info.result.result.recent | ForEach-Object { & $norm $_ }) }
    $path = ''
    if ($info) { $path = & $norm $info.result.result.path }
    Check ((Test-Path -LiteralPath $b) -and -not (& $hasIn $before) -and (& $hasIn $after)) 'save, new, open' "second.json written: $(Test-Path -LiteralPath $b); the new project is empty: $(-not (& $hasIn $before)); opening second.json brings its scene back: $(& $hasIn $after)"
    Check ($path -eq (& $norm $b) -and $recent.Count -ge 3 -and $recent[0] -eq (& $norm $b)) 'collection.info and the recent list' "path $path; recent: $($recent -join ', ')"
}

# Webcam Properties (#1195, #1196): the formats and controls the engine reads.
function Test-Webcams($d) {
    $r = Invoke-Cli $d @('capture', 'targets')
    if (-not $r) { Fail 'capture.targets' (Get-CliError); return 0 }
    $cams = @($r.result.result.targets | Where-Object { $_.kind -eq 'device' })
    if (-not $cams) { Warn 'webcam formats and controls' 'no webcam found: plug one in for this and the bench''s webcam scenario'; return 0 }
    foreach ($c in $cams) {
        $formats = @(); if ($c.PSObject.Properties['formats']) { $formats = @($c.formats) }
        $controls = @(); if ($c.PSObject.Properties['controls']) { $controls = @($c.controls) }
        $f = ($formats | Select-Object -First 4 | ForEach-Object { "$($_.width)x$($_.height)@$($_.fps) $($_.pixel_format)" }) -join ', '
        $k = ($controls | ForEach-Object { $_.name }) -join ', '
        Check ($formats.Count -gt 0) "webcam formats: $($c.name)" "$($formats.Count) formats ($f); controls: $(if ($k) { $k } else { 'none' })"
    }
    $cams.Count
}

# The bench (#1220): every scenario, the display, and the webcam when there is one.
function Test-Bench($d, [int]$Webcams) {
    $out = Join-Path $OutDir 'bench'
    $scen = 'all,display'
    if ($Webcams -gt 0) { $scen += ',webcam' }
    $benchArgs = @('bench', '--out', $out, '--scenario', $scen)
    $file = $Media
    if (-not $file) {
        $ffmpeg = Find-Tool 'ffmpeg'
        if ($ffmpeg) {
            $file = Join-Path $OutDir 'bench-media.mp4'
            $r = Invoke-Native $ffmpeg @('-hide_banner', '-nostdin', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=duration=10:size=1920x1080:rate=30',
                '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=10', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', $file) 120
            if ($r.Code -ne 0) { $file = $null }
        }
    }
    if ($file) { $benchArgs += @('--media', $file) } else { Say 'No -Media and no ffmpeg: the bench skips its media scenario.' 'DarkGray' }
    You "The bench runs for about $(if ($Webcams) { 8 } else { 7 }) minutes, recording each scenario. Leave the machine alone (the display scenario captures your screen)."
    $r = Invoke-Native $script:Cli (@('--url', $d.WsUrl, '--token-file', $d.TokenFile) + $benchArgs) 1800
    Write-Text (Join-Path $OutDir 'bench.log') "$($r.Out)`r`n$($r.Err)"
    $summary = Join-Path $out 'summary.md'
    $lines = ''
    if (Test-Path -LiteralPath $summary) {
        $lines = (Get-Content -LiteralPath $summary | Where-Object { $_ -match '\b(pass|fail|skip)' } | Select-Object -First 12) -join ' / '
    }
    switch ("$($r.Code)") {
        '0' { Pass 'bench' "every gate passed ($scen); $summary" }
        '4' { Fail 'bench' "a gate failed: $lines; see $summary" }
        '5' { Fail 'bench' "the bench couldn't finish: $(("$($r.Err)").Trim()); see bench.log" }
        default { Fail 'bench' "exit $($r.Code): $(("$($r.Err)").Trim())" }
    }
}

function Invoke-Automated {
    $script:Item = 'A'
    Section 'Automatic checks'
    Test-Versions
    $d = Start-Daemon
    if (-not (Need-Daemon $d)) { return }
    Pass 'daemon starts' "listening on $($d.WsUrl), with its own data folder"
    $cams = 0
    try {
        foreach ($t in @('Test-Engine', 'Test-Snapshots', 'Test-Projects')) {
            try { & $t $d } catch { Fail $t $_.Exception.Message }
        }
        try { $cams = [int](Test-Webcams $d | Select-Object -Last 1) } catch { Fail 'Test-Webcams' $_.Exception.Message }
        try { Test-Bench $d $cams } catch { Fail 'Test-Bench' $_.Exception.Message }
    } finally {
        $g = Stop-Daemon $d -Graceful
        if ($g) { Check ($g.Exited -and $g.Code -eq 0 -and $g.Stopped) 'graceful stop' "Ctrl-C sent: $($g.Sent); exited: $($g.Exited); exit code: $($g.Code); printed stopped: $($g.Stopped)" }
        else { Fail 'graceful stop' 'the daemon had already exited' }
    }
}

# --- The manual checks -------------------------------------------------------------------

# Ask before a new agent connects (#1229): sends an agent's hello to the running app.
function Test-AskAgent {
    $why = ''
    $tokenFile = Join-Path $env:APPDATA 'JStream\token'
    if (-not (Prompt-Do 'Close jstream.exe if it is open. Press Enter and the script starts it from the release folder with --mcp, so agents can reach it on 127.0.0.1:4455. Leave File > Settings > General > "Ask before a new agent connects" on.')) { Skip 'ask before an agent' 'skipped by you'; return }
    Start-Process -FilePath (Join-Path $script:Release 'jstream.exe') -ArgumentList '--mcp' -WorkingDirectory $script:Release | Out-Null
    if (-not (Prompt-Do 'Wait for the JStream window to open and connect.')) { Skip 'ask before an agent' 'skipped by you'; return }
    if (-not (Test-Path -LiteralPath $tokenFile)) { Fail 'ask before an agent' "no token at $tokenFile; did jstream.exe start with --mcp?"; return }
    $token = (Get-Content -LiteralPath $tokenFile -Raw).Trim()
    $script:Secrets.Add($token)
    # The hello waits for your answer, up to two minutes.
    $slow = New-Object System.Net.Http.HttpClient($handler)
    $slow.Timeout = [TimeSpan]::FromSeconds(150)
    $app = [pscustomobject]@{ McpUrl = 'http://127.0.0.1:4455/mcp'; Token = $token; Session = $null; Id = 0; Http = $slow }
    You 'An agent is connecting now. JStream should show "Agent wants to connect". Choose Deny.'
    $denied = $null
    try { $r = Invoke-Mcp $app 'initialize' @{ protocolVersion = '2025-11-25'; capabilities = @{}; clientInfo = @{ name = 'release-pass-agent'; version = '1' } }
        $denied = -not $r.PSObject.Properties['result'] } catch { $denied = $true; $why = $_.Exception.Message }
    Check $denied 'Deny turns the agent away' $(if ($denied) { 'the hello was refused' } else { 'the hello was answered: was the dialog shown, and Deny chosen?' })
    $app.Session = $null
    You 'It connects again. This time choose Always allow.'
    $ok = $false
    try { $r = Invoke-Mcp $app 'initialize' @{ protocolVersion = '2025-11-25'; capabilities = @{}; clientInfo = @{ name = 'release-pass-agent'; version = '1' } }
        $ok = [bool]$r.PSObject.Properties['result'] } catch { $why = $_.Exception.Message }
    Check $ok 'Always allow lets it in' $(if ($ok) { 'the hello was answered' } else { "refused: $why" })
    if ($app.Session) { try { [void](Send-Mcp $app ([System.Net.Http.HttpMethod]::Delete) $null) } catch {} }
    Ask 'listed and removable' 'Open File > Settings > Devices. Is release-pass-agent listed under Approved agents? Press Remove: is it gone?'
}

# The real PTZ controller (#1181): a daemon with a controller endpoint, a scene with a
# camera on program, and its counters read back.
function Test-PtzController {
    if ($AutomatedOnly) { Skip 'PTZ controller' 'needs a person (-AutomatedOnly)'; return }
    $ip = $PtzController
    if (-not $ip) {
        You 'What is your PTZ controller''s IP address? (Enter to skip this check)'
        $ip = (Read-Host '    IP').Trim()
    }
    if (-not $ip) { Skip 'PTZ controller' 'no controller address'; return }
    $local = $null
    try { $local = (Find-NetRoute -RemoteIPAddress $ip -ErrorAction Stop | Where-Object { $_.PSObject.Properties['IPAddress'] } | Select-Object -First 1).IPAddress } catch {}
    if (-not $local) {
        You "Which of this PC's IP addresses does the controller reach? (Find-NetRoute found none for $ip)"
        $local = (Read-Host '    IP').Trim()
    }
    if (-not $local) { Skip 'PTZ controller' 'no local address'; return }
    $d = Start-Daemon @('--ptz-controller', "$local`:52381=program", '--ptz-controller-allow', $ip)
    if (-not (Need-Daemon $d)) { return }
    try {
        Pass 'PTZ controller endpoint' "listening on $local`:52381 (VISCA over IP), answering $ip only"
        $cam = @{ position = @{ x = 960.0; y = 540.0; z = 1303.6753 }; target = @{ x = 960.0; y = 540.0; z = 0.0 }; fov_deg = 45.0; roll_deg = 0.0 }
        Invoke-Batch $d @(
            @{ type = 'canvas.set_size'; width = 1920; height = 1080 }
            @{ type = 'scene.create'; scene_id = 'main'; name = 'Main' }
            @{ type = 'source.create'; source_id = 'bg'; name = 'Background'; kind = @{ type = 'builtin'; kind = 'color' }; settings = @{ color = '#203040ff' } }
            @{ type = 'source.create'; source_id = 'box'; name = 'Box'; kind = @{ type = 'builtin'; kind = 'shape' }; settings = @{ width = 300; height = 300; color = '#ffb000ff' } }
            @{ type = 'item.add'; scene_id = 'main'; item_id = 'bg_item'; source_id = 'bg'
               geometry = @{ center = @{ x = 960; y = 540 }; size = @{ width = 1920; height = 1080 }; fit_mode = 'stretch' } }
            @{ type = 'item.add'; scene_id = 'main'; item_id = 'box_item'; source_id = 'box'
               geometry = @{ center = @{ x = 960; y = 540 }; size = @{ width = 300; height = 300 }; fit_mode = 'fit' } }
            @{ type = 'scene.set_camera'; scene_id = 'main'; camera = $cam }
        ) | Out-Null
        Invoke-Op $d 'scene' @{ type = 'scene.switch'; scene_id = 'main' } | Out-Null
        $before = Join-Path $OutDir 'ptz-before.png'
        [void](Save-Snapshot $d $before)
        if (-not (Prompt-Do "Point the controller at $local, port 52381, VISCA over IP (UDP), camera address 1. If Windows asks whether jstream-daemon may use the network, allow private networks. Then pan right, tilt up and zoom in for a few seconds, and let go.")) { Skip 'PTZ controller' 'skipped by you'; return }
        $aim = $null
        try { $aim = (Invoke-Op $d 'scene' @{ type = 'scene.camera_aim'; scene_id = 'main' }).result.result } catch { Fail 'scene.camera_aim' $_.Exception.Message }
        $p = Invoke-Cli $d @('profile', 'get', '30')
        $ctl = $null
        if ($p) { $ctl = @($p.result.result.ptz_controllers | Select-Object -First 1) }
        if ($ctl) {
            $c = $ctl[0]
            Check ($c.answered -gt 0) 'the endpoint answered the controller' "answered $($c.answered), unallowed $($c.unallowed), malformed $($c.malformed), refused $($c.refused)"
            if ($c.unallowed -gt 0 -and $c.answered -eq 0) { Say "Packets came from an address other than $ip: check the controller's address." 'Yellow' }
        } else { Fail 'the endpoint answered the controller' "no ptz_controllers in profile.get: $(Get-CliError)" }
        if ($aim) {
            $moved = ([Math]::Abs([double]$aim.pan_deg) -gt 0.5 -or [Math]::Abs([double]$aim.tilt_deg) -gt 0.5 -or [Math]::Abs([double]$aim.zoom - 1) -gt 0.05)
            Check $moved 'the joystick moved the scene camera' "aim: pan $($aim.pan_deg) deg, tilt $($aim.tilt_deg) deg, zoom $($aim.zoom), moving $($aim.moving)"
        }
        $after = Join-Path $OutDir 'ptz-after.png'
        if ((Save-Snapshot $d $after) -and (Test-Path -LiteralPath $before)) {
            $same = (Get-FileHash -LiteralPath $before).Hash -eq (Get-FileHash -LiteralPath $after).Hash
            Check (-not $same) 'the program picture moved' "before $before, after $after"
        }
        Ask 'home and presets' 'Press Home on the controller, then store a preset and recall it after moving away. Did the controller report the position, go home, and recall the preset? (The script reads the aim next.)'
        try { $aim = (Invoke-Op $d 'scene' @{ type = 'scene.camera_aim'; scene_id = 'main' }).result.result; Say "Aim now: pan $($aim.pan_deg), tilt $($aim.tilt_deg), zoom $($aim.zoom)" } catch {}
    } finally { [void](Stop-Daemon $d) }
}

# The alpha.5 checklist's manual items: the item, its issues, what to do and the question.
# Their steps and pass lines in full are in the runbook (docs/runbooks/windows-hardware-pass.md#alpha5).
$ManualItems = @(
    @{ Id = 'M1'; Issues = '#1221 #1217'; Title = 'Thread model and a seamless preview'
       Do = 'In jstream.exe, open a busy scene (browser page, display capture, webcam, animated text) and turn on studio mode. Switch preview and program 10 times, with Fade and with Cut.'
       Q = 'Did the preview and program never show a placeholder, a blank or a stall, and look as sharp as the source?' }
    @{ Id = 'M2'; Issues = '#1138 #1139'; Title = 'Animated sources'
       Do = 'Make a scene of about 20 small animated items (timers, text with motion) and one small still image. Watch the header''s fps and CPU for a minute.'
       Q = 'Did the fps hold at the base rate, with every item moving smoothly?' }
    @{ Id = 'M3'; Issues = '#1208'; Title = 'Pop-out projectors'
       Do = 'View > Projectors > Program in a window, drag it to your second monitor and double-click it. Then a scene row''s Pop out, and an item''s Pop out (its source alone). Escape closes each.'
       Q = 'Did each window open with a live picture (never black), full screen on the other monitor, at full frame rate?' }
    @{ Id = 'M4'; Issues = '#1207 #1210 #1212 #1192 #1230'; Title = 'Settings dialog'
       Do = 'File > Settings: visit General, Plugins, Audio plugins (press Rescan) and Devices. Help > Check for updates. File > Video settings: 1280x720 at 60 fps, Apply, then back to 1920x1080 at 30.'
       Q = 'Did every page open, the scan list your CLAP plugins, the update check say you are up to date, and the canvas change size and rate?' }
    @{ Id = 'M5'; Issues = '#1218'; Title = 'Save and load projects'
       Do = 'File > New project, add a scene with a text source, File > Save As to Documents\alpha5-test.json. File > New project again, then File > Open recent > alpha5-test. Quit jstream.exe and start it again.'
       Q = 'Did the scene come back on open, and is alpha5-test still in Open recent after the restart?' }
    @{ Id = 'M6'; Issues = '#1219 #1198 #1202 #1204 #1206'; Title = 'UI scrub'
       Do = 'Select an item: use the Inspector''s tabs and its Properties & filters button. Drag that dialog by its header. Rotate an item. Use a Browse button on a file field. Look at the Outputs panel.'
       Q = 'Are Properties and Filters one dialog, dialogs draggable, the item name turned with its box, Browse working, and the Outputs panel a compact list with settings in dialogs?' }
    @{ Id = 'M7'; Issues = '#1184 #1194 #1215'; Title = 'Snapshots on Windows'
       Do = 'In a scene with a browser source and a display capture, use the scene row''s Screenshot (scene). Open the PNG.'
       Q = 'Does the PNG show the browser page and the screen capture (not black boxes)?' }
    @{ Id = 'M8'; Issues = '#1225 #1216 #1214 #1188 #1346'; Title = 'Follow pointer, 3D camera, hotkeys'
       Do = 'Add the Follow pointer filter to a display capture item and move the mouse to each edge. Edit the scene''s 3D camera in Scene properties. Bind a hotkey to a scene and Ctrl+WheelUp to zoom, then click into Notepad and use both.'
       Q = 'Did the pointer stay in view with a margin, the 3D camera section work, and the hotkey and the wheel chord work while Notepad had the focus?' }
    @{ Id = 'M9'; Issues = '#1209 #1189'; Title = 'Small fixes'
       Do = 'Switch scenes with the Cut preset. Make a new project and look at File > Video settings.'
       Q = 'Is Cut an instant cut, and is a new project 30 fps with 60 as an option?' }
    @{ Id = 'M10'; Issues = '#1226 #1227 #1232 #1295'; Title = 'Filters'
       Do = 'On an opaque image: Glow, then Bloom, then Vignette. Then a Border, its 9-slice image frame, and a pulse from audio. Add a Custom shader filter, edit it in its editor and Save as pack.'
       Q = 'Did Glow show past the box, Bloom and Vignette tune easily, the borders draw, and the shader edit, save and reload?' }
    @{ Id = 'M11'; Issues = '#1228 #1060'; Title = 'Page API audio'
       Do = 'Add a browser source with an audio visualiser page (one that reads jstream.levels), tick its audio levels checkbox, and play sound or speak into the mic. Then untick it.'
       Q = 'Did the page move with the sound, show the badge while reading, and stop getting levels when unticked?' }
    @{ Id = 'M12'; Issues = '#1195 #1196'; Title = 'Webcam Properties'
       Do = 'Webcam source > Properties: pick the device, a resolution, a frame rate and a format, then move a camera control (brightness, white balance).'
       Q = 'Did each choice take effect in the picture, and the control change it live?' }
    @{ Id = 'M13'; Issues = '#1229'; Title = 'Ask before a new agent connects'; Fn = 'Test-AskAgent' }
    @{ Id = 'M14'; Issues = '#1222 #1223 #1224'; Title = 'MIDI and network audio'
       Do = 'With a MIDI keyboard: add a MIDI piano roll and a MIDI keyboard source and play. Bind a note to a scene switch. Send a MIDI clock from your DAW and set a motion''s speed in beats. With the JTunnel plugin in your DAW: Settings > Devices > Receive from JTunnel, then add a Network audio source.'
       Q = 'Did the MIDI sources draw your playing, the note switch scenes, motion follow the DAW tempo, and the network audio source''s meter move with the DAW?' }
    @{ Id = 'M15'; Issues = '#1182'; Title = 'PTZ cameras over VISCA'
       Do = 'If you have a VISCA-over-IP camera: add it (Settings > Devices, or the daemon''s --ptz-camera), drive it, store two presets, and put a preset recall in two keyframes.'
       Q = 'Did the camera move, store and recall presets, and follow the keyframes?' }
    @{ Id = 'M16'; Issues = '#1181'; Title = 'A real PTZ controller drives a scene camera'; Fn = 'Test-PtzController' }
)

function Invoke-Manual {
    Section 'Manual checklist'
    Say 'Each item says what to do in jstream.exe, from the release folder. The full steps and pass lines are in docs/runbooks/windows-hardware-pass.md (the alpha.5 checklist).'
    Say "Release folder: $script:Release"
    foreach ($m in $ManualItems) {
        $script:Item = $m.Id
        Section "$($m.Id): $($m.Title) ($($m.Issues))"
        try {
            if ($m.ContainsKey('Fn')) { & $m.Fn; continue }
            if (-not (Prompt-Do $m.Do)) { Skip $m.Title 'skipped by you'; continue }
            Ask $m.Title $m.Q
        } catch { Fail 'script' $_.Exception.Message }
    }
}

# --- Run -------------------------------------------------------------------------------------

$started = Get-Date
$os = Get-CimInstance Win32_OperatingSystem
$Machine = [ordered]@{
    os = "$($os.Caption) $($os.Version) build $($os.BuildNumber)"
    gpu = (@(Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) (driver $($_.DriverVersion))" }) -join '; ')
    cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    cores = (Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfLogicalProcessors -Sum).Sum
    powershell = "$($PSVersionTable.PSEdition) $($PSVersionTable.PSVersion)"
    expect = $Expect
}
Section "JStream release pass: $Expect"
Say "Run folder: $OutDir"
Say "Machine: $($Machine.os); $($Machine.gpu)"

$interrupted = $true
try {
    $script:Item = 'A'
    $script:Release = Get-Release
    if ($script:Release) {
        $script:Daemon = Join-Path $script:Release 'jstream-daemon.exe'
        $script:Cli = Join-Path $script:Release 'jstream-cli.exe'
        $Machine.release = $script:Release
        if (-not $ManualOnly) { Invoke-Automated }
        if (-not $AutomatedOnly) { Invoke-Manual }
    }
    $interrupted = $false
} finally {
    $script:Item = '(run)'
    if ($interrupted) { Warn 'run' 'stopped before every check ran' }

    $md = New-Object System.Text.StringBuilder
    [void]$md.AppendLine("# JStream release pass: $Expect")
    [void]$md.AppendLine('')
    foreach ($k in $Machine.Keys) { [void]$md.AppendLine("- **$k**: $($Machine[$k])") }
    [void]$md.AppendLine("- **took**: $([Math]::Round(((Get-Date) - $started).TotalMinutes)) min")
    $groups = @($Results | ForEach-Object { $_.Item } | Select-Object -Unique)
    foreach ($g in $groups) {
        $title = 'Automatic checks'
        if ($g -eq '(run)') { $title = 'The run' }
        $m = @($ManualItems | Where-Object { $_.Id -eq $g })
        if ($m) { $title = "$($m[0].Id): $($m[0].Title) ($($m[0].Issues))" }
        [void]$md.AppendLine('')
        [void]$md.AppendLine("## $title")
        [void]$md.AppendLine('')
        [void]$md.AppendLine('| result | check | detail |')
        [void]$md.AppendLine('|---|---|---|')
        foreach ($r in @($Results | Where-Object { $_.Item -eq $g })) {
            [void]$md.AppendLine("| $($r.Status) | $($r.Check) | $(($r.Detail -replace '\|', '/') -replace '[\r\n]+', ' ') |")
        }
    }
    $report = Join-Path $OutDir 'report.md'
    Write-Text $report $md.ToString()
    Write-Text (Join-Path $OutDir 'results.json') (@{ machine = $Machine; results = $Results } | ConvertTo-Json -Depth 6)

    try { Stop-Transcript | Out-Null } catch {}

    # The zip: text files and the snapshots, with the run's tokens redacted. Token files
    # and recordings stay out.
    $stage = Join-Path $OutDir '.zip-stage'
    Remove-Item -Recurse -Force -LiteralPath $stage -ErrorAction SilentlyContinue
    $textExt = @('.md', '.json', '.log', '.txt', '.jsonl')
    foreach ($f in @(Get-ChildItem -LiteralPath $OutDir -Recurse -File | Where-Object {
            ($textExt -contains $_.Extension -or $_.Extension -eq '.png') -and $_.FullName -notlike "$stage*" -and $_.Name -notmatch 'token' -and $_.FullName -notmatch '\\appdata\\' })) {
        $rel = $f.FullName.Substring($OutDir.Length).TrimStart('\')
        $dest = Join-Path $stage $rel
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
        if ($f.Extension -eq '.png') { Copy-Item -LiteralPath $f.FullName -Destination $dest; continue }
        $text = [IO.File]::ReadAllText($f.FullName)
        foreach ($s in $script:Secrets) { if ($s) { $text = $text.Replace($s, '***') } }
        Write-Text $dest $text
    }
    $zipOut = "$OutDir.zip"
    Remove-Item -Force -LiteralPath $zipOut -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $stage) { Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zipOut }
    Remove-Item -Recurse -Force -LiteralPath $stage -ErrorAction SilentlyContinue

    $count = { param($s) @($Results | Where-Object { $_.Status -eq $s }).Count }
    $fails = & $count 'FAIL'
    Write-Host ''
    Write-Host '================ Send this back ================' -ForegroundColor Cyan
    Write-Host "Attach this zip in the JStream project chat: $zipOut"
    Write-Host 'and paste these lines with it:'
    Write-Host ''
    Write-Host ("release-pass {0}: {1} pass, {2} fail, {3} warn, {4} skip; report {5}" -f $Expect, (& $count 'PASS'), $fails, (& $count 'WARN'), (& $count 'SKIP'), $report)
    foreach ($g in $groups) {
        $rs = @($Results | Where-Object { $_.Item -eq $g })
        $bad = @($rs | Where-Object { $_.Status -in @('FAIL', 'WARN') } | ForEach-Object { "$($_.Status) $($_.Check)" })
        $state = 'PASS'
        if (@($rs | Where-Object { $_.Status -eq 'FAIL' })) { $state = 'FAIL' }
        elseif (-not @($rs | Where-Object { $_.Status -eq 'PASS' })) { $state = 'SKIP' }
        Write-Host ("  {0,-6} {1,-4} {2}" -f $g, $state, ($bad -join '; '))
    }
    Write-Host '================================================' -ForegroundColor Cyan
}
if ($fails) { exit 1 }
exit 0
