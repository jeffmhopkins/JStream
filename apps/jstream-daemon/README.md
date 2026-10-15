<!-- @doc
id = "CRATE-jstream-daemon"
title = "jstream-daemon"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-daemon

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["apps/jstream-daemon/src/**", "apps/jstream-daemon/tests/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*"]
verified = { hash = "b3:ba52f5f3dcaeb8c8", by = "agent:claude@session_01PZRAdnkMzURQaUtof2W6W2", note = "Round 1 B1: Tests bullet now says WS sets the key and starts the stream, MCP starts the recording (matches e2e.rs); rest of Purpose re-read, holds" }
-->

The headless engine host ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)): it runs a
`jstream-core` `Engine` and serves it through `jstream-server`'s authenticated WebSocket
API on `127.0.0.1`, so `jstream-cli --url …` and the stdio MCP proxy `jstream-cli mcp`
can drive it, and turns on the server's MCP transport over streamable HTTP at `/mcp`, so
an agent can connect without the proxy. Like every host it only wires the engine to its interfaces
([ADR-0001](../../docs/adr/0001-library-first-engine.md)): each connection and each MCP
session gets its own `InProcessControl` session through the server's `Connector`. Layer 6, an app that links
core and server ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut, the M1a host of [ROADMAP › M1](../../ROADMAP.md#m1)):
- **Startup** (`main.rs`): parses the command line, creates the log directory, installs
  the crash-report hook and the log subscriber (stderr and the log files, filtered by
  `--log-level` or `RUST_LOG`), opens the audit log, loads or creates the token, runs
  the tokio runtime,
  prints where it listens, and maps failures to exit codes.
- **Command line** (`args.rs`): a hand-rolled, pure parser ([Usage](#usage)).
- **Token file** (`token.rs`): reuses the token in an existing user-only file, or
  creates the file with a new random token ([Design](#design)).
- **Serving** (`serve.rs`): builds the engine (with the audit log, the `--collection`
  file and the placeholder outputs; on Windows a live one, [Design](#design)), installs
  the signal handlers that stop it, then binds the server with MCP on.
- **Media Foundation check** (`main.rs`, `components.rs`, Windows only): exits with
  advice when Windows lacks Media Foundation or its AAC encoder, as N editions do
  ([Design](#design)).
- **Virtual camera registration** (`register.rs`, Windows only): `--register-vcam`
  registers the media source DLL shipped beside the daemon, and startup logs a warning
  when the registration names another folder ([Design](#design)).
- **Build script** (`build.rs`): delay-loads the Media Foundation DLLs on Windows
  (MSVC), so that check can run at all.
- **Log files** (`logs.rs`): the data directory, and the size-capped rotating log files
  ([Design](#design)).
- **Crash reports** (`crash.rs`): the panic hook that writes a redacted report into the
  log directory ([Design](#design)).
- **Tests** (`serve.rs`, `token.rs`, `args.rs`, `logs.rs`, `crash.rs` unit tests, the
  crash hook in a child process; the publish opener and the live engine on the
  platform fakes, in `serve.rs`; `tests/daemon.rs`, the built binary driven by
  `jstream-control`'s WebSocket client and a raw HTTP MCP client, and stopped with
  SIGINT; `tests/e2e.rs`, the headless end to end on every OS: the engine as the
  daemon makes it live, on the platform fakes with the CPU compositor, behind the
  server with MCP on, where MCP builds a scene and creates an RTMP output to a local
  loopback sink and a recording, the WebSocket client sets the key and starts the
  stream, MCP starts the recording, and the test checks `stats.tick`, what the sink
  received and the recorded MP4. The binary has no backends off Windows, so it runs
  in process; `tests/delay_load.rs`,
  Windows MSVC only, which reads the built binary's PE headers to check that the
  backends' Media Foundation DLLs are delay-loaded and none is a load-time import).

Not here yet: YouTube sign-in (it waits on `Engine::with_youtube`), a default collection file, a
default port and a default token file
location ([Design › Open questions](#open-questions)).

Why a crate: it is a host binary of its own (ADR-0020 ships `jstream-daemon.exe` next
to the desktop app), and only an app may link both `jstream-core` and `jstream-server`
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-daemon/src/args.rs", "apps/jstream-daemon/src/main.rs", "apps/jstream-daemon/src/token.rs"]
verified = { hash = "b3:08383bca8d7cfb6c", by = "agent:claude@session_01WbL3WRbcjCtYjwjoicNdxV", note = "U2 --ui-dir: re-read against args.rs and main.rs: new option row, ui: banner line before listening on, Web UI bullet, exit 1 for a non-directory" }
-->

```sh
jstream-daemon --token-file ~/.config/jstream/token --port 4460 --output out_1
# token file: /home/me/.config/jstream/token (created, new token)
# mcp endpoint: (the same port, path /mcp)
# listening on ws://127.0.0.1:4460/api/v1/ws
JSTREAM_TOKEN_FILE=~/.config/jstream/token jstream-cli --url ws://127.0.0.1:4460 scene list
```

| Option | Meaning |
|---|---|
| `--token-file <path>` | The control token file. Default: the `JSTREAM_TOKEN_FILE` environment variable, which `jstream-cli` reads too. One of them is required. |
| `--port <port>` | The port on `127.0.0.1`. Default `0`: the OS picks a free one, and the URL line says which. |
| `--collection <path>` | Loads the collection from this JSON file and saves every change to it, so the next run starts where this one stopped. Created, holding the empty collection, if it doesn't exist. Default: none, the collection starts empty and is lost on exit. |
| `--ui-dir <dir>` | Serves the web UI's built files (`trunk build`'s `dist`) from this directory at `http://127.0.0.1:<port>/`. The browser asks for a user name and password: the password is the token (see **Web UI** below). A path that isn't a directory fails with exit code 1. Default: no UI. |
| `--output <id>` | Adds a stopped placeholder output (repeatable), so `output start`, `output stop` and `output status` have something to act on. On Windows it has no target, so starting it fails: create outputs with `output.create` instead. |
| `--log-dir <dir>` | Where the log files, crash reports and the audit log go, created (with its parents) if missing. Default: `logs` in the platform data directory (see **Log files** below). |
| `--log-level <spec>` | The log filter, in `RUST_LOG`'s syntax, which it overrides. |
| `--register-vcam` | Windows: registers the virtual camera for this machine, then exits (see **Virtual camera** below). Takes no other options. |
| `-V`, `--version` | Prints the daemon and protocol versions. |
| `-h`, `--help` | Prints the help. |

- **The token file.** If it exists, its token (the file's text without surrounding
  whitespace) is reused, so clients keep working across restarts. If it doesn't, it is
  created with a new random token and a newline. An existing file that is empty, holds
  no valid token (visible ASCII, no spaces), is not a regular file, or (on Unix) gives
  group or others any permission (mode bits `077`), is refused with exit code 1, and
  left as it is.
  The parent directory must exist. The token itself is never printed.
- **Output.** On stdout: `token file: <path> (created, new token)` or
  `… (existing token)`, then `mcp endpoint: http://127.0.0.1:<port>/mcp` (point an MCP
  client there, with the token as `Authorization: Bearer <token>`), then
  `ui: http://127.0.0.1:<port>/` with `--ui-dir`, then
  `listening on ws://127.0.0.1:<port>/api/v1/ws` once it
  serves (a script waits for this line), and `stopped` after a graceful stop. Errors go
  to stderr as `error: <message>`.
- **Web UI.** With `--ui-dir`, open the `ui:` address in a browser on the same
  machine. It asks for a user name and password: enter anything as the user name and
  the token file's contents as the password. The browser keeps them for the session
  and sends them on the UI's WebSocket connection too
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)).
- **Logs.** The libraries' log lines go to stderr, never stdout, and to log files:
  connections accepted and refused (with the reason, never the token), handshakes,
  output actions, and how connections end. `--log-level`, else `RUST_LOG`, filters
  both with `target=level` directives and an optional bare default level, for example
  `--log-level jstream_core=debug,info`; unset or empty means `info`. A value that
  doesn't parse is a usage error (exit 2), reported before anything is created.
- **Log files.** Each run writes `jstream-daemon-<UTC time>-<pid>-<n>.log` in the log
  directory, and starts a new file when one would pass 10 MiB; the newest 10 log files
  are kept and older ones deleted, so the logs stay under about 100 MiB. The default
  directory is `logs` in the data directory: `%APPDATA%\JStream` on Windows,
  `~/Library/Application Support/JStream` on macOS, `$XDG_DATA_HOME/jstream` (else
  `~/.local/share/jstream`) elsewhere. At `info` and above, the first line logged names the file. A log
  directory that can't be created, or a data directory that can't be found (no `HOME`
  or `APPDATA`) without `--log-dir`, is a startup failure (exit 1), before the token
  file is touched. On Unix the directories it creates are mode `0700` and the files
  `0600`.
- **Crash reports.** A panic writes `crash-<UTC time>-<pid>-<n>.txt` into the log
  directory and prints `crash report: <path>` to stderr; the newest 10 are kept.
  Nothing is uploaded ([R-SEC-004](../../docs/RULES.md#R-SEC-004)): attach the report
  to a bug report yourself. What it holds is in [Design](#design).
- **Stopping.** Ctrl-C (SIGINT) or SIGTERM on Unix, Ctrl-C or Ctrl-Break on Windows:
  it stops accepting connections, closes the open ones (their clients see
  `Disconnected`, and event streams end) and exits 0. Without `--collection` the
  engine's state is lost.
- **Going live (Windows).** `output.create` with an RTMP(S) server URL, then
  `output.set_key` (stored in Windows Credential Manager), then `output.start`
  connects and streams. A start fails with `action_failed` and a reason that says what
  to fix when the output has no target (a `--output` placeholder), no stream key, or
  the key can't be read. Elsewhere, `output.start` only steps the placeholder state
  machine and `output.set_key` fails: there is no key store.
- **Media Foundation (Windows).** On a Windows without Media Foundation (an N or KN
  edition without the Media Feature Pack, or Windows Server without its "Server Media
  Foundation" feature), the daemon logs the problem, prints `error: Media Foundation
  is not installed: <what to install>` and exits 3, after creating the log directory
  and before touching the token file. If Media Foundation is there but has no AAC
  encoder, it does the same with `error: Media Foundation's AAC encoder is not
  installed: …`. Listing the codecs fails with exit 1.
- **Virtual camera (Windows).** `jstream-daemon --register-vcam` registers
  `jstream_vcam_windows.dll` from the daemon's own folder machine-wide, as the Windows
  camera Frame Server requires. From a normal prompt it shows one UAC prompt and runs
  itself elevated, without a window. It prints `virtual camera: registered <dll>`, adding
  the old path when the registration named another folder, or `virtual camera: already
  registered (<dll>)` without asking for elevation, leaving the registration and the
  file's ACL as they are (a DLL registered with `regsvr32`, or rebuilt in place, keeps
  whatever ACL it has). A missing DLL, a declined prompt or a
  failed registration exits 1; from an administrator prompt the error says what failed.
  At startup the daemon logs a warning when the registration names another folder (a
  moved install), and an `info` line when there is none. Elsewhere `--register-vcam`
  fails with exit 1.
- **Collection file.** With `--collection`, the engine loads the file at startup
  (upgrading an older version) and replaces it atomically after every applied batch,
  undo and redo, before the call returns. A file that can't be read, holds no
  collection the engine can load, or can't be created is a startup failure (exit 1)
  naming the file and what is wrong, and the file is left as it is. A save that fails
  later is logged at `error` and retried with the next change; the call still
  succeeds. Undo history is not saved. Nothing stops two daemons from using the same
  file: each saves over the other's changes, so give each its own.
- **Audit log.** Every batch, undo, redo and action, by any client, is one JSON line
  in `audit.jsonl` in the log directory: the time, the client's ID and `hello` name,
  the call and its edits by name, the output an action names, the `rev` before and
  after, and the result code. Never a call's arguments, so never a stream key
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). When it would pass 8 MiB it becomes
  `audit.1.jsonl`, replacing the one before. A log that can't be opened is a startup
  failure (exit 1).

| Exit code | Meaning |
|---|---|
| 0 | Stopped by a signal, printed the help or version, or registered the virtual camera (`--register-vcam`) |
| 1 | Failed: `--register-vcam` could not register the virtual camera (or isn't on Windows), the log directory, a log file or the audit log can't be created, the collection file can't be read, loaded or created, the token file is unusable or can't be created, the media clock can't be opened (Windows), the port can't be bound, the `--ui-dir` directory isn't one, the signal handlers or runtime can't be set up, or the server failed while serving |
| 2 | Usage: the command line could not be parsed, names no token file, or `--log-level` or `RUST_LOG` doesn't parse |
| 3 | Windows only: Media Foundation or its AAC encoder is not installed (see **Media Foundation** above) |

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-daemon/src/**", "apps/jstream-daemon/Cargo.toml"]
depends_on = ["crates/jstream-server/README.md#design", "crates/jstream-core/README.md#design", "docs/ARCHITECTURE.md#persistence"]
verified = { hash = "b3:bbe355fa0a69d257", by = "agent:claude@session_01Wfrx1wPtLnDmWjyjwBTwba", note = "G3b merge-only round: merged main at 0ba4f97 (#264 shared subprotocol constants, #277 fuzz targets and graph property suites, #285 R-TEST-004 apps); re-read main's new text beside this PR's virtual_cam text; protocol 0.18 holds; daemon backends still vcam: None" }
-->

- **One engine, a session per connection.** `serve.rs` builds one `Engine` and hands
  the server a `Connector` that calls `Engine::connect()`, so every WebSocket client
  gets its own `InProcessControl` (its own `client_id`) on the shared engine, and sees
  every other client's events ([jstream-core › Design](../../crates/jstream-core/README.md#design)).
  The daemon never touches the protocol itself. The connector holds the engine in an
  `Arc`; the server, and through it every session, keep it alive.
- **Live on Windows** (`serve.rs`). Under `cfg(windows)` the engine gets the
  `jstream-platform-windows` backends (`QpcClock`, `WgcCapture`, `MfVideoDevices`,
  `WasapiAudio`, `MfCodecs`, `WindowsGpu`; no virtual camera sink yet), `WindowsCredentials` as its
  stream key store (`with_key_store`), and a publish opener (`with_publish_opener`).
  Creating the backends opens no device; each opens what it needs when a pipeline
  starts. Elsewhere there are no backends yet, so outputs stay the placeholder state
  machine ([jstream-core › Design](../../crates/jstream-core/README.md#design)).
- **The publish opener.** On each `output.start` it reads the output's server URL with
  `Engine::output_kind` and its key from the store, then calls `connect_rtmp`, so each
  start connects anew. It fails the start with its own reason (`PublishError::new`)
  when the output is not in the collection, is a kind it can't publish, has no key, or
  the store fails; no reason carries the key
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)), and `connect_rtmp`'s errors don't
  either. The engine stores the opener, so the opener holds only a `Weak` to the
  engine's `Arc`, set once the engine is built: a strong handle would be a cycle that
  keeps the engine and its running pipeline alive after the server lets go. The
  opener upgrades it only to read the target. `live` is that wiring over any backends,
  key store and connect function, which is how the tests run it on the platform fakes.
- **Media Foundation check and delay-load.** The `windows` crate imports Media
  Foundation's DLLs as load-time imports, so on a Windows without Media Foundation
  the loader would refuse to start the binary, naming a DLL, before `main` runs.
  `build.rs` links the binary (not its test binaries) with `/DELAYLOAD` for each DLL
  the daemon's backends import from (`mfplat.dll` for `MfCodecs` and
  `MfVideoDevices`, `mf.dll` and `mfreadwrite.dll` for `MfVideoDevices`) and
  `delayimp.lib` when the target is MSVC, so each loads on the first call into it.
  `main.rs` runs `jstream_platform_windows::check_media_foundation` right after
  logging starts, before the engine is built or anything else calls Media Foundation,
  then lists `MfCodecs`' codecs and requires an AAC encoder (`components.rs`, plain
  data in and out, tested everywhere), since every stream's audio needs one. Either
  failure is a `MissingComponent`, mapped to exit code 3 with its advice.
  `mfsensorgroup.dll` (`WindowsVcam`) joins the list when the daemon links the
  virtual camera, not before: delay-loading a DLL the binary doesn't import from is
  linker warning LNK4199, which CI's `-D warnings` turns into an error.
  `tests/delay_load.rs` checks the built binary's import tables, so CI's Windows job
  proves `/DELAYLOAD` applies to the `windows` crate's raw-dylib imports.
- **Virtual camera registration** (`register.rs`). `--register-vcam` is a separate mode
  that exits without serving, so it creates no logs or token. The registry and ACL work
  is `jstream-platform-windows`'s (`register_vcam`), because the daemon has no `unsafe`.
  A non-elevated daemon doesn't write: it relaunches itself elevated with the same fixed
  argument (`run_elevated`), then reads the registration back, so success is judged by
  what the registry holds rather than by the hidden child's output. A registration that
  already names this DLL is left alone, so repeating the command never prompts. The
  startup check (`log_status`) only logs: the daemon serves without the camera, and
  `WindowsVcam::start` refuses to start an unregistered camera on its own.
- **Collection file and audit log** (`serve.rs`, `main.rs`). The daemon only passes
  them to the engine: `Engine::with_collection_file` loads and saves the collection,
  and `with_audit` takes a `jstream_core::AuditFile`, whose writer thread appends
  entries off the engine's locks, so a slow disk never delays a call
  ([jstream-core › Design](../../crates/jstream-core/README.md#design)). The `--output`
  placeholders are declared outputs, which a loaded collection never removes, so they
  are added after the load only for reading order. The audit log lives in the log
  directory, which the daemon already creates (user-only on Unix) and which `--log-dir`
  moves, rather than in a directory of its own. Its entries are written as they come;
  on a graceful stop the writer finishes the queue when the engine is dropped. Without `--collection` nothing is saved, as before
  ([Open questions](#open-questions), 6).
- **MCP over streamable HTTP.** `serve.rs` sets `ServerConfig::mcp`, so the server
  answers MCP at `/mcp` on the same port, with its default MCP session cap and idle
  timeout. Each MCP session is one more engine client through the same `Connector`, so
  an agent's edits form their own undo history
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)). There is
  no option to turn it off: it is behind the same token as the WebSocket API, which
  already gives full control ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)).
- **Listening.** Always `127.0.0.1` (`ServerConfig::new`, with only the port, MCP and the UI directory changed):
  the server's `Host`, `Origin` and token checks apply unchanged
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)). There is no
  option to listen on other addresses until remote browsers need it (with TLS and token
  pairing, [ROADMAP › M2](../../ROADMAP.md#m2)).
- **The token** ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence): a random
  token in a user-only file). New tokens are 32 bytes from the OS's random source
  (`getrandom`), written as 64 lowercase hex characters, so they fit an `Authorization`
  header unchanged. The file is created with `create_new`, so a file that appears
  meanwhile is never replaced, and on Unix with mode `0600`. The random bytes are drawn
  before the file is created, and a failed write removes the file, so no empty or
  partial token is left to be reused. Reusing an existing token keeps configured
  clients working; rotating it means deleting the file and restarting.
- **Signals.** The handlers are installed before the server binds, so a failure is a
  startup error (exit 1) rather than a daemon that can't be stopped gracefully. The
  signal future is `serve_with_shutdown`'s, which closes open connections with close
  code 1001.
- **Output through writers, errors with `anyhow`.** Clippy denies `print!`
  ([R-RS-005](../../docs/RULES.md#R-RS-005)), so output goes through locked stdout and
  stderr handles. Output is best effort: with stdout closed the server still runs.
  Failures are `anyhow` errors printed with their causes (apps may use `anyhow`,
  [R-RS-003](../../docs/RULES.md#R-RS-003)); only the exit code's kind matters, so no
  typed error is needed, unlike `jstream-cli`'s one code per protocol error.
- **Logging.** The libraries log through `tracing`; as the host, the daemon installs
  the one subscriber ([R-RS-005](../../docs/RULES.md#R-RS-005)): two of
  `tracing-subscriber`'s `fmt` layers writing the same text lines, one to stderr (so
  stdout keeps only the lines scripts read) and one to the log files, under one
  `Targets` filter parsed from `--log-level` or `RUST_LOG`. It is installed after the
  token file path is known and before the token is loaded, and nothing is logged before
  it, so usage errors still start with `error:`. `Targets` takes `target=level`
  directives, not `env-filter`'s span and field filters, which need `regex`; that is
  enough to turn one crate up. The spans and fields are each library's
  ([jstream-server › Design](../../crates/jstream-server/README.md#design),
  [jstream-core › Design](../../crates/jstream-core/README.md#design)); the files get
  exactly what stderr gets, so the libraries' redaction
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001): the token is never logged) holds for
  both, and the end-to-end test checks the file for the token too.
- **Log rotation by size, hand-rolled** (`logs.rs`, [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence):
  rotating logs in the data directory). A new file per run and per 10 MiB, and the
  newest 10 kept, bound the disk used however verbose the filter or long the run; a
  file per run also keeps two daemons from writing one file. `tracing-appender` was
  the alternative: it rotates by time only (a day of `debug` lines is unbounded), and
  would add `time`, `crossbeam-channel` and `symlink` for what is about 100 lines over
  a mutex-guarded `File`. The subscriber writes each event with one `write` call, so a
  file only ever switches between lines. Writes are synchronous, like stderr's; the
  libraries don't log on real-time threads ([R-RS-005](../../docs/RULES.md#R-RS-005)).
  Pruning skips the file being written but may delete another running daemon's (on
  Unix it keeps writing to the unlinked file; on Windows `std` opens files with
  `FILE_SHARE_DELETE`, so the same is expected, untested).
- **The data directory** is found from the environment (`APPDATA`, `HOME`,
  `XDG_DATA_HOME`), not a crate: three cases don't pay for `dirs`, which adds
  `dirs-sys` and, on Windows, `windows-sys`. ADR-0020's `portable` marker is not
  handled yet ([Open questions](#open-questions)).
- **Crash reports** (`crash.rs`, [R-SEC-004](../../docs/RULES.md#R-SEC-004)). The panic
  hook, installed as soon as the log directory exists, writes the daemon and protocol
  versions, OS and architecture, the UTC time, the process ID, the thread name, the
  panic's location and message (cut at 16 KiB), and a backtrace (`std`'s, always
  captured). It never writes the environment, the command line or settings. Secrets
  registered with it (the control token, by `token.rs`) are replaced by `***` anywhere
  in the report and in its stderr line ([R-SEC-001](../../docs/RULES.md#R-SEC-001));
  only their length and BLAKE3 digest are kept, and the text is scanned for windows
  with that digest, so the hook holds no plaintext. It replaces the default hook,
  whose stderr line would quote the message unredacted. A panic in a tokio task writes
  a report too, even though the daemon carries on. The oldest reports beyond 10 are
  deleted before each new one.
- **Argument parsing is hand-rolled**, for the reasons `jstream-cli` gives
  ([jstream-cli › Design](../jstream-cli/README.md#design)): a handful of options
  (`--token-file`, `--port`, `--output`, `--log-dir`, `--log-level`) don't pay for a
  parser crate.
- **Dependencies.** `jstream-core` and `jstream-server` (what a host links),
  `jstream-platform` (`Backends`, the key store trait), `jstream-platform-windows` on
  Windows only (the backends, the key store and the Media Foundation check),
  `jstream-protocol` (`OutputId`, `OutputKind`), `jstream-media` (`Codec`, for the AAC
  check), `anyhow`, `tracing` (the startup line) and `blake3`
  (the crash reports' secret digests), both already in the tree through the server, tokio (`rt-multi-thread` for the runtime,
  `signal` for Ctrl-C, which adds `signal-hook-registry` on Unix), `tracing-subscriber`
  (features `fmt` and `std` only: no `ansi`, `env-filter` or `tracing-log`, so it adds
  only `sharded-slab` and `thread_local`), and `getrandom`
  (OS randomness; already in the tree through tungstenite's `rand`). `std` has no
  portable source of cryptographic randomness, and the alternatives without a crate
  (`RandomState`'s hash keys, reading `/dev/urandom`) are either not documented as
  cryptographic or Unix only. Dev-dependencies: `jstream-control` (feature `ws`),
  `futures-util` and tokio's `time`, to drive the daemon as a client would;
  `jstream-platform` (feature `fake`), for the live engine and opener tests.

<a id="open-questions"></a>
### Open questions

1. **Default port.** No port is chosen yet, so the default is a free one and the URL
   line reports it. The docs' examples use 4455, which is also OBS WebSocket's default
   port, so a JStream default should avoid it. A fixed default would let `jstream-cli`
   default its `--url` ([jstream-cli › Design](../jstream-cli/README.md#design), open
   question 5).
2. **Default token file location.** The platform config directory
   ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence); `%APPDATA%\JStream`
   or the portable folder per [ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)),
   once the engine's settings code locates it. Until then the path is required, and the
   same variable serves the daemon and the CLI.
3. **Windows ACL.** On Windows the new file gets its directory's ACL, which is
   user-only under `%APPDATA%` but not everywhere, and an existing file's ACL is not
   checked. Restricting it explicitly needs the Windows security API, so it belongs in
   `jstream-platform-windows`, the crate allowed `unsafe`.
4. **Native crash dumps.** Only panics write a report. A native crash (an access
   violation in a driver or FFI code, an abort) writes nothing yet, though
   [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence) plans local minidumps.
   On Windows that needs either `MiniDumpWriteDump` from an unhandled-exception filter,
   which is `unsafe` and so belongs in `jstream-platform-windows`
   ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001); the daemon forbids `unsafe`), or
   an out-of-process monitor (`crash-handler` and `minidumper`), whose attach call is
   `unsafe` too and which adds a helper process. Either needs testing on Windows.
5. **Data directory.** The log directory's default should come from the same settings
   code as question 2, which will also handle ADR-0020's `portable` marker (state in a
   folder beside the exe). Until then the daemon reads `APPDATA`, `HOME` and
   `XDG_DATA_HOME` itself, and `--log-dir` overrides them.
6. **Default collection file.** Without `--collection` the collection is not saved.
   A default (in the data directory, as [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)
   places collections) would make every run persist, but needs the same settings code as
   question 2 to find that directory, and a way to start from scratch.
