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
verified = { hash = "b3:f3b946b7d870c961", by = "agent:claude@session_01PE5EjidrHZa45cD9qxQTvL", note = "U4: canvas entry now describes the canvas editor (geometry edits, undo, placeholder background, no preview yet); re-read the rest, holds" }
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
- **Session** (`session.rs`): `Session` (the connection and the engine's `welcome`),
  `Session::connect` and `Session::open` (`hello` with the client name `jstream-ui`),
  and `engine_url_for_page`, the engine URL the connect form offers.
- **Panels** (`panels/`): the scenes and sources tree, the inspector and the mixer.
  Placeholders for now.
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
verified = { hash = "b3:4c33d809769b5b68", by = "agent:claude@session_01PE5EjidrHZa45cD9qxQTvL", note = "U4: re-read against the canvas editor; it opens its own events() subscription as the section says, keeping only the stream so disconnecting still closes the socket; shell grid and styles unchanged; holds" }
-->

- **Connecting.** The connect form asks for the engine URL and the control token. The
  URL defaults to the page's own host with the API path (`ws://` for an `http:` page,
  `wss://` for an `https:` one), because the daemon serves the UI from the port the API
  is on. The token stays in memory: the page never stores it or writes it to the
  console, and the field is a password field. It may be left empty when the daemon
  served the page behind HTTP Basic with the token as the password: the browser's
  cached Basic credentials then carry the token on the upgrade, and no token
  subprotocol is sent. How the daemon hands the token to a
  browser (pairing) is not decided yet. A failed connect shows the transport's reason;
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
- **Native builds.** The crate is a workspace member, so `cargo clippy` and
  `cargo test` build it natively. Its logic is tested there against `MockEngine`
  (`session.rs`); only `start()` needs a browser.

<a id="canvas"></a>
## Canvas
<!-- @section
covers = ["crates/jstream-ui/src/canvas/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0032-live-edits-and-undo.md"]
verified = { hash = "b3:5168bee338266e3b", by = "agent:claude@session_01PE5EjidrHZa45cD9qxQTvL", note = "U4: new section; checked against transform.rs, snap.rs, model.rs and mod.rs (handles, aspect, rotate snap 90/4 deg and 15 deg steps, 6 px snapping, one item.set_geometry batch per gesture without expected_rev, history calls, legacy items not drawn); behaviour exercised in Chromium against the daemon" }
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
- **Styles** are inline, since the shell's stylesheet belongs to the shell.

<a id="build"></a>
## Build
<!-- @section
covers = ["crates/jstream-ui/index.html", "crates/jstream-ui/Trunk.toml", "crates/jstream-ui/Cargo.toml", "crates/jstream-ui/src/main.rs"]
depends_on = ["docs/adr/0045-leptos-web-ui.md"]
verified = { hash = "b3:1ea0cbddc4084706", by = "agent:claude@session_01SHnfsdCupikc9rN2BdzzeD", note = "U0: new section; trunk commands, wasm-bindgen CLI matching Cargo.lock, create_nonce placeholder and the CI wasm job (pinned, checksummed trunk and wasm-bindgen, clippy wasm32) match Trunk.toml, index.html and ci.yml" }
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
its `Origin` unless it is configured as an extra origin; and until `jstream-server`
accepts the token subprotocol ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)), the
UI connects only to an engine that does.
