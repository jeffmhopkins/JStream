<!-- @doc
id = "CRATE-jstream-cli"
title = "jstream-cli"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-cli

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["apps/jstream-cli/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:8490c58c1bf9d243", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section: checked each Contents bullet against args.rs, run.rs, exit.rs and block.rs; dependencies are jstream-control (mock), serde, serde_json only, layer 6 as in ARCH#crate-map; mcp proxy and remote connection stated as not built" }
-->

The command-line client: it drives an engine from a shell or a script. It is a client
like the web UI and MCP, so it talks to the engine only through `jstream_control::Control`
and the `jstream-protocol` types, and never links the engine
([ADR-0001](../../docs/adr/0001-library-first-engine.md),
[R-ARCH-003](../../docs/RULES.md#R-ARCH-003)). Layer 6, an app
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut):
- **Commands** (`args.rs`): a hand-rolled parser that maps each command onto one protocol
  call: the handshake, queries, a batch of edits, actions, and the event stream
  ([Usage](#usage)).
- **Running** (`run.rs`): opens a session through a connector, runs the call, and writes
  human or `--json` output through the `Write` handles it is given.
- **Exit codes** (`exit.rs`): one per failure kind, including one per protocol error code.
- **Blocking** (`block.rs`): a thread-parking `block_on` for `Control`'s futures.

Not here yet: a connection to a real engine (it needs the WebSocket client in
`jstream-control`), and `jstream mcp`, the stdio MCP proxy
([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)). Until then every command that talks
to an engine needs `--mock`.

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-cli/src/args.rs", "apps/jstream-cli/src/exit.rs", "apps/jstream-cli/src/run.rs"]
verified = { hash = "b3:da71bc0d26e90102", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section: command table, edit grammar (all 13 protocol 0.2 edits), JSON shapes, stderr error format and the exit-code table checked against args.rs USAGE, run.rs and exit.rs and their golden tests" }
-->

```sh
jstream-cli [global options] <command> [arguments]
jstream-cli --mock --mock-output out_1 output start out_1
jstream-cli --mock edit --expected-rev 0 scene.create main Main \
    source.create cam Camera builtin:webcam item.add main cam_1 cam
```

Global options come before the command: `--json`, `--mock`, `--mock-output <id>`
(repeatable; adds a stopped output to the mock) and `--help`. `jstream-cli help` prints
the full grammar.

| Command | Protocol call |
|---|---|
| `version` | none: prints the CLI and protocol versions |
| `hello` | the `hello`/`welcome` handshake |
| `scene list` | query `scene.list` |
| `output status <id>` | query `output.status` |
| `output start <id>`, `output stop <id>` | action `output.start`, `output.stop` |
| `edit [--expected-rev <rev>] [--idempotency-key <key>] <edit>...` | one batch |
| `events [--count <n>]` | the event subscription |

Every command except `version` and `help` sends `hello` first, as client `jstream-cli`.

- **Edits** are written as their protocol command name followed by its arguments, in a
  fixed order (`scene.create <scene-id> <name>`, `item.set_crop <item-id> <left> <top>
  <right> <bottom>`, …), so the CLI adds no vocabulary of its own. Every edit in the
  protocol is supported. Several edits on one line go as **one atomic batch**: one new
  `rev`, or nothing applied. A source kind is `builtin:<kind>` or `scene:<scene-id>`.
- **Output:** human-readable by default. With `--json`, each result is one line of JSON
  on stdout: the `welcome` for `hello`, the protocol `Response` (request ID and result)
  for a call, and each event as the protocol serializes it. Events of a kind this
  protocol version doesn't know are skipped, as the protocol asks.
- **Errors** go to stderr: `error: <message> (<code>)`, or with `--json` an object
  `{"error": {"code": …, "message": …}}`. For an engine error it is the protocol's wire
  error, with its detail fields. The CLI's own codes are `usage`, `disconnected` and
  `io`.
- **Events** are printed and flushed one by one until `--count` is reached or the
  stream ends. A stream that ends means the engine went away, so it exits with code 3.

**Exit codes.** Stable: scripts may rely on them.

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | The output could not be written (a closed pipe exits silently) |
| 2 | Usage: the command line could not be parsed |
| 3 | `Disconnected`: no connection to the engine, or it was lost (whether the call took effect is unknown) |
| 10 | `unsupported_version` |
| 11 | `handshake_required` |
| 12 | `malformed` |
| 13 | `empty_batch` |
| 14 | `rev_conflict` |
| 15 | `not_found` |
| 16 | `already_exists` |
| 17 | `invalid_argument` |
| 18 | `action_failed` |
| 19 | `internal` |
| 20 | an error code this CLI doesn't know (from a newer engine) |

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-cli/src/**", "apps/jstream-cli/Cargo.toml"]
depends_on = ["crates/jstream-control/README.md#design"]
verified = { hash = "b3:18e302927628148a", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section: hand-rolled parser, thread-parking block_on, connector seam returning Disconnected without --mock, writer-based output and typed CliError all match src/ and Cargo.toml; consistent with jstream-control#design (boxed executor-free futures, mock always ready)" }
-->

- **Argument parsing is hand-rolled.** The grammar is small, and the part that matters
  most, a sequence of fixed-arity edits named by their protocol command names, doesn't
  fit a derive-based parser: with `clap` it would be a trailing list of raw strings
  parsed by hand anyway. A parser crate would add a dependency tree (and a review under
  [R-DEP-004](../../docs/RULES.md#R-DEP-004)) for the few global options and the help
  text. The parser is pure (`&[String]` in, a typed `Invocation` out) and unit-tested.
  Revisit if the command set grows enough that generated help and completions pay off.
- **No async runtime.** `Control` returns boxed futures that need no particular executor
  ([jstream-control › Design](../../crates/jstream-control/README.md#design)). The CLI
  does one call at a time, so `block_on` polls on the calling thread and parks between
  polls; a transport that completes a future from another thread wakes it through the
  waker. The mock's futures are always ready.
- **The connection is a seam.** `run` takes a connector, `Fn(&Connection) ->
  Result<Box<dyn Control>, ControlError>`, and everything after it sees only
  `dyn Control`. `--mock` builds a fresh `MockEngine` (from `jstream-control`'s `mock`
  feature, a normal dependency for now), so its state lasts one run. Without `--mock`
  the connector answers `Disconnected`, until the WebSocket client lands; then that
  branch opens it, with the address and token file as new global options.
- **Output through writers.** Clippy denies `print!` ([R-RS-005](../../docs/RULES.md#R-RS-005)),
  so `run` writes to the `Write` handles it is given: locked stdout and stderr in
  `main`, byte buffers in tests, which compare the exact human and JSON output.
- **Errors are typed, not `anyhow`.** Each failure must map to its own exit code, so
  `CliError` keeps the `ControlError` (and its protocol code) instead of erasing it.

### Open questions

1. **`jstream mcp`.** The stdio MCP proxy ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape))
   would make the CLI depend on `jstream-mcp`, which doesn't exist yet. Left out.
2. **More queries.** The protocol has only `scene.list` and `output.status`. Listing
   sources or items, or reading the full state, needs new protocol queries first
   ([R-ARCH-001](../../docs/RULES.md#R-ARCH-001)).
3. **The mock in release builds.** `--mock` ships in the binary. Once a real connection
   exists it could move behind a cargo feature.
