<!-- @doc
id = "RUNBOOK-GO-LIVE"
title = "Go-live runbook"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Go-live runbook

How to take the headless daemon live on Windows by hand, and how to run the smoke script
that does the same unattended. What the daemon does at each step is in its
[Usage](../../apps/jstream-daemon/README.md#usage); this page is the order to do it in
and what to check.

<a id="before"></a>
## Before you start
<!-- @section
depends_on = ["apps/jstream-daemon/README.md#usage"]
-->

- **Windows with Media Foundation.** On an N or KN edition, install the Media Feature
  Pack first. Without it the daemon exits 3 and says what to install.
- **A build.** From the repository root:
  `cargo build --release -p jstream-daemon -p jstream-cli`. The binaries are in
  `target\release`.
- **A token file location.** Its folder must exist. The daemon creates the file with a
  new token on the first run and reuses it after that. Never paste the token into a
  chat, an issue or an agent prompt.
- **The stream key** from your streaming service, saved in a file only you can read, or
  ready to type. It never goes on a command line or through an agent
  ([R-SEC-001](../RULES.md#R-SEC-001)).

<a id="steps"></a>
## Going live by hand
<!-- @section
depends_on = ["apps/jstream-daemon/README.md#usage", "apps/jstream-cli/README.md#usage", "crates/jstream-server/README.md#design"]
-->

The commands are PowerShell, run from the repository root. `$cli` is short for the CLI
with its connection options:

```powershell
$env:JSTREAM_TOKEN_FILE = "$env:APPDATA\JStream\token"
$cli = 'target\release\jstream-cli.exe'
# then: & $cli --url ws://127.0.0.1:4460 <command>
```

1. **Start the daemon** in its own terminal:
   ```powershell
   New-Item -ItemType Directory -Force "$env:APPDATA\JStream" | Out-Null
   target\release\jstream-daemon.exe --port 4460
   ```
   Check: it prints `mcp endpoint: http://127.0.0.1:4460/mcp`, then
   `listening on ws://127.0.0.1:4460/api/v1/ws`. Exit code 3 means Media Foundation or
   its AAC encoder is missing. The first log line names the log file, under
   `%APPDATA%\JStream\logs`.
2. **Create the RTMP output over MCP.** Point the agent's MCP client at
   `http://127.0.0.1:4460/mcp` with the header `Authorization: Bearer <token>`, or have it
   launch the stdio proxy, `jstream-cli --url ws://127.0.0.1:4460 mcp`, with
   `JSTREAM_TOKEN_FILE` set. Ask it to call the `output` tool with
   `{"op": {"type": "output.create", "output_id": "live", "kind": {"type": "rtmp",
   "target": {"type": "generic", "url": "<the service's server URL>"}}}}`. The server URL
   is `rtmp://host[:port]/app` or `rtmps://…`, without the key. The CLI does the same:
   `& $cli --url ws://127.0.0.1:4460 edit output.create live <server URL>`.
   Check: `& $cli --url ws://127.0.0.1:4460 output list` shows `live` with that target.
3. **Set the stream key with the CLI**, not through the agent, so the key never enters
   the agent's context:
   `& $cli --url ws://127.0.0.1:4460 output set-key live --key-file <path>`
   (or leave out `--key-file`, type the key, and press Ctrl-Z then Enter).
   Check: `output status live` says `stream key set`. The key is stored in Windows
   Credential Manager as `JStream/stream-key/live`. If the line ends with
   `warning: plain rtmp://`, the key will cross the network unencrypted: use the
   service's `rtmps://` server URL if it has one.
4. **Watch the events** in a second terminal before going live:
   `& $cli --url ws://127.0.0.1:4460 events`. A `stats.tick` line comes about once a
   second, before and while the output runs. Its counters are running totals of dropped
   frames and packets since the daemon started, so all zeros is healthy. A counter that
   keeps climbing is worth an issue. The line ends with the engine's load:
   `program_frames` grows by about 30 a second while live, and `cpu_ms` is the CPU time
   JStream has used. While the program runs, `meters.tick` lines, the audio
   levels, come about ten a second as well.
5. **Go live:** the `output` tool with `{"op": {"type": "output.start", "output_id":
   "live"}}`, or `& $cli --url ws://127.0.0.1:4460 output start live`.
   Check: the events show `output.state_changed` going `starting`, then `active`, and
   the streaming service shows the stream arriving. A start without a key or target
   fails with `action_failed` and says which. `reconnecting` means the connection
   dropped and the output is retrying; `failed` means it stopped on an error (keep
   the daemon's log for the issue).
6. **Take a snapshot:** the `video` tool with `{"op": {"type": "video.snapshot"}}`
   returns the program mix as an image. The CLI's `video snapshot` prints only its size;
   `--json video snapshot` includes the PNG as base64.
7. **Stop:** `output.stop` through the tool, or `output stop live`.
   Check: the state goes `stopping`, then `stopped`, and the service sees the stream
   end. Then press Ctrl-C in the daemon's terminal. It prints `stopped` and exits 0.
8. **Clean up** if this was a test: `output clear-key live` removes the key from
   Credential Manager. The output itself is gone when the daemon stops, unless it
   was started with `--collection <file>`, which saves it for the next run.

<a id="smoke"></a>
## Smoke script
<!-- @section
covers = ["scripts/smoke.ps1"]
depends_on = ["apps/jstream-daemon/README.md#usage", "apps/jstream-cli/README.md#usage"]
-->

`scripts/smoke.ps1` runs the steps above against a daemon it starts itself. By default
it streams to a local RTMP sink, an `ffmpeg -listen` process on `127.0.0.1`, with a
placeholder key, so it needs no real stream key and sends nothing off the machine. It
runs on Windows PowerShell 5.1 and PowerShell 7:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\smoke.ps1 -Release
pwsh scripts/smoke.ps1                  # debug build, also on Linux
```

| Option | Meaning |
|---|---|
| `-Release` | Use `target/release` instead of `target/debug` |
| `-NoBuild` | Skip `cargo build -p jstream-daemon -p jstream-cli` |
| `-Seconds <n>` | How long to stay live. Default 20 |
| `-SinkPort <port>` | The local sink's port. Default 1935 |
| `-RtmpUrl <url> -KeyFile <path>` | Stream to a real server instead of the sink (Windows only). Only the key file's path is used |
| `-OutDir <dir>` | Where the run's files go. Default: a new `jstream-smoke-<time>` folder in the temp directory |

What it does, in order, with a `PASS`, `FAIL`, `WARN` or `SKIP` line for each step:
1. Builds the daemon and CLI, and prints their versions.
2. Starts the sink: `ffmpeg` (and `ffprobe` for the codec check) must be on `PATH`.
3. Starts the daemon with a fresh token file and log directory in the run's folder, and
   waits for its `listening on` line. Exit 3 is reported as missing Media Foundation.
4. Opens an MCP session at `/mcp` and creates the output `smoke_test` with the
   `output` tool.
5. Sets the key with `jstream-cli output set-key --key-file`, and checks that
   `output status` says it is set.
6. Subscribes to events with `jstream-cli events`, starts the output and waits for
   `active`, then polls `output status` every second while live. Any state other than
   `active` is a warning, and `failed` fails the run.
7. Takes a snapshot, checks that it is a PNG and saves it as `snapshot.png`.
8. Stops the output and waits for `stopped`.
9. Checks the events: the output's state changes, and at least one `stats.tick`.
   Fewer than one a second (allowing for start-up), or non-zero drop counters, are a
   warning.
10. Clears the key from Credential Manager, and checks that the sink received H.264 and
    AAC lasting at least half the live time.
11. Ends the daemon. On Linux it sends SIGINT and checks the graceful stop
    (`stopped`, exit 0). On Windows a process without a console can't be sent Ctrl-C,
    so it is ended, and the graceful stop is step 7 of the manual run.

It exits 0 when nothing failed, 1 when a step failed, and 2 for bad options or on
macOS. The run's folder keeps the daemon's logs (and any crash report), the snapshot
and the sink's recording. It holds the run's token file too, so don't attach the whole
folder to an issue.

On Linux the run skips the key, the sink and `stats.tick`, and checks the rest:
start-up, MCP, the output's states, the snapshot and the graceful stop. The daemon only
steps a placeholder output there, so that is what CI or a Linux machine can run. On
macOS the daemon goes live, but the script doesn't drive that yet, so it refuses to
run there with exit 2 ("macOS: going live is checked by hand, #644").

<a id="youtube"></a>
## YouTube
<!-- @section
-->

Going live to YouTube needs no sign-in and no Google OAuth client
([ADR-0058](../adr/0058-youtube-sign-in-with-a-jstream-client.md)): use a plain stream
key as above, with YouTube's RTMPS server URL from YouTube Studio, and turn on
auto-start (and auto-stop) for the stream in YouTube Studio, so the broadcast goes live
when data arrives and ends when it stops.

Signing in, to manage broadcasts through the API, is optional and Windows only. It
uses JStream's own OAuth client (ADR-0058 above), which this build doesn't have yet, so a
run for now uses a developer's Google Cloud OAuth client of type "Desktop app", with the
YouTube Data API v3 enabled. The `youtube.*` calls below are a person's: they go over
the WebSocket API, since MCP has no `youtube` tool and `jstream-cli` has no
YouTube commands yet. Then:

1. Start the daemon with `--youtube-client <the client's JSON>`, or with
   `JSTREAM_YOUTUBE_CLIENT_ID` (and `JSTREAM_YOUTUBE_CLIENT_SECRET`) set
   ([daemon › Usage](../../apps/jstream-daemon/README.md#usage)). Without either,
   `youtube.status` answers `unavailable` until the built-in client is filled in.
2. `youtube.status` answers `signed_out`. Call `youtube.sign_in`, open the consent URL
   it returns, and approve.
3. `youtube.status` answers `signed_in`. The refresh token is in Credential Manager, so
   a restart stays signed in.
4. Create a broadcast (`youtube.create_broadcast`) or pick one
   (`youtube.list_broadcasts`), and bind a stream to it (`youtube.bind_broadcast`).
   Create an output with the `youtube { broadcast_id }` target and start it.
   `youtube.health_changed` reports the stream's health.
5. Starting the output doesn't take the broadcast live: broadcasts are created without
   auto-start. Once the health is good, call `youtube.transition_broadcast` with `to:
   testing`, then `to: live`.
6. To end, call `youtube.transition_broadcast` with `to: complete`, then stop the
   output.
7. `youtube.sign_out` removes the token.

This path has not yet been run on hardware: issue #337 holds its run, step by step.

<a id="results"></a>
## Reporting results
<!-- @section
-->

Each problem a run finds becomes its own issue, with:
- the script's summary lines, or the manual step that failed and what you saw;
- the daemon's log file from the run's `logs` folder, and any `crash-*.txt` report
  there. Both are redacted of the token and stream key, but read them before attaching;
- the Windows edition and build, and the GPU.

Never attach the token file, a key file or a screenshot of Credential Manager.
