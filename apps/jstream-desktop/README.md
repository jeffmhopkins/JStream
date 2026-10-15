<!-- @doc
id = "CRATE-jstream-desktop"
title = "jstream-desktop"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-desktop

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["apps/jstream-desktop/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*"]
verified = { hash = "b3:30dcae94297a3b25", by = "agent:claude@session_012JH6K3TesVLzsmqwabzmYW", note = "P1: data.rs removed, contents list says logs.rs and the paths module; release build of the portable folder now cargo xtask dist" }
-->

The desktop app ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)): a
[Tauri](https://v2.tauri.app/) window that shows the web UI (`jstream-ui`), with a
`jstream-core` `Engine` running in the same process. The window's UI talks to the engine
over `jstream-server`'s authenticated WebSocket API on `127.0.0.1`, the same one a
browser or the CLI uses, so the UI has one code path
([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). It ships as `jstream.exe` in the
portable folder ([ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)).
Layer 6, an app that links core and server
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut; Windows first, because the window needs WebView2):
- **Startup** (`app.rs`, `main.rs`): the command line, the log file, then the engine,
  its server and the UI bundle. Anything that fails becomes the page the window shows.
- **Engine host** (`host.rs`): the engine (on Windows a live one, wired as the daemon's
  is) and its server on a free port, with a token made fresh at each start.
- **UI scheme** (`ui.rs`): serves the UI bundle from a folder to the webview, or the
  page that says why the app couldn't start.
- **Bootstrap script** (`bootstrap.rs`): hands the page the engine's URL and token.
- **Window** (`shell.rs`, Windows only): the Tauri window, its navigation guard and
  shutdown.
- **Log file** (`logs.rs`), in the data folder `jstream-platform`'s `paths` module finds.
- **Tests** (unit tests in each module, run on every platform: the server behind the
  token and the webview's origin, the bundle's paths, types and CSP, the bootstrap
  script's escaping and the capped log file).

Not here yet: a window on Linux and macOS (the binary says to use the daemon), the
virtual camera's registration mode (`jstream.exe --register-vcam`, which
`jstream-daemon` provides for now), a native dialog when WebView2 is missing, crash
reports, and MCP and the token for outside clients. `cargo xtask dist` builds the
portable folder ([ARCH › Packaging](../../docs/ARCHITECTURE.md#packaging)).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-desktop/src/main.rs", "apps/jstream-desktop/src/app.rs", "apps/jstream-desktop/src/logs.rs", "apps/jstream-desktop/Cargo.toml"]
verified = { hash = "b3:ea77775577946bf9", by = "agent:claude@session_012JH6K3TesVLzsmqwabzmYW", note = "P1: covers drops the deleted data.rs (the data folder is jstream-platform paths now, documented in its Purpose); the Data folder bullet links there and holds" }
-->

On Windows, with the UI built first:

```sh
cd crates/jstream-ui && trunk build --release && cd ../..
cargo run -p jstream-desktop -- --ui-dir crates/jstream-ui/dist
```

The binary is `jstream` (`jstream.exe`). It opens a window on the UI and connects it to
its engine at once, with no connect form: `jstream-server` takes the token from the
WebSocket subprotocol the UI offers ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)).
If the connection is refused, the window shows the connect form with that error.
Closing the window stops the engine.

| Option | Meaning |
|---|---|
| `--ui-dir <folder>` | The UI bundle (`trunk build`'s `dist`). Default: the `ui` folder beside the exe. |

- **Data folder.** `%APPDATA%\JStream`, the daemon's too. When a file named `portable`
  sits beside the exe, a `data` folder beside the exe instead, so the app can run from
  a USB stick ([ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)). Both
  hosts find it through `jstream-platform`'s `paths` module
  ([jstream-platform › Purpose](../../crates/jstream-platform/README.md#purpose)).
- **The collection** is `collection.json` in the data folder, created empty the first
  time and saved on every change, so the next start opens where the last one stopped.
- **Logs.** `logs\jstream-desktop.log` in the data folder, replaced at each start and
  capped at 10 MiB, filtered by `RUST_LOG` (default `info`). A debug build also logs to
  its console; a release build has no console window. The audit log goes in the same
  folder, as the daemon's does. The token is never logged.
- **When it can't start** (no data folder, Media Foundation missing, the collection
  file unreadable, no UI bundle), the window shows what went wrong and the log has the
  details. An unknown command-line argument is shown the same way.
- **Going live** works as in the daemon: create an RTMP(S) output, set its stream key
  (kept in Windows Credential Manager) and start it.
- **Elsewhere** the binary prints that the app runs on Windows only for now and exits 1.

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-desktop/src/**", "apps/jstream-desktop/build.rs", "apps/jstream-desktop/tauri.conf.json"]
depends_on = ["docs/adr/0045-leptos-web-ui.md", "docs/adr/0020-portable-windows-distribution.md", "docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-002"]
verified = { hash = "b3:d8c3940849482bc8", by = "agent:claude@session_012JH6K3TesVLzsmqwabzmYW", note = "Merge of main into P1: main added the daemon e2e test, the CLAP plugin-proto row and plugin-host status; P1's data-folder/paths and dist text is unchanged and both hold together" }
-->

- **The engine behind the API.** The engine is served on `127.0.0.1`, on a port the OS
  picks, behind a 32-byte random token made at each start and kept only in memory: no
  token file, and no other client can connect. MCP is off. The server accepts the
  webview's origin besides its own (R-SEC-002); every connection still presents the
  token. On Windows the engine is live with the same wiring as the daemon (OS backends,
  keys in Credential Manager, an RTMP(S) publish opener); elsewhere it has no backends.
  When the app exits, the server closes its connections and the engine stops.
- **The UI is read at run time.** The app registers its own URL scheme, `jstream`, and
  serves the bundle's files from a folder on each request, so the binary embeds nothing
  and the build doesn't need the bundle. WebView2 gives the scheme the origin
  `http://jstream.localhost` (other webviews `jstream://localhost`). Paths are checked
  segment by segment: percent-decoded, no `..`, `\`, `:` or
  NUL, and the canonical file must be inside the canonical folder, so no path or link
  reaches outside it. Each response has a `Content-Security-Policy`: the bundle's own
  files, WASM compiled from them, inline scripts and styles only with the response's
  nonce (trunk's placeholder, as `jstream-server` fills it), and connections only to
  the bundle and the engine's `ws://127.0.0.1:<port>`.
- **Handing the page the engine.** Before the page's own scripts, a bootstrap script
  defines `window.__JSTREAM_DESKTOP__` as a frozen `{ url, token }` that can't be
  reassigned, and only when the page's origin is the app's. `jstream-ui` checks for it
  at start (its `host.rs`) and connects at once; the browser client offers the token as
  its subprotocol, as in a browser ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)).
  The strings are written as escaped JavaScript literals.
- **The window stays on the app.** Navigation to anything but the app's scheme is
  refused, and so are new windows, so no other page loads in a webview that runs the
  bootstrap script. The page gets no Tauri IPC: the app defines no commands and grants
  no capabilities, so the API is the only way in.
- **Failures are a page, not a crash.** Media Foundation missing (an N edition), no
  data folder, a broken collection file or a missing bundle: the scheme serves a page
  with the reason (HTML-escaped, with a policy that allows nothing) and no bootstrap
  script. A missing bundle is found after the engine started, so the engine is stopped
  first. As in the daemon, the build script delay-loads the Media Foundation DLLs with
  MSVC, so the app starts on a Windows without them and can say so.
- **Windows only, for now.** Tauri is a Windows-only dependency, so Linux CI builds
  and tests everything but the window (`shell.rs`) without GTK or WebKitGTK. Tauri's
  other platforms still appear in `cargo metadata`, so `deny.toml` and the lint-deps
  allowlist list their duplicates and native crates, each marked as never built for
  Windows where that holds. `tauri.conf.json` names the app and its icon; the bundler
  is off (the portable folder is ADR-0020's), and so are Tauri's CSP injection and
  global API. `tauri-build` writes ACL schemas to `gen/`, which is not committed.
- **Logs and the token.** The token reaches only the bootstrap script. It is never
  logged, written to disk, or put in a URL (R-SEC-001).
