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
verified = { hash = "b3:160f4916f930df3d", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "args.rs gained state get, source list, item list; run.rs gained their table output; contents bullets (parser, running, exit codes, block_on) and the not-yet list still hold" }
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
to an engine needs `--mock`, which needs a build with the `mock` cargo feature
(`cargo run -p jstream-cli --features mock -- --mock …`).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-cli/src/args.rs", "apps/jstream-cli/src/exit.rs", "apps/jstream-cli/src/run.rs"]
verified = { hash = "b3:4fea1e84e90d1879", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Command table adds state get, source list, item list <scene-id>; new Reads bullet describes the tables (kind in builtin:/scene: spelling, items bottom first with VISIBLE/POSITION/SCALE/ROTATION/CROP, empty-list lines), checked against run.rs and the new golden tests; exit codes unchanged (item list of a missing scene exits 15)" }
-->

```sh
jstream-cli [global options] <command> [arguments]
jstream-cli --mock --mock-output out_1 output start out_1
jstream-cli --mock edit --expected-rev 0 scene.create main Main \
    source.create cam Camera builtin:webcam item.add main cam_1 cam
```

Global options come before the command: `--json`, `--mock`, `--mock-output <id>`
(repeatable; adds a stopped output to the mock) and `--help`. `jstream-cli help` prints
the full grammar. `--mock` needs the `mock` cargo feature; without it, `--mock` is a
usage error that says so.

| Command | Protocol call |
|---|---|
| `version` | none: prints the CLI and protocol versions |
| `hello` | the `hello`/`welcome` handshake |
| `state get` | query `state.get` |
| `scene list` | query `scene.list` |
| `source list` | query `source.list` |
| `item list <scene-id>` | query `item.list` |
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
- **Output:** human-readable by default; a call's line ends with its request ID
  (`applied: rev 4 (req_1)`, `output.start accepted for out_1 (req_1)`), which matches
  the `state.changed` event it causes. With `--json`, each result is one line of JSON
  on stdout: the `welcome` for `hello`, the protocol `Response` (request ID and result)
  for a call, and each event as the protocol serializes it. Events of a kind this
  protocol version doesn't know are skipped, as the protocol asks.
- **Reads** print `rev N` first, then a table per list: scenes with their item counts;
  sources with their kind in the `source.create` spelling (`builtin:webcam`,
  `scene:main`); items bottom first, with `VISIBLE`, `POSITION` (`x,y`), `SCALE`,
  `ROTATION` and `CROP` (`left,top,right,bottom`) in the argument order of the edits
  that set them. `state get` prints the sources, then each scene in display order with
  its items. An empty list prints `no scenes`, `no sources` or `no items`.
- **Errors** go to stderr. For people: `error: <message>`, with ` (<code>)` appended
  when it is an engine error (``error: scene `x` does not exist (not_found)``). With
  `--json` (found among the global options even when parsing fails): an object
  `{"error": {"code": …, "message": …}}`. For an engine error it is the protocol's wire
  error, with its detail fields; otherwise the code is one of the CLI's own, `usage`,
  `disconnected` or `io`, which appear only in JSON output.
- **Events** are printed and flushed one by one until `--count` is reached or the
  stream ends. A stream that ends means the engine went away, so it exits with code 3.

**Exit codes.** Provisional until the maintainer confirms the scheme
([open question 4](#design)); after that, a released code keeps its meaning.

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
verified = { hash = "b3:26b56fe42751b8b7", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Open question 2 (more queries) resolved by protocol 0.3 and replaced with the persistent-mock question; parser, block_on, connector seam, opt-in mock and writer bullets unchanged against src/ and Cargo.toml; control#design dependency re-read" }
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
  Result<Box<dyn Control>, CliError>`, and everything after it sees only
  `dyn Control`. `--mock` builds a fresh `MockEngine`, so its state lasts one run.
  Without `--mock` the connector answers `Disconnected`, until the WebSocket client
  lands; then that branch opens it, with the address and token file as new global
  options.
- **The mock is opt-in.** This crate's `mock` feature (off by default) enables
  `jstream-control/mock` and the `--mock` branch; without it, `--mock` is a usage error,
  so a default or release build carries no mock. The unit tests get `MockEngine`
  through a dev-dependency with `mock` on, whatever the features, and test both
  branches of `--mock`; the binary-level test (`tests/exit_codes.rs`) needs the feature
  (`required-features`) and runs under `--all-features`.
- **Output through writers.** Clippy denies `print!` ([R-RS-005](../../docs/RULES.md#R-RS-005)),
  so `run` writes to the `Write` handles it is given: locked stdout and stderr in
  `main`, byte buffers in tests, which compare the exact human and JSON output.
- **Errors are typed, not `anyhow`.** Each failure must map to its own exit code, so
  `CliError` keeps the `ControlError` (and its protocol code) instead of erasing it.

### Open questions

1. **`jstream mcp`.** The stdio MCP proxy ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape))
   would make the CLI depend on `jstream-mcp`, which doesn't exist yet. Left out.
2. **A persistent mock.** `--mock` starts empty on every run, and a run is one command,
   so through the binary the read commands always show an empty collection (the unit
   tests read a populated `MockEngine`). Should `--mock` load and save its state (a
   file option) until a real engine can be reached?
3. **The `mock` feature after the WebSocket client.** Keep it as a scripting sandbox,
   or remove it once a real engine can be reached.
4. **Exit codes.** The numbering (2 usage, 3 disconnected, 10 and up for protocol
   codes) is provisional until the maintainer confirms it.
