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
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:020254e87be7003d", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Review #70 N1: Serving bullet now says the signal handlers are installed before the bind, as main.rs/serve.rs do and Design says; tests/daemon.rs reads stdout through a reader thread with 10 s deadlines (N2), which the Tests bullet still describes correctly" }
-->

The headless engine host ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)): it runs a
`jstream-core` `Engine` and serves it through `jstream-server`'s authenticated WebSocket
API on `127.0.0.1`, so `jstream-cli --url …` and the stdio MCP proxy `jstream-cli mcp`
can drive it. Like every host it only wires the engine to its interfaces
([ADR-0001](../../docs/adr/0001-library-first-engine.md)): each connection gets its own
`InProcessControl` session through the server's `Connector`. Layer 6, an app that links
core and server ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut, the M1a host of [ROADMAP › M1](../../ROADMAP.md#m1)):
- **Startup** (`main.rs`): parses the command line, loads or creates the token, runs
  the tokio runtime, prints where it listens, and maps failures to exit codes.
- **Command line** (`args.rs`): a hand-rolled, pure parser ([Usage](#usage)).
- **Token file** (`token.rs`): reuses the token in an existing user-only file, or
  creates the file with a new random token ([Design](#design)).
- **Serving** (`serve.rs`): builds the engine (with its placeholder outputs), installs
  the signal handlers that stop it, then binds the server.
- **Tests** (`serve.rs`, `token.rs`, `args.rs` unit tests; `tests/daemon.rs`, the
  built binary driven by `jstream-control`'s WebSocket client and stopped with SIGINT).

Not here yet: serving the web UI, MCP over streamable HTTP (both wait on
`jstream-server`, [jstream-server › Design](../../crates/jstream-server/README.md#design)),
`--collection` (the engine has no persistence), a default port and a default token file
location ([Design › Open questions](#design)).

Why a crate: it is a host binary of its own (ADR-0020 ships `jstream-daemon.exe` next
to the desktop app), and only an app may link both `jstream-core` and `jstream-server`
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-daemon/src/args.rs", "apps/jstream-daemon/src/main.rs", "apps/jstream-daemon/src/token.rs"]
verified = { hash = "b3:1acb1fdd9f33d6c4", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "token.rs: a create_new failure other than AlreadyExists now still goes to read_existing when something is at the path (Windows reports a directory as AccessDenied), so 'is not a regular file' refusal holds on Windows too, as the text already says; nothing else changed" }
-->

```sh
jstream-daemon --token-file ~/.config/jstream/token --port 4460 --output out_1
# token file: /home/me/.config/jstream/token (created, new token)
# listening on ws://127.0.0.1:4460/api/v1/ws
JSTREAM_TOKEN_FILE=~/.config/jstream/token jstream-cli --url ws://127.0.0.1:4460 scene list
```

| Option | Meaning |
|---|---|
| `--token-file <path>` | The control token file. Default: the `JSTREAM_TOKEN_FILE` environment variable, which `jstream-cli` reads too. One of them is required. |
| `--port <port>` | The port on `127.0.0.1`. Default `0`: the OS picks a free one, and the URL line says which. |
| `--output <id>` | Adds a stopped placeholder output (repeatable), so `output start`, `output stop` and `output status` have something to act on. |
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
  `… (existing token)`, then `listening on ws://127.0.0.1:<port>/api/v1/ws` once it
  serves (a script waits for this line), and `stopped` after a graceful stop. Errors go
  to stderr as `error: <message>`.
- **Stopping.** Ctrl-C (SIGINT) or SIGTERM on Unix, Ctrl-C or Ctrl-Break on Windows:
  it stops accepting connections, closes the open ones (their clients see
  `Disconnected`, and event streams end) and exits 0. The engine's state is lost.
- `--collection` is refused with a usage error that says it is not supported yet.

| Exit code | Meaning |
|---|---|
| 0 | Stopped by a signal, or printed the help or version |
| 1 | Failed: the token file is unusable or can't be created, the port can't be bound, the signal handlers or runtime can't be set up, or the server failed while serving |
| 2 | Usage: the command line could not be parsed, or names no token file |

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-daemon/src/**", "apps/jstream-daemon/Cargo.toml"]
depends_on = ["crates/jstream-server/README.md#design", "crates/jstream-core/README.md#design", "docs/ARCHITECTURE.md#persistence"]
verified = { hash = "b3:062399e995dbb35d", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Merged main into #80: dependency ARCH#persistence gains this PR's Asset files bullet; the daemon still persists nothing but its token file, so its design holds" }
-->

- **One engine, a session per connection.** `serve.rs` builds one `Engine` and hands
  the server a `Connector` that calls `Engine::connect()`, so every WebSocket client
  gets its own `InProcessControl` (its own `client_id`) on the shared engine, and sees
  every other client's events ([jstream-core › Design](../../crates/jstream-core/README.md#design)).
  The daemon never touches the protocol itself.
- **Listening.** Always `127.0.0.1` (`ServerConfig::new`, with only the port changed):
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
- **Argument parsing is hand-rolled**, for the reasons `jstream-cli` gives
  ([jstream-cli › Design](../jstream-cli/README.md#design)): three options don't pay
  for a parser crate.
- **Dependencies.** `jstream-core` and `jstream-server` (what a host links),
  `jstream-protocol` (`OutputId`), `anyhow`, tokio (`rt-multi-thread` for the runtime,
  `signal` for Ctrl-C, which adds `signal-hook-registry` on Unix), and `getrandom`
  (OS randomness; already in the tree through tungstenite's `rand`). `std` has no
  portable source of cryptographic randomness, and the alternatives without a crate
  (`RandomState`'s hash keys, reading `/dev/urandom`) are either not documented as
  cryptographic or Unix only. Dev-dependencies: `jstream-control` (feature `ws`),
  `futures-util` and tokio's `time`, to drive the daemon as a client would.

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
4. **Logging.** Nothing is logged beyond the startup lines, as in `jstream-server`
   (its open question 6): `tracing` comes with the first code that needs it.
