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
verified = { hash = "b3:f77129a46f3e8525", by = "agent:claude@session_01SHnfsdCupikc9rN2BdzzeD", note = "U0: browser client connects without a subprotocol when the token is empty (relying on browser credentials such as U2's HTTP Basic); ADR-0045 point 4, control and ui READMEs, server open question 2 updated; re-read, holds" }
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
- **Canvas** (`canvas/`): the preview canvas. A placeholder for now.
- **Entry point** (`main.rs`): `start()`, which mounts `App` on the page's `<body>`;
  `index.html`, `style.css` and `Trunk.toml` are the page trunk builds around it
  ([Build](#build)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-ui/src/**", "crates/jstream-ui/style.css"]
depends_on = ["docs/adr/0045-leptos-web-ui.md", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:5d91e7e926945a0d", by = "agent:claude@session_01SHnfsdCupikc9rN2BdzzeD", note = "Merge of main (B10 #201 program scene and scene.switch, and others) into U0: took main's text and stamp, re-read; main's changes and U0's browser client, token subprotocol and ADR-0045 text both hold; server still accepts only Authorization on main" }
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
