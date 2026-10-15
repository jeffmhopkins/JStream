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
verified = { hash = "b3:98fe955f3ee621ae", by = "agent:claude@session_01FnBvDaHWEbSca763xGBa1z", note = "U3a: contents list app routing, store.rs (Studio, Collection, Selection, call helpers) and the scenes, sources, properties and confirm panels; re-read against the code, holds" }
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
- **Session** (`session.rs`): `Session` (the connection and the engine's `welcome`),
  `Session::connect` and `Session::open` (`hello` with the client name `jstream-ui`),
  and `engine_url_for_page`, the engine URL the connect form offers.
- **Store** (`store.rs`): `Studio`, the state every panel shares: the collection as
  `state.get` last reported it (`Collection`), the program scene, the `Selection`, the
  source kinds and the kind descriptors read so far, and the last failed call. Also the
  call helpers (`query`, `apply`, `plan_delete`), `plan_summary` and `fresh_id`.
- **Panels** (`panels/`): the scene list (`scenes.rs`), the sources list
  (`sources.rs`), the properties form (`properties.rs`), and the delete confirmation
  they share (`confirm.rs`). The mixer is a placeholder for now.
- **Canvas** (`canvas/`): the preview canvas. A placeholder for now.
- **Entry point** (`main.rs`): `start()`, which mounts `App` on the page's `<body>`;
  `index.html`, `style.css` and `Trunk.toml` are the page trunk builds around it
  ([Build](#build)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-ui/src/**", "crates/jstream-ui/style.css"]
depends_on = ["docs/adr/0045-leptos-web-ui.md", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:47d8eeec8a98d65f", by = "agent:claude@session_01FnBvDaHWEbSca763xGBa1z", note = "U3a: events feed Studio (coalesced state.get, studio.changed), selection fix-up, scene and source panels (scene.switch, delete.plan with expected_rev, canvas-covering geometry), descriptor-generated properties form, inspector_pages routing; re-read against the code and a browser smoke run, holds" }
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
- **Events and the shared state.** One task reads the session's event stream. It keeps
  the status bar's revision current and hands each event to `Studio`, which re-reads
  the state with `state.get` after a `state.changed` newer than its copy (or after
  `events.dropped`), and follows `studio.changed` for the program scene. Reads coalesce:
  while one `state.get` is in flight, any number of further changes cost one more read.
  Panels never edit the copy: they send edits and the copy follows from the
  `state.changed` that comes back, so every client's change shows the same way. When
  the stream ends (the engine closed the connection) the UI goes back to the connect
  form with the reason. Disconnecting drops the session, which closes the socket.
- **Selection.** `Selection` is the scene being edited (shown by the sources list, and
  by the canvas once it draws) and an item in it (edited by the properties form). It
  is fixed up after every change: a deleted scene falls back to the program scene, then
  the first listed scene, and an item that left the scene is dropped.
- **Scenes and sources.** The scene list shows scenes in display order and leaves out
  groups (ADR-0024), marks the program scene, and puts a scene on program with
  `scene.switch`. The sources list shows the selected scene's items with the top of the
  stack first. New scenes, sources and items get IDs from `fresh_id`, which includes
  the session's client ID so two UIs on one engine don't pick the same one. "Add"
  creates a source of a kind `kind.list` reports, or places an existing source or
  another scene (a `scene` source), as one batch, with a geometry covering the canvas
  (ADR-0022). Deleting a scene, or a source everywhere, reads `delete.plan` first,
  shows what else it takes, and sends the plan's edits expecting the plan's revision,
  so a change in between makes the engine refuse it rather than delete more than the
  user saw. Removing an item from its scene is a plain `item.remove`.
- **The properties form** is generated from the kind's descriptor (`kind.describe`,
  read once per kind and kept), with no code per kind (ADR-0025): a checkbox, number
  (and slider when bounded), select, colour, text, asset (root and path) or reference
  select per parameter type, in the descriptor's order, grouped by `group`, advanced
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
- **Native builds.** The crate is a workspace member, so `cargo clippy` and
  `cargo test` build it natively. Its logic is tested there against `MockEngine`
  (`session.rs`, `store.rs`) and in plain functions the panels are built on (the rows,
  what "Add" sends, the form's sections, parsing and checks); only the components
  themselves need a browser.

<a id="build"></a>
## Build
<!-- @section
covers = ["crates/jstream-ui/index.html", "crates/jstream-ui/Trunk.toml", "crates/jstream-ui/Cargo.toml", "crates/jstream-ui/src/main.rs"]
depends_on = ["docs/adr/0045-leptos-web-ui.md"]
verified = { hash = "b3:7fb8f22f98aca2b6", by = "agent:claude@session_01FnBvDaHWEbSca763xGBa1z", note = "U3a: Cargo.toml gains serde_json only; trunk build --locked --offline with the pinned trunk and wasm-bindgen still succeeds; text holds" }
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
