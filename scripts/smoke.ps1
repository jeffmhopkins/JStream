<#
.SYNOPSIS
Go-live smoke test: drives a local jstream-daemon through creating an RTMP output, setting
its key, going live, sampling stats.tick, taking a snapshot and stopping.

.DESCRIPTION
The steps and what each one checks are in docs/runbooks/go-live.md (Smoke script). By
default it streams to a local RTMP sink (an ffmpeg listen process) with a throwaway key,
so it needs no real stream key. On Windows the daemon really encodes and publishes, and
the script checks it. On Linux the daemon only steps the placeholder output, so the key,
stats and sink checks are skipped. On macOS the daemon goes live, but the script doesn't
drive that yet (its key would go to the login keychain), so it refuses to run there with
exit 2: going live on a Mac is checked by hand (#644).

The script never reads, prints or logs a stream key: -KeyFile is passed to jstream-cli as
a path, and the local sink's key is a fixed, non-secret placeholder.

Runs on Windows PowerShell 5.1 and PowerShell 7 (pwsh).

.PARAMETER Release
Use the release build (target/release) instead of the debug build.

.PARAMETER NoBuild
Skip `cargo build` and use the binaries already built.

.PARAMETER Seconds
How long to stay live. Default 20.

.PARAMETER SinkPort
The local RTMP sink's port on 127.0.0.1. Default 1935.

.PARAMETER RtmpUrl
Stream to this server URL instead of the local sink (Windows only; needs -KeyFile).

.PARAMETER KeyFile
The file holding the stream key for -RtmpUrl. Only its path is used.

.PARAMETER OutDir
Where the run's files go (token, logs, snapshot, sink recording). Default: a new directory
under the system temp directory. It is kept after the run.
#>
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$NoBuild,
    [ValidateRange(5, 3600)][int]$Seconds = 20,
    [ValidateRange(1, 65535)][int]$SinkPort = 1935,
    [string]$RtmpUrl,
    [string]$KeyFile,
    [string]$OutDir
)

Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

$OnWindows = ($PSVersionTable.PSEdition -eq 'Desktop') -or $IsWindows
$Exe = ''
if ($OnWindows) { $Exe = '.exe' }
# PowerShell 5.1 has no $IsMacOS; it runs only on Windows.
if (-not $OnWindows -and $IsMacOS) {
    Write-Error 'macOS: going live is checked by hand, #644'; exit 2
}
$Repo = Split-Path -Parent $PSScriptRoot
# The output the run creates. Its key lives in Credential Manager as
# JStream/stream-key/smoke_test while the run lasts, and is cleared at the end.
$OutputId = 'smoke_test'
# The local sink's stream key: a placeholder, not a secret.
$SinkKey = 'smoke'

if ($RtmpUrl -and -not $KeyFile) { Write-Error '-RtmpUrl needs -KeyFile'; exit 2 }
if ($KeyFile -and -not $RtmpUrl) { Write-Error '-KeyFile needs -RtmpUrl'; exit 2 }
if ($RtmpUrl -and -not $OnWindows) {
    Write-Error '-RtmpUrl needs Windows: on Linux the daemon does not publish yet'; exit 2
}
if ($KeyFile) {
    if (-not (Test-Path -LiteralPath $KeyFile -PathType Leaf)) {
        Write-Error "-KeyFile: no such file: $KeyFile"; exit 2
    }
    $KeyFile = (Resolve-Path -LiteralPath $KeyFile).Path
}

# --- Results ---------------------------------------------------------------------------

$Results = New-Object System.Collections.Generic.List[object]
function Add-Result([string]$Status, [string]$Step, [string]$Detail) {
    $Results.Add([pscustomobject]@{ Status = $Status; Step = $Step; Detail = $Detail })
    $color = @{ PASS = 'Green'; FAIL = 'Red'; WARN = 'Yellow'; SKIP = 'DarkGray' }[$Status]
    Write-Host ('{0,-4} {1}: {2}' -f $Status, $Step, $Detail) -ForegroundColor $color
}
# A failed step that the later ones depend on: recorded, then the run goes to cleanup.
$StepFailed = 'jstream-smoke: step failed'
function Fail([string]$Step, [string]$Detail) {
    Add-Result 'FAIL' $Step $Detail
    throw $StepFailed
}

# --- Native processes ------------------------------------------------------------------

# Quotes one argument the way the C runtime (and .NET on every OS) splits a command line.
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

function Start-Native([string]$File, [string[]]$Arguments) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $File
    $psi.Arguments = ($Arguments | ForEach-Object { Format-Arg $_ }) -join ' '
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    [System.Diagnostics.Process]::Start($psi)
}

# Runs a process to completion and returns its exit code, stdout and stderr.
function Invoke-Native([string]$File, [string[]]$Arguments, [int]$TimeoutSec = 60) {
    $p = Start-Native $File $Arguments
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($TimeoutSec * 1000)) {
        $p.Kill()
        throw "$(Split-Path -Leaf $File) did not finish within $TimeoutSec s"
    }
    $p.WaitForExit()
    [pscustomobject]@{ Code = $p.ExitCode; Out = $out.Result; Err = $err.Result.Trim() }
}

function Stop-Native($p) {
    if ($p -and -not $p.HasExited) { try { $p.Kill() } catch {} ; [void]$p.WaitForExit(10000) }
}

# --- The daemon's two interfaces -------------------------------------------------------

# One jstream-cli call with --json. Returns the parsed reply, or $null with $LastCli set.
$script:LastCli = $null
function Invoke-Cli([string[]]$Arguments) {
    $r = Invoke-Native $Cli (@('--json', '--url', $WsUrl, '--token-file', $TokenFile) + $Arguments)
    $script:LastCli = $r
    if ($r.Code -ne 0) { return $null }
    $r.Out.Trim() | ConvertFrom-Json
}
function Get-CliError {
    $text = $script:LastCli.Err
    try { $e = ($text | ConvertFrom-Json).error; return "$($e.message) ($($e.code), exit $($script:LastCli.Code))" }
    catch { return "$text (exit $($script:LastCli.Code))" }
}

$script:McpSession = $null
$script:McpId = 0
# HttpClient rather than Invoke-WebRequest, which differs between PowerShell 5.1 and 7 in
# how it takes headers. No proxy: the daemon is on 127.0.0.1.
Add-Type -AssemblyName System.Net.Http
$handler = New-Object System.Net.Http.HttpClientHandler
$handler.UseProxy = $false
$Http = New-Object System.Net.Http.HttpClient($handler)
$Http.Timeout = [TimeSpan]::FromSeconds(30)

function Send-Mcp([System.Net.Http.HttpMethod]$HttpMethod, [string]$Body) {
    $msg = New-Object System.Net.Http.HttpRequestMessage($HttpMethod, $McpUrl)
    [void]$msg.Headers.TryAddWithoutValidation('Authorization', "Bearer $Token")
    [void]$msg.Headers.TryAddWithoutValidation('Accept', 'application/json, text/event-stream')
    if ($script:McpSession) {
        [void]$msg.Headers.TryAddWithoutValidation('Mcp-Session-Id', $script:McpSession)
        [void]$msg.Headers.TryAddWithoutValidation('MCP-Protocol-Version', '2025-11-25')
    }
    if ($Body) {
        $msg.Content = New-Object System.Net.Http.StringContent($Body, [System.Text.Encoding]::UTF8, 'application/json')
    }
    $Http.SendAsync($msg).GetAwaiter().GetResult()
}

# One MCP JSON-RPC request over the daemon's streamable-HTTP endpoint.
function Invoke-Mcp([string]$Method, $Params) {
    $script:McpId++
    $body = @{ jsonrpc = '2.0'; id = $script:McpId; method = $Method; params = $Params } |
        ConvertTo-Json -Depth 20 -Compress
    $resp = Send-Mcp ([System.Net.Http.HttpMethod]::Post) $body
    $content = $resp.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    if (-not $resp.IsSuccessStatusCode) { throw "MCP $Method answered HTTP $([int]$resp.StatusCode): $content" }
    $values = $null
    if ($resp.Headers.TryGetValues('Mcp-Session-Id', [ref]$values)) { $script:McpSession = @($values)[0] }
    # A server may answer as an event stream: the JSON is on its `data:` line.
    if ($content -match '(?m)^data:\s*(\{.*\})\s*$') { $content = $Matches[1] }
    $content | ConvertFrom-Json
}

function Get-OutputState {
    $r = Invoke-Cli @('output', 'status', $OutputId)
    if (-not $r) { return $null }
    $r.result.result
}

# Polls output.status until it is $Want. A `failed` state ends the wait at once.
function Wait-OutputState([string]$Want, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = '(none)'
    do {
        $s = Get-OutputState
        if ($s) {
            $last = $s.state
            if ($last -eq $Want) { return $true }
            if ($last -eq 'failed') { break }
        }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    $script:LastState = $last
    $false
}

# --- Run -------------------------------------------------------------------------------

$BuildProfile = 'debug'
if ($Release) { $BuildProfile = 'release' }
$Daemon = Join-Path $Repo "target/$BuildProfile/jstream-daemon$Exe"
$Cli = Join-Path $Repo "target/$BuildProfile/jstream-cli$Exe"

if (-not $OutDir) {
    $OutDir = Join-Path ([System.IO.Path]::GetTempPath()) ('jstream-smoke-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path
$TokenFile = Join-Path $OutDir 'token'
$LogDir = Join-Path $OutDir 'logs'
$SnapshotFile = Join-Path $OutDir 'snapshot.png'
$SinkFile = Join-Path $OutDir 'sink.flv'
$SinkKeyFile = Join-Path $OutDir 'sink-key'
$UseSink = $OnWindows -and -not $RtmpUrl

$daemonProc = $null; $eventsProc = $null; $sinkProc = $null
$daemonOut = $null; $daemonErr = $null; $eventsOut = $null; $sinkErr = $null
$script:LastState = '(none)'
$keySet = $false; $McpUrl = $null; $WsUrl = $null; $Token = $null

Write-Host "jstream smoke test: $BuildProfile build, live for $Seconds s, files in $OutDir"

try {
    # 1. Build.
    if ($NoBuild) {
        Add-Result 'SKIP' 'build' '-NoBuild'
    } else {
        $buildArgs = @('build', '-p', 'jstream-daemon', '-p', 'jstream-cli')
        if ($Release) { $buildArgs += '--release' }
        Push-Location $Repo
        try { & cargo @buildArgs } finally { Pop-Location }
        if ($LASTEXITCODE -ne 0) { Fail 'build' "cargo build exited $LASTEXITCODE" }
        Add-Result 'PASS' 'build' "cargo $($buildArgs -join ' ')"
    }
    foreach ($bin in @($Daemon, $Cli)) {
        if (-not (Test-Path -LiteralPath $bin)) { Fail 'build' "missing $bin" }
    }
    $v = Invoke-Native $Daemon @('--version')
    Add-Result 'PASS' 'version' (($v.Out -split "`n" | Where-Object { $_ }) -join '; ')

    # 2. The local RTMP sink, before anything publishes to it.
    if ($UseSink) {
        $ffmpeg = Get-Command ffmpeg -ErrorAction SilentlyContinue
        if (-not $ffmpeg) { Fail 'sink' 'ffmpeg is not on PATH (needed for the local RTMP sink; or pass -RtmpUrl and -KeyFile)' }
        Set-Content -LiteralPath $SinkKeyFile -Value $SinkKey -NoNewline
        $sinkProc = Start-Native $ffmpeg.Source @(
            '-hide_banner', '-nostdin', '-nostats', '-loglevel', 'warning', '-listen', '1',
            '-i', "rtmp://127.0.0.1:$SinkPort/live/$SinkKey", '-c', 'copy', '-f', 'flv', $SinkFile)
        $sinkOut = $sinkProc.StandardOutput.ReadToEndAsync()
        $sinkErr = $sinkProc.StandardError.ReadToEndAsync()
        Start-Sleep -Seconds 1
        if ($sinkProc.HasExited) { Fail 'sink' "ffmpeg exited $($sinkProc.ExitCode): $($sinkErr.Result.Trim())" }
        Add-Result 'PASS' 'sink' "ffmpeg listening on rtmp://127.0.0.1:$SinkPort/live"
    } elseif ($RtmpUrl) {
        Add-Result 'SKIP' 'sink' 'streaming to -RtmpUrl instead'
    } else {
        Add-Result 'SKIP' 'sink' 'not Windows: the daemon does not publish here'
    }

    # 3. Start the daemon and wait for its `listening on` line.
    $daemonProc = Start-Native $Daemon @('--token-file', $TokenFile, '--log-dir', $LogDir)
    $daemonErr = $daemonProc.StandardError.ReadToEndAsync()
    $lines = @()
    $deadline = (Get-Date).AddSeconds(60)
    while (-not $WsUrl -and (Get-Date) -lt $deadline) {
        $read = $daemonProc.StandardOutput.ReadLineAsync()
        $left = [Math]::Max(0, [int](($deadline - (Get-Date)).TotalMilliseconds))
        if (-not $read.Wait($left)) { break }
        $line = $read.Result
        if ($null -eq $line) { break }
        $lines += $line
        if ($line -match '^mcp endpoint: (\S+)') { $McpUrl = $Matches[1] }
        if ($line -match '^listening on (ws://\S+)') { $WsUrl = $Matches[1] }
    }
    if (-not $WsUrl) {
        Stop-Native $daemonProc
        $code = ''
        if ($daemonProc.HasExited) { $code = " (exit $($daemonProc.ExitCode))" }
        $why = ($daemonErr.Result -split "`n" | Where-Object { $_ -match '^error:' }) -join ' '
        if ($daemonProc.HasExited -and $daemonProc.ExitCode -eq 3) {
            $why = "Media Foundation is missing: $why"
        }
        Fail 'daemon' "no listening line$code. $why"
    }
    $daemonOut = $daemonProc.StandardOutput.ReadToEndAsync()
    $Token = (Get-Content -LiteralPath $TokenFile -Raw).Trim()
    Add-Result 'PASS' 'daemon' "listening on $WsUrl, MCP at $McpUrl"

    # 4. MCP: initialize, then output.create through the `output` tool.
    $init = Invoke-Mcp 'initialize' @{
        protocolVersion = '2025-11-25'; capabilities = @{}
        clientInfo = @{ name = 'jstream-smoke'; version = '1' }
    }
    if (-not $init.PSObject.Properties['result'] -or -not $script:McpSession) {
        Fail 'mcp initialize' "no session: $($init | ConvertTo-Json -Compress -Depth 5)"
    }
    Add-Result 'PASS' 'mcp initialize' "session open, server $($init.result.serverInfo.name)"
    $url = "rtmp://127.0.0.1:$SinkPort/live"
    if ($RtmpUrl) { $url = $RtmpUrl }
    $create = Invoke-Mcp 'tools/call' @{
        name = 'output'
        arguments = @{ op = @{
            type = 'output.create'; output_id = $OutputId
            kind = @{ type = 'rtmp'; target = @{ type = 'generic'; url = $url } }
        } }
    }
    if (-not $create.PSObject.Properties['result'] -or $create.result.isError) {
        Fail 'output.create (MCP)' ($create | ConvertTo-Json -Compress -Depth 10)
    }
    Add-Result 'PASS' 'output.create (MCP)' "$OutputId at rev $($create.result.structuredContent.result.rev)"

    # 5. The stream key, through the CLI so it never passes through an agent.
    if ($OnWindows) {
        $kf = $SinkKeyFile
        if ($KeyFile) { $kf = $KeyFile }
        $r = Invoke-Cli @('output', 'set-key', $OutputId, '--key-file', $kf)
        if (-not $r) { Fail 'output.set_key' (Get-CliError) }
        $keySet = $true
        $s = Get-OutputState
        if (-not $s -or -not $s.key_set) { Fail 'output.set_key' 'output.status says no key is set' }
        Add-Result 'PASS' 'output.set_key' 'stored in Credential Manager; status says key set'
    } else {
        Add-Result 'SKIP' 'output.set_key' 'Linux: this daemon has no key store'
    }

    # 6. Subscribe to events before going live, so the first stats.tick is caught.
    $eventsProc = Start-Native $Cli @('--json', '--url', $WsUrl, '--token-file', $TokenFile, 'events', '--count', '1000000')
    $eventsOut = $eventsProc.StandardOutput.ReadToEndAsync()
    $eventsErr = $eventsProc.StandardError.ReadToEndAsync()
    Start-Sleep -Milliseconds 500

    # 7. Go live.
    $r = Invoke-Cli @('output', 'start', $OutputId)
    if (-not $r) { Fail 'output.start' (Get-CliError) }
    if (-not (Wait-OutputState 'active' 30)) {
        Fail 'output.start' "never became active (last state: $script:LastState); see $LogDir"
    }
    Add-Result 'PASS' 'output.start' 'active'

    # 8. Stay live, watching the state each second.
    $states = @{}
    $end = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $end) {
        $s = Get-OutputState
        if ($s) { $states[$s.state] = 1 + [int]$states[$s.state] }
        Start-Sleep -Seconds 1
    }
    $seen = ($states.Keys | Sort-Object | ForEach-Object { "$_ x$($states[$_])" }) -join ', '
    if ($states.ContainsKey('failed')) { Fail 'live' "the output failed while live ($seen)" }
    if ($states.Keys | Where-Object { $_ -ne 'active' }) { Add-Result 'WARN' 'live' "not active throughout: $seen" }
    else { Add-Result 'PASS' 'live' "active for $Seconds s ($seen)" }

    # 9. Snapshot of the program mix.
    $r = Invoke-Cli @('video', 'snapshot')
    if (-not $r) { Fail 'video.snapshot' (Get-CliError) }
    $snap = $r.result.result
    $bytes = [Convert]::FromBase64String($snap.png)
    [System.IO.File]::WriteAllBytes($SnapshotFile, $bytes)
    $sig = [byte[]](0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A)
    if ($bytes.Length -lt 8 -or (Compare-Object $sig $bytes[0..7])) { Fail 'video.snapshot' 'not a PNG' }
    Add-Result 'PASS' 'video.snapshot' "$($snap.width)x$($snap.height), $($bytes.Length) bytes, saved to $SnapshotFile"

    # 10. Stop.
    $r = Invoke-Cli @('output', 'stop', $OutputId)
    if (-not $r) { Fail 'output.stop' (Get-CliError) }
    if (-not (Wait-OutputState 'stopped' 30)) { Fail 'output.stop' "never stopped (last state: $script:LastState)" }
    Add-Result 'PASS' 'output.stop' 'stopped'
    # The pipeline emits one last stats.tick after it stops.
    Start-Sleep -Seconds 2
} catch {
    # A failed step is recorded already; anything else is a fault in the run itself.
    if ($_.Exception.Message -ne $StepFailed) { Add-Result 'FAIL' 'script' $_.Exception.Message }
} finally {
    # Events: stats.tick and state changes seen while live.
    if ($eventsProc) {
        Stop-Native $eventsProc
        $events = @($eventsOut.Result -split "`n" | Where-Object { $_.Trim() } | ForEach-Object {
            try { $_ | ConvertFrom-Json } catch { $null } } | Where-Object { $_ })
        $ticks = @($events | Where-Object { $_.type -eq 'stats.tick' })
        $outStates = ($events | Where-Object { $_.type -eq 'output.state_changed' -and $_.output_id -eq $OutputId } |
            ForEach-Object { $_.state }) -join ' > '
        $dropped = @($events | Where-Object { $_.type -eq 'events.dropped' })
        if ($outStates) { Add-Result 'PASS' 'events' "states: $outStates" }
        else { Add-Result 'FAIL' 'events' 'no output.state_changed event' }
        if ($dropped) { Add-Result 'WARN' 'events' "the server dropped events $($dropped.Count) time(s)" }
        if ($OnWindows) {
            if ($ticks.Count -eq 0) {
                Add-Result 'FAIL' 'stats.tick' 'none while live'
            } else {
                $last = $ticks[-1].drops
                $drops = ($last.PSObject.Properties | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join ' '
                $nonzero = @($last.PSObject.Properties | Where-Object { [long]$_.Value -ne 0 })
                $msg = "$($ticks.Count) ticks (seq $($ticks[0].seq)..$($ticks[-1].seq)); drops: $drops"
                if ($ticks.Count -lt [Math]::Max(1, $Seconds - 5)) { Add-Result 'WARN' 'stats.tick' "fewer than expected: $msg" }
                elseif ($nonzero) { Add-Result 'WARN' 'stats.tick' $msg }
                else { Add-Result 'PASS' 'stats.tick' $msg }
            }
        } else {
            Add-Result 'SKIP' 'stats.tick' 'not Windows: no pipeline runs here'
        }
    }

    # The test key must not outlive the run.
    if ($keySet) {
        $r = Invoke-Cli @('output', 'clear-key', $OutputId)
        if ($r) { Add-Result 'PASS' 'output.clear_key' "JStream/stream-key/$OutputId removed" }
        else { Add-Result 'FAIL' 'output.clear_key' "$(Get-CliError); remove JStream/stream-key/$OutputId in Credential Manager" }
    }

    # The sink saw the stream end when the output stopped; give it a moment to close.
    if ($sinkProc) {
        if (-not $sinkProc.WaitForExit(10000)) { Stop-Native $sinkProc }
        if (Test-Path -LiteralPath $SinkFile) {
            $ffprobe = Get-Command ffprobe -ErrorAction SilentlyContinue
            if ($ffprobe) {
                $p = Invoke-Native $ffprobe.Source @('-v', 'error', '-show_entries',
                    'stream=codec_type,codec_name:format=duration', '-of', 'json', $SinkFile)
                $info = $p.Out | ConvertFrom-Json
                $codecs = ($info.streams | ForEach-Object { "$($_.codec_type):$($_.codec_name)" }) -join ' '
                $dur = [double]$info.format.duration
                $msg = "$codecs, $([Math]::Round($dur, 1)) s in $SinkFile"
                if ($codecs -notmatch 'video:h264' -or $codecs -notmatch 'audio:aac') { Add-Result 'FAIL' 'sink' "expected H.264 and AAC: $msg" }
                elseif ($dur -lt $Seconds / 2) { Add-Result 'FAIL' 'sink' "too short: $msg" }
                else { Add-Result 'PASS' 'sink' "received $msg" }
            } else {
                $len = (Get-Item -LiteralPath $SinkFile).Length
                if ($len -gt 0) { Add-Result 'PASS' 'sink' "received $len bytes in $SinkFile (no ffprobe to check codecs)" }
                else { Add-Result 'FAIL' 'sink' 'received nothing' }
            }
        } elseif ($Results | Where-Object { $_.Step -eq 'output.start' -and $_.Status -eq 'PASS' }) {
            Add-Result 'FAIL' 'sink' "received nothing: $($sinkErr.Result.Trim())"
        }
    }

    if ($script:McpSession) {
        try { [void](Send-Mcp ([System.Net.Http.HttpMethod]::Delete) $null) } catch {}
    }

    # Stop the daemon: SIGINT where there is one (a graceful stop, checked); on Windows a
    # process outside the console can't be sent Ctrl-C, so it is ended (runbook step 7).
    if ($daemonProc -and -not $daemonProc.HasExited) {
        if ($OnWindows) {
            Stop-Native $daemonProc
            Add-Result 'SKIP' 'daemon stop' 'ended (the graceful stop is checked by hand, runbook step 7)'
        } else {
            & kill -INT $daemonProc.Id
            if ($daemonProc.WaitForExit(15000) -and $daemonProc.ExitCode -eq 0 -and $daemonOut.Result -match '(?m)^stopped') {
                Add-Result 'PASS' 'daemon stop' 'SIGINT: printed stopped, exit 0'
            } else {
                Stop-Native $daemonProc
                Add-Result 'FAIL' 'daemon stop' "no graceful stop on SIGINT (exit $($daemonProc.ExitCode))"
            }
        }
    }
}

$fails = @($Results | Where-Object { $_.Status -eq 'FAIL' }).Count
$warns = @($Results | Where-Object { $_.Status -eq 'WARN' }).Count
Write-Host ''
Write-Host "Files: $OutDir (daemon logs in $LogDir)"
if ($fails) { Write-Host "SMOKE FAILED: $fails failed, $warns warnings" -ForegroundColor Red; exit 1 }
Write-Host "SMOKE PASSED ($warns warnings)" -ForegroundColor Green
exit 0
