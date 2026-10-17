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
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*"]
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
- **Running** (`run.rs`): opens a session through a connector (the engine's WebSocket
  API through `jstream-control`'s `WsControl`, or the mock), runs the call (an edit
  batch may first read `state.get`, to fit new capture items, see [Design](#design)), and writes
  human or `--json` output through the `Write` handles it is given (`mcp`, and
  `output set-key` for the stream key, also read the `BufRead` it is given).
- **Exit codes** (`exit.rs`): one per failure kind, including one per protocol error code.
- **Blocking** (`block.rs`): a thread-parking `block_on` for `Control`'s futures.
- **MCP proxy** (`mcp.rs`): `jstream-cli mcp`, the stdio MCP proxy
  ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)): `jstream-mcp`'s server over the
  same session the other commands open.

Not here yet: a default address and token file ([open question 5](#design)). A real
engine is served by [`jstream-daemon`](../jstream-daemon/README.md#usage): point
`--url` at the URL it prints and the token file at the one it uses. Without one, the
CLI runs against `--mock`, which needs a build with the `mock` cargo feature
(`cargo run -p jstream-cli --features mock -- --mock …`).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-cli/src/args.rs", "apps/jstream-cli/src/exit.rs", "apps/jstream-cli/src/run.rs", "apps/jstream-cli/src/mcp.rs"]
-->

```sh
jstream-cli [global options] <command> [arguments]
jstream-cli --url ws://127.0.0.1:4455 --token-file /path/to/token scene list
jstream-cli --mock --mock-output out_1 output start out_1
jstream-cli --mock edit --expected-rev 0 scene.create main Main \
    source.create cam Camera builtin:webcam item.add main cam_1 cam
jstream-cli --url ws://127.0.0.1:4455 --token-file /path/to/token \
    edit output.create yt rtmps://a.rtmps.youtube.com:443/live2
jstream-cli --url ws://127.0.0.1:4455 --token-file /path/to/token \
    output set-key yt --key-file /path/to/stream-key
```

Global options come before the command: `--json`, `--url <url>`, `--token-file <path>`,
`--mock`, `--mock-output <id>` (repeatable; adds a stopped output to the mock) and
`--help`. `jstream-cli help` prints the full grammar.

- **A real engine:** `--url` is the engine's WebSocket API, `ws://<host>:<port>` (the
  path defaults to `/api/v1/ws`; `wss://` is not supported yet). The control token is
  read from the file named by `--token-file`, or else by the `JSTREAM_TOKEN_FILE`
  environment variable, without surrounding whitespace. There is no option that takes
  the token itself, which would show it in the process list
  ([ADR-0006](../../docs/adr/0006-mcp-full-access.md): the token lives in a user-only
  file). A missing `--url` or token file, or an unreadable or empty one, is a usage
  error.
- **The mock:** `--mock` needs the `mock` cargo feature; without it, `--mock` is a
  usage error that says so. It can't be combined with `--url` or `--token-file`.

| Command | Protocol call |
|---|---|
| `version` | none: prints the CLI and protocol versions |
| `hello` | the `hello`/`welcome` handshake |
| `state get` | query `state.get` |
| `scene list` | query `scene.list` |
| `scene switch <scene-id>` | action `scene.switch` (a cut to that scene) |
| `studio get` | query `studio.get` (the scene on program) |
| `capture targets` | query `capture.targets` (the displays and windows a `display` or `window` source's `target` can name) |
| `privacy on`, `privacy off` | action `privacy.set` (covers every output, or uncovers them) |
| `privacy get` | query `privacy.get` |
| `source list` | query `source.list` |
| `item list <scene-id>` | query `item.list` |
| `output list` | query `output.list` |
| `output status <id>` | query `output.status` |
| `video snapshot` | query `video.snapshot` |
| `output start <id>`, `output stop <id>` | action `output.start`, `output.stop` |
| `output set-key <id> [--key-file <path>]` | action `output.set_key`, with the key read from the file or stdin |
| `output clear-key <id>` | action `output.clear_key` |
| `pair` | action `pairing.start`: prints a one-time code to enter on the device |
| `device list` | query `device.list` |
| `device revoke <device-id>` | action `device.revoke` (also closes the device's connections) |
| `edit [--expected-rev <rev>] [--idempotency-key <key>] <edit>...` | one batch |
| `events [--count <n>]` | the event subscription |
| `mcp` | serves MCP on stdin and stdout; each tool call is one protocol call (see **MCP** below) |

Every command except `version`, `help` and `mcp` sends `hello` first, as client
`jstream-cli`.

- **Edits** are written as their protocol command name followed by its arguments, in a
  fixed order (`scene.create <scene-id> <name> [role:scene|role:group]`, `item.set_crop <item-id> <left> <top>
  <right> <bottom>`, …), so the CLI adds no vocabulary of its own. Every edit in the
  protocol is supported except settings, encoder profiles, video mixes,
  `source.create`'s `duplicated_from` and the filter edits. `source.set_settings`,
  `source.create` with settings, `encoder.create`, `encoder.update`, `encoder.delete`,
  `output.set_encoder`, `video_mix.create`, `video_mix.update`, `video_mix.delete`,
  `output.set_video_mix` and the `filter.*` edits need a JSON argument form the CLI
  doesn't have yet, so a
  source it creates gets its kind's defaults and an output it creates the engine's
  default encoder and the program (MCP and the WebSocket API take them). `item.add` of a
  `display`, `window` or `webcam` source sends a `geometry` covering the canvas with
  `fit`, so the whole capture is in frame; the canvas is the batch's latest
  `canvas.set_size`, else `state.get`'s, which the CLI also reads for the kind of a
  source the batch didn't create. Any other `item.add` sends neither placement, so its
  item is a legacy one at 1:1. Place either with `item.set_geometry <item-id>
  <center-x> <center-y> <width> <height> <rotation-deg> stretch|fit|fill` in the same
  batch ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)). Its 3D fields
  are always sent neutral, since the engine refuses anything else until it draws 3D.
  `canvas.set_size <width> <height>` sets the canvas. Groups
  ([ADR-0024](../../docs/adr/0024-scene-structure.md)) are
  `scene.set_role <scene-id> scene|group` and `item.move <item-id> <scene-id>
  <index>`, and `scene.create` takes an optional trailing `role:group` or `role:scene`:
  grouping is one batch that creates a scene as a group, places it through a `scene:`
  source, and moves the items into it. The placement edits'
  optional `scene_id` is not in the CLI: until keyframe scenes exist it can only name
  the item's own scene, which omitting it already means. Protocol 0.19's audio edits
  (`source.set_audio`, `bus.*`, `send.*`), `bus.list` and `audio.reset_loudness` are not
  in the CLI either.
  `item.set_source <item-id> <source-id>` switches an item to another source.
  `item.set_opacity <item-id> <0..1>` and `item.set_blend <item-id> <mode>` set an
  item's opacity and blend mode; the engine accepts only `normal` until it draws the
  others.
  `hotkey.set <chord> <action> <id>` binds a chord (`Ctrl+Shift+KeyS`, `F1`) to
  `scene.switch`, `output.start`, `output.stop` or `output.clear_key` on that scene or
  output, and `hotkey.clear <chord>` unbinds it; any other action is a usage error.
  Applying a batch prints `removed hotkey <chord>` for each binding its deletes
  removed. Several edits on one line go as **one atomic batch**: one new
  `rev`, or nothing applied. A source kind is `builtin:<kind>` or `scene:<scene-id>`.
  `output.create <output-id> <rtmp-url>` and `output.set_target <output-id>
  <rtmp-url>` take the ingest server URL (`rtmp://host[:port]/app` or `rtmps://…`),
  which is the whole target of a generic RTMP output. The CLI has no form for a
  recording output or a YouTube-bound one yet, and none of protocol 0.23's YouTube
  actions and queries. An argument without an `rtmp` or `rtmps` scheme is a usage
  error, which leaves other spellings free for later output kinds; the engine checks
  the rest. Neither message repeats
  the URL.
- **No undo or redo command.** Undo history belongs to a client and ends with its
  session ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), and each run of a
  command is a new client, so a one-shot `undo` would always find nothing to undo.
  `mcp` is one client for its whole run: its `history` tool undoes and redoes the
  agent's own edits.
- **No live-value command.** A gesture belongs to its client and expires when the
  session ends ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), so a one-shot
  `live.set` would be undone as the command exits. `mcp`'s `live` tool keeps one open
  for the agent's run. `events` shows other clients' gestures: human output prints
  `live.changed <gesture> by <client>: <setters>` and `live.ended <gesture> by
  <client> <outcome> [rev N]`.
- **Stream keys** are never arguments, so they stay out of shell history and the
  process list (`ps`), as the control token does. `output set-key <id>` reads the key
  from `--key-file <path>`, or else from stdin until it ends (pipe it in, or type it
  and press Ctrl-D; the terminal shows what is typed). A leading byte-order mark
  (Windows PowerShell 5.1 writes one on a pipe; Notepad's "UTF-8 with BOM" starts a
  file with one) and surrounding whitespace, such as a final newline, are dropped.
  Anything else after the output ID is a usage error that doesn't repeat it. An empty
  key, a key with a control or non-ASCII character (no streaming service issues one),
  an unreadable file, or input over 4096 bytes (no stream key is that long; it is
  refused before it is buffered or sent) is a usage error too, and the key is read
  before connecting. Neither the CLI nor the engine prints a key back: the
  reply is `output.set_key accepted for <id> (<request>)`, and `output status` says
  only whether one is set.
- **Output:** human-readable by default; a call's line ends with its request ID
  (`applied: rev 4 (req_1)`, `output.start accepted for out_1 (req_1)`), which matches
  the `state.changed` (or `studio.changed`, or `privacy.changed`) event it causes; an event an undo or redo caused ends with
  `undo` or `redo`. With `--json`, each result is one line of JSON
  on stdout: the `welcome` for `hello`, the protocol `Response` (request ID and result)
  for a call, and each event as the protocol serializes it. Events of a kind this
  protocol version doesn't know are skipped, as the protocol asks.
- **Reads** print `rev N` first, then a table per list: scenes with their `ROLE`
  (`scene` or `group`) and item counts;
  sources with their kind in the `source.create` spelling (`builtin:webcam`,
  `scene:main`); items bottom first, with `VISIBLE`, `POSITION` (`x,y`), `SCALE`,
  `ROTATION` and `CROP` (`left,top,right,bottom`) in the argument order of the edits
  that set them; for an item placed with a geometry, those columns show the
  protocol's deprecated transform view of it, and only `--json` shows its `geometry`
  and the canvas. Outputs are listed with their `KIND` (`rtmp`) and `TARGET` (the
  server URL, as `output.create` takes it; for any other output, its kind as JSON).
  `state get` prints the sources, then each
  scene in display order with its items, headed `scene <id> (<name>):` or `group <id>
  (<name>):` by its role, then the outputs, then a `hotkeys:` table (`CHORD`,
  `ACTION`) when any are bound; only `--json` shows the sources' audio, the buses and
  the sends. `output status` prints
  `<id>: <state>, stream key set` (or `not set`), and, while the output runs,
  `, own encoder` or `, encoder shared with <id>, <id>`. An empty list prints `no scenes`,
  `no sources`, `no items` or `no outputs`.
  `video snapshot` prints `rev N, WxH png (N base64 chars)` and does not print the
  payload; `--json` prints the full response.
- **Pairing** ([ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)): `pair`
  prints `pairing code: XXXX-XXXX` and how long it stays open; the user enters it on the
  device, which posts it to the server's `/api/v1/pair` for a device token of its own
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)). The code is
  printed because it is the answer, and is never logged. `device list` prints a table
  of `ID`, `NAME` and when each was paired and last used (Unix seconds), or `no paired
  devices`; `device revoke <id>` prints `device <id> revoked (<request>)`. The server
  answers these calls, so the engine must be reached through `--url`; `--mock` answers
  `unsupported` (exit 20).
- **Errors** go to stderr. For people: `error: <message>`, with ` (<code>)` appended
  when it is an engine error (``error: scene `x` does not exist (not_found)``). With
  `--json` (found among the global options even when parsing fails): an object
  `{"error": {"code": …, "message": …}}`. For an engine error it is the protocol's wire
  error, with its detail fields; otherwise the code is one of the CLI's own, `usage`,
  `disconnected` or `io`, which appear only in JSON output.
- **Events** are printed and flushed one by one until `--count` is reached or the
  stream ends. A stream that ends means the engine went away, so it exits with code 3.
  When the server dropped events because the CLI read too slowly, it prints
  `events.dropped <count>`. A pipeline drop sample prints
  `stats.tick seq <n> capture_frames <n> audio_frames <n> render_ticks <n> encode_frames <n> encode_blocks <n> output_packets <n>`.
  Any other event, `meters.tick` among them, prints as its JSON.

**MCP.** `jstream-cli mcp` is the stdio MCP proxy for an agent's MCP client to launch,
for example:

```json
{"command": "jstream-cli", "args": ["--url", "ws://127.0.0.1:4455", "mcp"],
 "env": {"JSTREAM_TOKEN_FILE": "/path/to/token"}}
```

- It connects with the global options above (`--url` and the token file, or `--mock`
  with the `mock` feature), then serves `jstream-mcp`'s tools, revisions and errors
  ([jstream-mcp › Design](../../crates/jstream-mcp/README.md#design)) on stdin and
  stdout, one JSON-RPC message per line, until stdin ends.
- **stdout carries only MCP messages.** Errors before serving go to stderr, as for any
  command (as JSON with `--json`). Nothing else is written to stderr while serving.
- **Connecting fails before serving**, with the usual exit codes: 2 for a missing URL
  or token file, 3 when the engine can't be reached or refuses the token. Nothing is
  written to stdout then, so the MCP client sees the process exit.
- **One session for the whole run.** Every tool call acts as one client of the engine,
  with one undo history; `hello` is sent at `initialize` (the proxy has already
  connected, and the server closes a connection that hasn't said `hello` in time), with
  the MCP client's name (`clientInfo.name`) instead of `jstream-cli`.
- **A lost engine doesn't stop the proxy.** Each tool call that needs the engine is then
  answered with the `disconnected` tool error, and `describe_api` still works, as does
  `events`, which returns what was buffered and says the feed ended. The proxy doesn't
  reconnect: the MCP client restarts it to get a new session, a new client of the
  engine with its own undo history ([open question 6](#design)).
- **Exit:** 0 when stdin ends, whatever the engine did meanwhile; 1 if stdin or stdout
  fails (a closed stdout exits silently).

**Exit codes.** Provisional until the maintainer confirms the scheme
([open question 4](#design)); after that, a released code keeps its meaning.

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | The output could not be written, or `mcp` or `output set-key` could not read stdin (a closed pipe exits silently) |
| 2 | Usage: the command line could not be parsed, or names no engine or no readable token file |
| 3 | `Disconnected`: no connection to the engine (unreachable, or it refused the token, `Origin` or `Host`), or it was lost (whether the call took effect is unknown) |
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
| 20 | an error code this CLI doesn't know (from a newer engine); `undo_conflict` and `history_empty`, which answer only a `history` call, which the CLI doesn't send; and `unsupported`, which `pair` and `device` get from an engine with no server in front (`--mock`) |

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-cli/src/**", "apps/jstream-cli/Cargo.toml"]
depends_on = ["crates/jstream-control/README.md#design"]
-->

- **Argument parsing is hand-rolled.** The grammar is small, and the part that matters
  most, a sequence of fixed-arity edits (only `scene.create` takes one optional trailing
  `role:`) named by their protocol command names, doesn't
  fit a derive-based parser: with `clap` it would be a trailing list of raw strings
  parsed by hand anyway. A parser crate would add a dependency tree (and a review under
  [R-DEP-004](../../docs/RULES.md#R-DEP-004)) for the few global options and the help
  text. The parser is pure (`&[String]` in, a typed `Invocation` out) and unit-tested.
  Revisit if the command set grows enough that generated help and completions pay off.
- **No async runtime of its own.** `Control` returns boxed futures that need no particular executor
  ([jstream-control › Design](../../crates/jstream-control/README.md#design)). The CLI
  does one call at a time, so `block_on` polls on the calling thread and parks between
  polls; a transport that completes a future from another thread wakes it through the
  waker. The mock's futures are always ready; the WebSocket client's are completed by
  its own connection thread (which runs tokio inside `jstream-control`).
- **The connection is a seam.** `run` takes a connector, `Fn(&Connection) ->
  Result<Box<dyn Control>, CliError>`, and everything after it sees only
  `dyn Control`. `--mock` builds a fresh `MockEngine`, so its state lasts one run.
  Without `--mock` the connector reads the token file and opens a `WsControl`
  (`jstream-control`'s `ws` feature, always on here) to `--url`. The parser stays pure:
  the environment variable and the file are read by the connector. The client runs its
  own connection thread, so `block_on` drives its futures too.
- **The mock is opt-in.** This crate's `mock` feature (off by default) enables
  `jstream-control/mock` and the `--mock` branch; without it, `--mock` is a usage error,
  so a default or release build carries no mock. The unit tests get `MockEngine`
  through a dev-dependency with `mock` on, whatever the features, and test both
  branches of `--mock`; the binary-level test (`tests/exit_codes.rs`) needs the feature
  (`required-features`) and runs under `--all-features`.
- **Output through writers.** Clippy denies `print!` ([R-RS-005](../../docs/RULES.md#R-RS-005)),
  so `run` writes to the `Write` handles it is given, and `mcp` and `output set-key`
  read the `BufRead` it is given: locked stdin, stdout and stderr in `main`, byte buffers in tests, which
  compare the exact human and JSON output.
- **The MCP proxy is `jstream-mcp` on the CLI's connection.** The binary keeps the name
  `jstream-cli` (maintainer's choice), so the stdio MCP proxy
  ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)) is its `mcp` subcommand. It goes
  through the same connector as every other command, so it takes the same options and
  fails to connect with the same exit codes, then hands the session to
  `jstream_mcp::Server` and runs `jstream_mcp::serve_stdio` on stdin and stdout. The
  CLI adds no MCP code of its own (`jstream-mcp` is layer 3, below this app, and needs
  no runtime). Like `ws`, it is always built, with no feature: `mcp` is part of what the
  CLI is for (ARCH), and it adds no third-party crate. It doesn't send `hello` first,
  unlike the other commands: the server does, at `initialize`, with the MCP client's
  name, so the audit log names the agent rather than the CLI. A lost connection follows `jstream-mcp`'s
  design: a `disconnected` tool error per call that the model can read and act on,
  while the transport keeps going until the MCP client closes stdin. Exiting instead
  would surface to the agent only as a crashed server. The proxy doesn't reconnect by
  itself ([open question 6](#design)).
- **New capture items fit the canvas.** The protocol's placement for an `item.add`
  that sends none is the legacy 1:1 one (ADR-0022), which shows only the top-left
  corner of a display larger than the canvas. Changing that default would change the
  wire meaning of an omitted field, so the CLI chooses instead: before sending a batch,
  `fit_captures` gives each such `item.add` of a `display`, `window` or `webcam` source
  `ItemGeometry::covering_canvas` (`fit`). The canvas is the batch's latest
  `canvas.set_size` before the item; a source created earlier in the batch gives its
  kind. Anything else comes from one `state.get`, read only when an item needs it. An
  unknown source or a zero canvas is left for the engine to reject. MCP clients write
  their own edits, so the protocol's `item.add` doc tells them to send the same
  geometry.
- **Errors are typed, not `anyhow`.** Each failure must map to its own exit code, so
  `CliError` keeps the `ControlError` (and its protocol code) instead of erasing it.
  `jstream-mcp` reports a failed read and a failed write as the same transport error,
  so `mcp` notes failures of its own input and reports those as `CliError::Read`
  ("could not read the input"), not as unwritable output; both exit 1 with the JSON
  code `io`.
- **Tests.** The unit tests drive `mcp` through `run` with in-memory input and output
  (`initialize`, `tools/list`, `tools/call`, a refused connection, a lost one and a
  failing input), and
  `tests/mcp.rs` runs the binary's `mcp` against a real `jstream-server` on a loopback
  port, through a server shutdown, and with an agent that idles past a short handshake
  timeout after `initialize` and still has its session. `jstream-server` and tokio (for the runtime the test
  serves on) are dev-dependencies only; the binary itself gains only `jstream-mcp`.

### Open questions

1. **`jstream-cli mcp`.** Done: [Usage](#usage) describes it.
2. **A persistent mock.** `--mock` starts empty on every run, and a run is one command,
   so through the binary the read commands always show an empty collection (the unit
   tests read a populated `MockEngine`). `--mock mcp` keeps one mock for a whole MCP
   session, which covers trying the tools locally. Should `--mock` also load and save
   its state (a file option) until a real engine can be reached?
3. **The `mock` feature after the WebSocket client.** Keep it as a scripting sandbox,
   or remove it once an engine host serves the WebSocket API.
4. **Exit codes.** The numbering (2 usage, 3 disconnected, 10 and up for protocol
   codes) is provisional until the maintainer confirms it. A refused token exits 3
   with a reason naming HTTP 401; a code of its own may be clearer for scripts.
5. **Defaults.** No default `--url` (the daemon's port is not chosen yet; it listens
   on a free port unless given `--port`) and no default token file (it will be in the
   platform config directory,
   [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence); the daemon creates
   the file at the path it is given, read from the same `JSTREAM_TOKEN_FILE`). Both are
   required until then
   ([jstream-daemon › Design](../jstream-daemon/README.md#design), open questions 1
   and 2).
6. **Reconnecting `mcp`.** After the engine goes away, every tool call that needs it
   fails with `disconnected` until the MCP client restarts the proxy. The proxy could
   instead open a new session on the next call; that session gets a new `client_id`
   and an empty undo history, which an agent would need to be told about
   ([jstream-control › Design](../../crates/jstream-control/README.md#design) leaves
   reconnecting to the caller). Is that worth doing, or is a restart by the MCP client
   enough?
