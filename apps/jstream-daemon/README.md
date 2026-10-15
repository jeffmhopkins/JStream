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
verified = { hash = "b3:761e5c3e899a34e5", by = "agent:claude@session_01WG1uLVazjTqn2gmsarSsqg", note = "Merge of main (#147 nested scenes, #149, #153 MCP over HTTP, #163, #166 CLI/mock outputs, #170 record kind 0.9, #178, #181, #187) into B2 (0.10): re-read; #153 MCP-at-/mcp text holds; B2 only touched a daemon test pattern (cause: None)" }
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
  `--log-level` or `RUST_LOG`), loads or creates the token, runs the tokio runtime,
  prints where it listens, and maps failures to exit codes.
- **Command line** (`args.rs`): a hand-rolled, pure parser ([Usage](#usage)).
- **Token file** (`token.rs`): reuses the token in an existing user-only file, or
  creates the file with a new random token ([Design](#design)).
- **Serving** (`serve.rs`): builds the engine (with its placeholder outputs), installs
  the signal handlers that stop it, then binds the server with MCP on.
- **Log files** (`logs.rs`): the data directory, and the size-capped rotating log files
  ([Design](#design)).
- **Crash reports** (`crash.rs`): the panic hook that writes a redacted report into the
  log directory ([Design](#design)).
- **Tests** (`serve.rs`, `token.rs`, `args.rs`, `logs.rs`, `crash.rs` unit tests, the
  crash hook in a child process; `tests/daemon.rs`, the built binary driven by
  `jstream-control`'s WebSocket client and a raw HTTP MCP client, and stopped with
  SIGINT).

Not here yet: serving the web UI (it waits on `jstream-server`,
[jstream-server › Design](../../crates/jstream-server/README.md#design)),
`--collection` (the engine has no persistence), a default port and a default token file
location ([Design › Open questions](#open-questions)).

Why a crate: it is a host binary of its own (ADR-0020 ships `jstream-daemon.exe` next
to the desktop app), and only an app may link both `jstream-core` and `jstream-server`
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-daemon/src/args.rs", "apps/jstream-daemon/src/main.rs", "apps/jstream-daemon/src/token.rs"]
verified = { hash = "b3:96e6a64a7ccf1aed", by = "agent:claude@session_017CbEsAJTsNE6yE4tdDMzJm", note = "Merge of main (#155 kinds/protocol 0.8, #165 sweep) into I1 #153: re-read; mcp endpoint line and options match main.rs/args.rs" }
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
| `--output <id>` | Adds a stopped placeholder output (repeatable), so `output start`, `output stop` and `output status` have something to act on. |
| `--log-dir <dir>` | Where the log files and crash reports go, created (with its parents) if missing. Default: `logs` in the platform data directory (see **Log files** below). |
| `--log-level <spec>` | The log filter, in `RUST_LOG`'s syntax, which it overrides. |
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
  `listening on ws://127.0.0.1:<port>/api/v1/ws` once it
  serves (a script waits for this line), and `stopped` after a graceful stop. Errors go
  to stderr as `error: <message>`.
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
  `Disconnected`, and event streams end) and exits 0. The engine's state is lost.
- `--collection` is refused with a usage error that says it is not supported yet.

| Exit code | Meaning |
|---|---|
| 0 | Stopped by a signal, or printed the help or version |
| 1 | Failed: the log directory or a log file can't be created, the token file is unusable or can't be created, the port can't be bound, the signal handlers or runtime can't be set up, or the server failed while serving |
| 2 | Usage: the command line could not be parsed, names no token file, or `--log-level` or `RUST_LOG` doesn't parse |

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-daemon/src/**", "apps/jstream-daemon/Cargo.toml"]
depends_on = ["crates/jstream-server/README.md#design", "crates/jstream-core/README.md#design", "docs/ARCHITECTURE.md#persistence"]
verified = { hash = "b3:c5754e01110314ef", by = "agent:claude@session_01WYFCTjMhcJcAomAVNMYrRc", note = "Merge of main (B2 history, B3 geometry) into B5: stale through core/server design only; daemon text names no edit or placement, unaffected" }
-->

- **One engine, a session per connection.** `serve.rs` builds one `Engine` and hands
  the server a `Connector` that calls `Engine::connect()`, so every WebSocket client
  gets its own `InProcessControl` (its own `client_id`) on the shared engine, and sees
  every other client's events ([jstream-core › Design](../../crates/jstream-core/README.md#design)).
  The daemon never touches the protocol itself.
- **MCP over streamable HTTP.** `serve.rs` sets `ServerConfig::mcp`, so the server
  answers MCP at `/mcp` on the same port, with its default MCP session cap and idle
  timeout. Each MCP session is one more engine client through the same `Connector`, so
  an agent's edits form their own undo history
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)). There is
  no option to turn it off: it is behind the same token as the WebSocket API, which
  already gives full control ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)).
- **Listening.** Always `127.0.0.1` (`ServerConfig::new`, with only the port and MCP changed):
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
  `jstream-protocol` (`OutputId`), `anyhow`, `tracing` (the startup line) and `blake3`
  (the crash reports' secret digests), both already in the tree through the server, tokio (`rt-multi-thread` for the runtime,
  `signal` for Ctrl-C, which adds `signal-hook-registry` on Unix), `tracing-subscriber`
  (features `fmt` and `std` only: no `ansi`, `env-filter` or `tracing-log`, so it adds
  only `sharded-slab` and `thread_local`), and `getrandom`
  (OS randomness; already in the tree through tungstenite's `rand`). `std` has no
  portable source of cryptographic randomness, and the alternatives without a crate
  (`RandomState`'s hash keys, reading `/dev/urandom`) are either not documented as
  cryptographic or Unix only. Dev-dependencies: `jstream-control` (feature `ws`),
  `futures-util` and tokio's `time`, to drive the daemon as a client would.

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
