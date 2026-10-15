<!-- @doc
id = "CRATE-jstream-ui"
title = "jstream-ui"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-ui

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-ui/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-ui"]
verified = { hash = "b3:5412cbf5a35d49d0", by = "agent:claude@session_01BL6HM3DJQvGExjdE4Z1JCd", note = "B7 merge of main: re-read; only test edits in canvas/model.rs gained opacity/blend None; contents unchanged; holds" }
-->

The web UI: one Rust app, compiled to WASM, for both the desktop app and the browser
([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). It is built with Leptos and rendered
client-side. The daemon serves it over HTTP and the desktop app loads it in a Tauri
webview. It is a client like the CLI and the MCP server: it holds a `dyn Control` from
`jstream-control` and never links the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)). Layer 3: it builds on `jstream-control`
(layer 2) and `jstream-protocol`.

Contents (the skeleton; [Design](#design) explains the choices):
- **App** (`app.rs`): `App`, the root component. It shows the connect form until a
  session is open, then the shell with its regions, and follows the engine's revision
  from `state.changed` events. `SessionContext` gives components the open session.
- **Host detection** (`host.rs`): `desktop()`, the engine URL and token the desktop
  app hands the page through the `__JSTREAM_DESKTOP__` global, if the page runs there.
- **Session** (`session.rs`): `Session` (the connection and the engine's `welcome`),
  `Session::connect` and `Session::open` (`hello` with the client name `jstream-ui`),
  and `engine_url_for_page`, the engine URL the connect form offers.
- **Panels** (`panels/`): the scenes and sources tree, the inspector and the mixer.
  Placeholders for now, except `outputs.rs`: `Outputs`, the outputs panel under the
  inspector (creating, editing and deleting outputs, their stream keys, start and stop,
  go-live and the pipeline's health), and the calls and logic behind it, tested
  natively against `MockEngine`.
- **Canvas** (`canvas/`): the canvas editor. It draws the program scene's item boxes
  over a placeholder background, edits their geometry with the pointer and the
  keyboard, and undoes and redoes ([Canvas](#canvas)). The preview stream is not
  hooked up yet.
- **Entry point** (`main.rs`): `start()`, which mounts `App` on the page's `<body>`;
  `index.html`, `style.css` and `Trunk.toml` are the page trunk builds around it
  ([Build](#build)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-ui/src/**", "crates/jstream-ui/style.css"]
depends_on = ["docs/adr/0045-leptos-web-ui.md", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:244289af1aa12b8a", by = "agent:claude@session_01BL6HM3DJQvGExjdE4Z1JCd", note = "B7 merge of main: re-read; unaffected by the test change; holds" }
-->

- **Connecting.** The connect form asks for the engine URL and the control token. The
  URL defaults to the page's own host with the API path (`ws://` for an `http:` page,
  `wss://` for an `https:` one), because the daemon serves the UI from the port the API
  is on. The token stays in memory: the page never stores it or writes it to the
  console, and the field is a password field. It may be left empty when the daemon
  served the page behind HTTP Basic with the token as the password: the browser's
  cached Basic credentials then carry the token on the upgrade, and no token
  subprotocol is sent. A remote browser will pair with a one-time code instead
  ([ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)); that isn't built yet. In the desktop app the form is skipped: the
  app defines `window.__JSTREAM_DESKTOP__` (`{ url, token }`) before the page loads,
  and `App` connects to that engine at once (`host.rs`). That connection carries the
  token in the subprotocol, which `jstream-server` accepts
  ([jstream-server › Design](../jstream-server/README.md#design)). After a disconnect the form
  shows, without the token; reloading the page connects again. A failed connect shows the transport's reason;
  a browser doesn't say why an upgrade was refused, so a wrong token, a refused origin
  and an unreachable engine read the same.
- **The session.** `Session::connect` opens `jstream-control`'s `WsControl` and says
  `hello`. The open session is provided as Leptos context (`SessionContext`), so a
  panel calls the engine with `session.control.call(…)` and never sees the socket. The
  session is `Rc`-shared: everything in the page runs on one thread, and on `wasm32`
  `Control` needs no `Send`.
- **Events.** One task reads the session's event stream and keeps the status bar's
  revision current. When the stream ends (the engine closed the connection) the UI goes
  back to the connect form with the reason. Disconnecting drops the session, which
  closes the socket. Panels open their own `events()` subscriptions as they need them.
- **The shell** is a CSS grid: a status bar on top, the scenes tree on the left, the
  preview canvas in the middle, the inspector on the right and the mixer along the
  bottom; narrow windows stack them. Each region is its own component, so the panels
  and the canvas are built separately without touching the shell. Colours follow the
  system's light or dark scheme.
- **Outputs.** The panel reads the outputs and encoder profiles with `state.get`, then
  each output's `output.status`, and re-reads them on `state.changed` and
  `events.dropped`; `output.state_changed` updates one output's state in place. It
  creates RTMP(S) and recording outputs (IDs `output-N`, the first free one), changes
  an RTMP server URL (`output.set_target`) and the encoder profile, deletes a stopped
  output, and starts and stops each. "Go live" starts every stopped or failed RTMP
  output whose stream key is set, and "End stream" stops the running ones; recordings
  have only their own buttons. Both try every output, even after one fails, and report
  each failure. An RTMP output without a key can't be started from the
  panel. The health line sums `stats.tick`'s drop totals over the last five ticks; a
  `seq` that doesn't increase or a total that shrinks (an engine restart) starts it
  over.
- **The stream key is write-only** ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). Its
  password field is not bound to a signal: the panel reads it only on save, wraps it in
  a `SecretString` at once (trimming a pasted newline), empties the field and sends
  `output.set_key`. It never stores, logs or displays the key, and the engine never
  sends it back: the panel shows only `output.status`'s `key_set`, re-read after it
  sets or clears a key, since no event reports that. Another client's key change shows
  on the next re-read.
- **Browser password managers are out of scope.** The stream key and the control
  token are secrets for the OS credential store, not for the browser's
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)), so their fields opt out: each is
  `autocomplete="off"` with a name that doesn't look like a credential, and neither
  sits in a `<form>`: a button click or Enter saves the key or connects, so there is
  no form submission for the browser to offer saving. A test checks both fields.
- **Native builds.** The crate is a workspace member, so `cargo clippy` and
  `cargo test` build it natively. Its logic is tested there against `MockEngine`
  (`session.rs`); only `start()` needs a browser.

<a id="canvas"></a>
## Canvas
<!-- @section
covers = ["crates/jstream-ui/src/canvas/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0032-live-edits-and-undo.md"]
verified = { hash = "b3:34cdcf2817be36d5", by = "agent:claude@session_01BL6HM3DJQvGExjdE4Z1JCd", note = "B7 merge of main: re-read; model.rs change is test-only (ItemAdd's new optional fields); canvas behaviour unchanged; holds" }
-->

The canvas edits the boxes of [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)
in canvas pixels. `transform.rs` maps the page to the canvas, hit-tests boxes and
handles, and turns a drag into a new geometry; `snap.rs` snaps; `model.rs` reads the
scene and sends the edits. These three are pure and tested natively against
`MockEngine`; `mod.rs` is the component.

- **What it shows.** The scene on program (`studio.get`, or the first non-group scene
  if none is), re-read on every `state.changed`, `studio.changed` and
  `events.dropped`. Boxes are outlined with `jstream-protocol`'s placement formula, so
  the overlay matches what the compositor draws. An item still stored as a 0.3 legacy
  placement is not drawn: its box depends on the source's runtime size, which the
  protocol doesn't report yet (ADR-0022's `source.sizes`), and the toolbar counts
  such items. Hidden items are drawn dashed.
- **Selecting and dragging.** A press selects the topmost box under the pointer, or
  clears the selection on empty canvas. Dragging the box moves it. The selected box
  has eight resize handles and a rotate handle above its top edge. A corner keeps the
  box's aspect ratio (Shift frees it) and an edge handle changes one side; the
  opposite corner or edge stays put, in the box's own rotated frame, and a box never
  gets smaller than 1 pixel or flips. Rotation turns about the centre and snaps to
  the nearest multiple of 90° within 4°; Shift moves it in 15° steps. Arrow keys
  nudge the selected box by 1 pixel (Shift: 10). Escape cancels a drag, or clears the
  selection.
- **Snapping.** A moved box's edges and centre snap, within 6 screen pixels, to the
  canvas's edges and centre lines and to the other boxes' edges and centres, using
  each box's axis-aligned bounds; the lines it lands on are drawn as guides. An
  upright box's resized edges snap the same way. Alt skips snapping for one drag, and
  a toolbar toggle turns it off.
- **Undoable edits.** A drag changes nothing in the engine until it ends: the overlay
  draws the gesture locally, and on release one batch with one `item.set_geometry`
  commits it, so each gesture (and each nudge) is one undo step. It carries no
  `expected_rev`: a drag is last writer wins per field
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)). Live values
  (`live.set`), which would show the drag on the output while it happens, are not in
  the protocol yet. Undo and redo (toolbar buttons, Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y
  while the canvas has focus) are `history.undo` and `history.redo`; the buttons grey
  out when the engine reports nothing left, and an `undo_conflict` is explained.
- **Styles** are classes in `style.css`. The hosts' Content Security Policy has no
  `'unsafe-inline'` for styles, so a `style` attribute would be refused, static or set at
  run time (`style=move || …`, `attr:style`); a style computed at run time uses a Leptos
  `style:` binding, which sets it through the CSSOM. A test fails on any `style`
  attribute in the crate's sources outside test modules, or in `index.html`.

<a id="build"></a>
## Build
<!-- @section
covers = ["crates/jstream-ui/index.html", "crates/jstream-ui/Trunk.toml", "crates/jstream-ui/Cargo.toml", "crates/jstream-ui/src/main.rs"]
depends_on = ["docs/adr/0045-leptos-web-ui.md"]
verified = { hash = "b3:755c311fa103cf2c", by = "agent:claude@session_01WbL3WRbcjCtYjwjoicNdxV", note = "Merge of main into U2: dropped 'until jstream-server accepts the token subprotocol'; trunk-served page still needs an extra origin; holds" }
-->

The UI is built with [trunk](https://github.com/trunk-rs/trunk) for
`wasm32-unknown-unknown` (a target `rust-toolchain.toml` already installs):

```sh
cd crates/jstream-ui
trunk build            # debug, into dist/
trunk build --release  # what the daemon and the desktop app ship
trunk serve            # rebuilds on change and serves dist/ locally
```

trunk needs the `wasm-bindgen` CLI at the version `Cargo.lock` resolves for
`wasm-bindgen`. It uses one found on `PATH`, and otherwise downloads it; a release build
also downloads `wasm-opt`. `dist/` holds `index.html`, the JavaScript glue, the `.wasm`
and the stylesheet, with content hashes in their names. It is not committed. trunk's
inline loader script carries a `{{__TRUNK NONCE__}}` placeholder (`create_nonce` in
`Trunk.toml`), which the serving engine replaces with each response's
Content-Security-Policy nonce.

CI's `wasm` job builds the UI this way on every PR, with trunk and the `wasm-bindgen`
CLI downloaded at pinned versions and checked against pinned SHA-256 sums, and runs
clippy for `wasm32` over `jstream-control` (with `ws`) and `jstream-ui`.

A page served by trunk is on a different port from the engine, so the engine refuses
its `Origin` unless it is configured as an extra origin.
