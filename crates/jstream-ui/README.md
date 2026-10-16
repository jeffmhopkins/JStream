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
-->

The web UI: one Rust app, compiled to WASM, for both the desktop app and the browser
([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). It is built with Leptos and rendered
client-side. The daemon serves it over HTTP and the desktop app loads it in a Tauri
webview. It is a client like the CLI and the MCP server: it holds a `dyn Control` from
`jstream-control` and never links the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)). Layer 3: it builds on `jstream-control`
(layer 2) and `jstream-protocol`.

Contents ([Design](#design) explains the choices):
- **App** (`app.rs`): `App`, the root component. It shows the connect form until a
  session is open, then the shell with its regions, and keeps the shared state current
  from the session's events. It owns the routing: which panel sits in which region, and
  the inspector's pages. `SessionContext` gives components the open session.
- **Host detection** (`host.rs`): `desktop()`, the engine URL and token the desktop
  app hands the page through the `__JSTREAM_DESKTOP__` global, if the page runs there.
- **Session** (`session.rs`): `Session` (the connection and the engine's `welcome`),
  `Session::connect` and `Session::open` (`hello` with the client name `jstream-ui`),
  and `engine_url_for_page`, the engine URL the connect form offers.
- **Store** (`store.rs`): `Studio`, the state the scene, sources and properties panels
  share: the collection as `state.get` last reported it (`Collection`, with its buses
  for the bus picker), the program
  scene, the `Selection`, the source kinds and the kind descriptors read so far, and the
  last failed call. Also the call helpers (`query`, `apply`, `list_kinds`, `plan_delete`),
  `plan_summary` and `fresh_id`.
- **Panels** (`panels/`): the scene list (`scenes.rs`), the sources list
  (`sources.rs`), the properties form (`properties.rs`) and the delete confirmation
  they share (`confirm.rs`); `outputs.rs`: `Outputs`, the outputs panel, an inspector
  page (creating, editing and deleting outputs, their stream keys, start and stop,
  go-live and the pipeline's health), and the calls and logic behind it, tested
  natively against `MockEngine`; and `mixer.rs`: `Mixer`, the audio mixer along the
  bottom (faders, mutes, routing and buses, with meters and the master's loudness),
  also tested natively against `MockEngine`.
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
  a browser doesn't say why an upgrade was refused, so a wrong token, a refused origin,
  an unreachable engine and a URL the page's policy blocks read the same.
- **The session.** `Session::connect` opens `jstream-control`'s `WsControl` and says
  `hello`. The open session is provided as Leptos context (`SessionContext`), so a
  panel calls the engine with `session.control.call(…)` and never sees the socket. The
  session is `Rc`-shared: everything in the page runs on one thread, and on `wasm32`
  `Control` needs no `Send`.
- **Events and the shared state.** One task reads the session's event stream. It keeps
  the status bar's revision current and hands each event to `Studio`, which re-reads
  the state with `state.get` after a `state.changed` newer than its copy (or after
  `events.dropped`), and follows `studio.changed` for the program scene. Reads coalesce:
  while one `state.get` is in flight, any number of further changes cost one more read.
  Panels never edit the copy: they send edits and the copy follows from the
  `state.changed` that comes back, so every client's change shows the same way. When
  the stream ends (the engine closed the connection) the UI goes back to the connect
  form with the reason. The loop reads the session from the context per event and
  holds no handle of its own, so Disconnecting drops the last one, which closes the
  socket.
- **Selection.** `Selection` is the scene being edited (shown by the sources list) and
  an item in it (edited by the properties form). A new session reads the program scene
  before the state, so it starts on the scene on program. It is fixed up after every
  change: a
  deleted scene falls back to the program scene, then the first listed scene, and an
  item that left the scene is dropped. The canvas still draws the program scene with a
  selection of its own; moving it onto `Selection` is a canvas follow-up.
- **Scenes and sources.** The scene list shows scenes in display order and leaves out
  groups (ADR-0024), marks the program scene, and puts a scene on program with
  `scene.switch`. The sources list shows the selected scene's items with the top of the
  stack first. New scenes, sources and items get IDs from `fresh_id`, which includes
  the session's client ID so two UIs on one engine don't pick the same one. "Add"
  creates a source of a kind `kind.list` reports, or places an existing source or
  another scene (a `scene` source), as one batch, with a geometry covering the canvas
  (ADR-0022). An engine that lists no kinds (the mock) offers no new kinds, only
  existing sources and scenes. A `kind.list` that fails does the same, and its
  error shows as any failed call's does. Deleting a scene, or a source everywhere, reads `delete.plan` first,
  shows what else it takes, and sends the plan's edits expecting the plan's revision,
  so a change in between makes the engine refuse it rather than delete more than the
  user saw. Removing an item from its scene is a plain `item.remove`.
- **The properties form** is generated from the kind's descriptor (`kind.describe`,
  read once per kind and kept), with no code per kind (ADR-0025): a checkbox, number
  (and slider when bounded), select, colour, text, asset (root and path) or reference
  select (of the scenes, the other sources, or the buses, with None for `null`, which
  for a `waveform`'s or `spectrum`'s `audio` is the program mix) per parameter type, in the descriptor's order, grouped by `group`, advanced
  ones collapsed, and hidden while their `visible_if` doesn't hold. A type this UI
  doesn't know is shown read-only. A change is checked with the descriptor's own check,
  then sent as a one-parameter `source.set_settings` patch, and Reset sends `null` for
  the default. The descriptor's or the engine's reason shows under the field. A
  `scene` source has no settings.
- **Routing.** `app.rs` decides the regions: the scene and sources lists on the left,
  the canvas in the middle, the inspector on the right, and the mixer along the
  bottom. The inspector's pages are `inspector_pages()`, one line each, shown as tabs
  once there is more than one. A failed call shows in a dismissable bar.
- **The shell** is a CSS grid: a status bar on top, the scene and sources lists on the left, the
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
- **Mixer** ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)). The panel reads
  the audio sources (those `state.get` reports with `audio`), the buses and the sends
  into a copy of its own, not `Studio`'s, and re-reads them on `state.changed` and
  `events.dropped`. Each source gets a strip,
  ordered by name: its meter, fader, mute and, folded under "Routing", whether its
  audio is on, when it is heard (`always` or `when_shown`) and its send into each bus
  (on or off, and its gain). A nested scene's strip has only the routing toggles,
  since it has no fader or sends of its own. Each bus gets a strip too, the master
  first, with renaming and (except the master) deleting, which the engine refuses
  (the mixer shows its reason, and the strip stays) while a `waveform` or `spectrum`
  draws the bus; the master's adds its
  momentary, short-term and integrated loudness, its true peak, and a button that
  resets the integrated loudness (`audio.reset_loudness`). New buses take the first
  free `bus-N` ID; the button is off at the protocol's bus limit (`MAX_BUSES`, 16).
  - **Faders** run from −100 to +24 dB on a console taper: unity at three quarters
    of the travel, the quiet end compressed, and snapping to 0 dB within a quarter of
    a dB. While the pointer holds a fader the strip shows the value under it; letting
    go (or each key press) sends one `source.set_audio` or `bus.set_fader`, so a drag
    is one undo step. Hearing the fader during the drag needs live values
    ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), not in the protocol
    yet. Clicking the readout sets 0 dB. An accepted edit is applied to the shown
    state at once, so the strip doesn't jump back before the re-read.
  - **Meters** take `meters.tick`: an RMS bar over a fainter peak bar per channel,
    from −60 dBFS to full scale, and a peak hold that waits about 1.5 s, then falls
    about 3 dB a tick, and turns red at full scale. A source the tick leaves out loses
    its reading, and half a second without a tick (no output runs, so the mixer is
    stopped) blanks every meter.
- **Browser password managers are out of scope.** The stream key and the control
  token are secrets for the OS credential store, not for the browser's
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)), so their fields opt out: each is
  `autocomplete="off"` with a name that doesn't look like a credential, and neither
  sits in a `<form>`: a button click or Enter saves the key or connects, so there is
  no form submission for the browser to offer saving. A test checks both fields.
- **Native builds.** The crate is a workspace member, so `cargo clippy` and
  `cargo test` build it natively. Its logic is tested there against `MockEngine`
  (`session.rs`, `store.rs`, `panels/outputs.rs`, `panels/mixer.rs`) and in plain functions the panels are built on (the rows,
  what "Add" sends, the form's sections, parsing and checks); only the components
  themselves need a browser.

<a id="canvas"></a>
## Canvas
<!-- @section
covers = ["crates/jstream-ui/src/canvas/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0032-live-edits-and-undo.md"]
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

The built page is tested in a browser by CI's `ui-e2e` job: Playwright against a mock
daemon ([end-to-end tests](tests/e2e/README.md)).
