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
covers = ["apps/jstream-desktop/src/**", "apps/jstream-desktop/tests/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*"]
-->

The desktop app ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)): a
[Tauri](https://v2.tauri.app/) window that shows the web UI (`jstream-ui`), with a
`jstream-core` `Engine` running in the same process. The window's UI talks to the engine
over `jstream-server`'s authenticated WebSocket API on `127.0.0.1`, the same one a
browser or the CLI uses, so the UI has one code path
([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). It ships as `jstream.exe` in the
portable folder ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)).
Layer 6, an app that links core and server
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut; Windows first, because the window needs WebView2):
- **Startup** (`app.rs`, `main.rs`): the command line, the log file, then the engine,
  its server and the UI bundle. Anything that fails becomes the page the window shows.
- **Engine host** (`host.rs`): the engine (on Windows a live one, wired as the daemon's
  is, plus the virtual camera) and its server on a free port, with a token made fresh
  at each start (with `--mcp`, on a fixed port with the data folder's token file and MCP
  on, and with `Network` the TLS network listener and pairing); `vcam_status`, whether
  the camera is registered and current.
- **Network access** (`network.rs`): "Allow devices on my network"
  ([ADR-0104](../../docs/adr/0104-network-listener-over-tls.md)), the setting in the
  data folder's `network.json`, the network listener's configuration, and `set_network`,
  the command that saves it.
- **Global hotkeys** (`hotkeys.rs`, wired in `host.rs` on Windows and on an X11 session): hears the bound
  chords while another application has the focus and runs their actions
  ([Design](#design)). Tested with a fake listener on every platform.
- **UI scheme** (`ui.rs`): serves the UI bundle from a folder to the webview, or the
  page that says why the app couldn't start.
- **Update check** (`updates.rs`): answers `GET /jstream-update-check` with GitHub's
  public releases list, or a short error. The page cannot choose the address. The
  request sends no body, token or version. Tested without the network.
- **Bootstrap script** (`bootstrap.rs`): hands the page the engine's URL and token,
  the virtual camera's status, the data folder (so a picked file under it can be
  stored relative to the collection), whether "Allow devices on my network" is on,
  `browse`, which opens a native dialog, and `allowNetwork`, which saves that setting.
- **Browser sources** (`browser.rs`, Windows with the `cef` feature only): the CEF
  backend the engine opens `browser` sources' pages in.
- **Window** (`shell.rs`, Windows only): the Tauri window, its navigation guard, the
  projector windows the UI opens, the links it hands to the system browser (the
  project's pages, and Cisco's OpenH264 licence; `links.rs`, which builds and is
  tested everywhere), shutdown, and the two commands the page may call: a native file,
  save or folder dialog (`pick.rs`), and saving the network setting (`network.rs`).
- **Log file** (`logs.rs`), in the data folder `jstream-platform`'s `paths` module finds:
  a new user-only file at each start, through `create_private_file` on Windows and
  `jstream-crash`'s `create_new` elsewhere.
- **Crash reports**: `jstream-crash`'s panic hook, which writes a redacted report beside
  the log file (none for a panic on a thread in `jstream_core::HANDLED_PANIC_THREADS`,
  an effect-pack load the engine catches and logs, #999), and its `Redacting` key store wrapper, which registers each stream key
  with the redaction.
- **Tests** (unit tests in each module, run on every platform: the server behind the
  token and the webview's origin, the bundle's paths, types and CSP, the bootstrap
  script's escaping, the command line, the virtual camera's status values, the
  capped log file (user-only, checked on Unix), the update check's request and the
  replies it gives a releases list, a rate limit and a malformed message, the network
  setting (off until saved on, user-only, off when malformed), the network listener and
  pairing it turns on, and the control token registered for redaction; the crash hook's own
  tests are `jstream-crash`'s), and one integration test, `tests/delay_load.rs` (Windows MSVC
  only), which reads the built `jstream.exe`'s PE headers to check that each Media
  Foundation DLL `build.rs` delay-loads is delay-loaded and none is a load-time import.

Not here yet: a window on Linux and macOS (the binary says to use the daemon), the
virtual camera's optional registration and unregistration modes (`jstream.exe --register-vcam`, which
`jstream-daemon` provides for now), a native dialog when WebView2 is missing, native crash
dumps (only a panic writes a report, as in the daemon), renewing the network
certificate from the app (the daemon's `--lan-renew` renews the same file), and browser sources in a default build
(the `cef` feature is off). `cargo xtask dist` builds the portable folder
([ARCH › Packaging](../../docs/ARCHITECTURE.md#packaging)).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-desktop/src/main.rs", "apps/jstream-desktop/src/app.rs", "apps/jstream-desktop/src/logs.rs", "apps/jstream-desktop/Cargo.toml"]
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
| `--no-vcam` | Run without the virtual camera: a `virtual_cam` output can't start. |
| `--mcp` | Let agents in: listen on `127.0.0.1:4455` with the data folder's `token` file, and serve MCP at `/mcp` (see **Agents** below). Off by default. |
| `--port <port>` | With `--mcp`, the port instead of 4455. Without `--mcp` it is refused. |

**Browser sources** need the `cef` feature (`cargo run -p jstream-desktop --features
cef`) and the sandboxed browser helper in the `browser` folder beside `jstream.exe` (or
next to it, in a build folder): CEF's bootstrap exe renamed `jstream-browser-helper.exe`,
`jstream-browser-helper.dll` and CEF's runtime files
([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md),
[the helper's README](../jstream-browser-helper/README.md#purpose)). The app itself
doesn't build or link CEF, so it starts without them; a `browser` source fails to open
until they are there. The feature is off by default, because only Windows has the
backend; the portable zip turns it on and ships the helper
([ARCH › Packaging](../../docs/ARCHITECTURE.md#packaging)). Without it, a `browser`
source draws nothing. CEF's cache is the data folder's `cef`
folder, and its log `logs\cef.log`.

- **Data folder.** `%APPDATA%\JStream`, the daemon's too. When a file named `portable`
  sits beside the exe, a `data` folder beside the exe instead, so the app can run from
  a USB stick ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)). Both
  hosts find it through `jstream-platform`'s `paths` module
  ([jstream-platform › Purpose](../../crates/jstream-platform/README.md#purpose)).
- **The collection** is the project opened most recently, from
  `recent-collections.json` in the data folder, if it still loads, and otherwise
  `collection.json` there, created the first time with nothing in it but the
  prototype's four sample labels. It is saved on every change, so the next start opens
  where the last one stopped. A client switches projects with `collection.open`,
  `collection.new` and `collection.save_as`
  ([ADR-0121](../../docs/adr/0121-projects-open-new-and-save-as.md)); the publish opener
  reads a stream key under the open project's name for it (`Engine::stream_key_name`).
- **Effect packs** are the pack folders in the data folder's `effects`, the folder the
  daemon watches by default: the engine loads them at start and reloads them when they
  change (`Engine::watch_effect_packs`,
  [jstream-core › Design](../../crates/jstream-core/README.md#design)).
- **The workspace layout** (the dock and saved layouts) is kept in `ui-settings.json` in
  the data folder, the file the daemon keeps it in too, so the window and a browser of
  the daemon show the same layout
  ([ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md)).
- **Logs.** `logs\jstream-desktop.log` in the data folder, replaced at each start by a
  new, user-only file (a protected DACL for SYSTEM and the user on Windows, mode `0600`
  on Unix, in a folder created `0700`, #780) and capped at 10 MiB, filtered by `RUST_LOG` (default `info`). A debug build also logs to
  its console; a release build has no console window. The audit log goes in the same
  folder, as the daemon's does. The token is never logged.
- **Crash reports.** A panic writes `logs\crash-<UTC time>-<pid>-<n>.txt` in the data
  folder; the newest 10 are kept. An effect pack whose shader panics the validator
  writes none (unless its load thread can't start): it is logged as a pack that failed
  to load. Nothing is uploaded
  ([R-SEC-004](../../docs/RULES.md#R-SEC-004)).
- **When it can't start** (no data folder, Media Foundation missing, the collection
  file unreadable, no UI bundle), the window shows what went wrong and the log has the
  details. An unknown command-line argument is shown the same way.
- **Agents (`--mcp`).** The app then lets outside clients in as the daemon does
  ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)): it listens on
  `127.0.0.1:4455` (or `--port`), takes its token from `token` in the data folder,
  created user-only the first time and reused after
  (`jstream_server::token_file`, [jstream-server › Purpose](../../crates/jstream-server/README.md#purpose)),
  and serves MCP's streamable-HTTP transport at `http://127.0.0.1:4455/mcp`. An MCP client
  launches `jstream-cli.exe --url ws://127.0.0.1:4455 mcp` from the same folder with
  `JSTREAM_TOKEN_FILE` naming that file, or posts to `/mcp` with the token as a bearer
  token ([Getting started › Connect an agent](../../docs/GETTING-STARTED.md#agents)). The
  window uses the same token. The log names the endpoint and the token file, never the
  token. A port in use stops the start with a page that says to pass `--port`. Pairing
  stays off unless network access is on.
- **Devices on the network.** Settings › Devices › "Allow devices on my network" (off
  until turned on) saves `network.json` in the data folder; it applies at the next
  start. On, the app also listens on every interface on port 4456 (the daemon's
  `--lan` port), TLS only, admitting paired devices only
  ([ADR-0104](../../docs/adr/0104-network-listener-over-tls.md)), and pairs devices:
  Settings › Devices then opens a pairing code and shows the addresses to open and the
  certificate's fingerprint to compare. The certificate (`token.tls`, made the
  first time) and the paired devices (`token.devices`) are kept beside the `token` file, the daemon's
  files, so the two share them. When the port is in use (a daemon with `--lan`), the
  app starts without network access, the log says why, and Settings › Devices says
  devices can't connect. Windows may ask to let JStream through its firewall the first
  time, and on a network marked Public it blocks other devices. Turning the setting off
  applies at the next start and keeps the paired devices, which work again if it is
  turned back on: revoke them on the same page to end their access for good.
- **Going live** works as in the daemon: create an RTMP(S) output, set its stream key
  (kept in Windows Credential Manager) and start it.
- **The virtual camera.** Add a "Virtual camera" output in the outputs panel and start
  it: other apps then see the program as the webcam *JStream Virtual Camera*. The
  camera must be registered once, with `jstream-daemon --register-vcam` from the same
  folder (one administrator prompt); the panel says whether it is, as the app read it
  at start, so restart the app after registering. `--no-vcam` turns the camera off.
- **Elsewhere** the binary prints that the app runs on Windows only for now and exits 1.

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-desktop/src/**", "apps/jstream-desktop/build.rs", "apps/jstream-desktop/media_foundation.rs", "apps/jstream-desktop/tests/delay_load.rs", "apps/jstream-desktop/tauri.conf.json"]
depends_on = ["docs/adr/0045-leptos-web-ui.md", "docs/adr/0066-portable-windows-distribution-v2.md", "docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-002"]
-->

- **The engine behind the API.** The engine is served on `127.0.0.1`, on a port the OS
  picks, behind a 32-byte random token made at each start and kept only in memory: no
  token file, and no other client can connect. MCP is off. With `--mcp`
  (`host::Agents`) the server binds `127.0.0.1` on a fixed port (`--port`, 4455 by
  default), takes the token from
  the data folder's token file through `jstream_server::token_file`, the daemon's
  code, so it has the same checks (user-only, refused otherwise), and turns MCP on
  (`ServerConfig::mcp`), so agents and the CLI connect with that file. The server accepts the
  webview's origin besides its own (R-SEC-002); every connection still presents the
  token. The window needs no pairing, so unless "Allow devices on my network" is on the
  server pairs no device (`ServerConfig::pairing` off): `pairing.start`, `device.list`
  and `device.revoke` answer `unsupported`, `jstream-ui`'s pairing panel says no device
  can pair, and there is no pairing page or device cookie
  ([ADR-0047](../../docs/adr/0047-browser-pairing-page-and-device-cookie.md)). The
  server still answers `ui_settings.get` and `ui_settings.set`, keeping them in the data
  folder's `ui-settings.json` (`Host::start`'s `ui_settings`,
  [ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md)). On Windows the engine is live with the same wiring as the daemon (OS backends,
  keys in Credential Manager, an RTMP(S) publish opener); elsewhere it has no backends.
  Everywhere it watches the data folder's `effects` for effect packs
  (`Engine::watch_effect_packs`) and starts the engine's stats heartbeat, so the header's readout shows the
  engine's load while nothing is live, and on Windows it hands the engine
  `WindowsProcessCpu` for that readout's CPU share, `WindowsThreadCpu` for
  `profile.get`'s threads and `WindowsThreadPriority` for the render and mixer threads'
  MMCSS tasks ([ADR-0117](../../docs/adr/0117-real-time-video-thread-model.md) §8).
  When the app exits, the server closes its connections and the engine stops.
- **Network access** (`network.rs`, [ADR-0104](../../docs/adr/0104-network-listener-over-tls.md)
  decision 1). The setting is `{"allow": true}` in the data folder's `network.json`,
  written user-only and replaced whole; a missing, unreadable or malformed file is off.
  Only the person changes it: the page calls `allowNetwork`, which invokes the Tauri
  command `set_network`, granted to the main window by the `network` capability and to
  nothing else. It is not a UI setting (`ui_settings.set`), because any engine client
  (an agent with `--mcp`, a paired device) can write those, and widening who can reach the
  engine is the person's choice. It is read once at start, so a change applies at the
  next start. On, `Host::start` gets a `Network`: `ServerConfig::network` on
  `0.0.0.0:4456` with the certificate from `jstream_server::identity` (made the first
  time, kept when its names are out of date, with a warning in the log, never renewed
  silently), `device_file` beside the token file, and pairing on, so the loopback
  listener (which the window uses) opens codes and `pairing.start` names the network
  listener's addresses and fingerprint (protocol 0.97). A certificate that can't be
  used is logged and the app starts without network access. Any other failed start with
  the listener (most likely its port in use) is logged and retried without it; a cause
  that isn't the listener's fails the retry too, with its own error. The page is told
  whether the listener opened (`networkOpen`), so Settings › Devices says when the
  setting is on but devices can't connect.
- **Hotkeys while another app has the focus.** On Windows and on Linux under X11 the host hears the
  collection's bound chords while another application has the focus
  ([ADR-0112](../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5 and §6, #1188).
  `hotkeys.rs` opens its own session on the engine (`jstream-desktop hotkeys` in the
  audit log), reads the bindings from `state.get` and reads them again after each
  `state.changed`, so a chord bound or cleared from any client is heard or let go at
  once. `jstream_platform_windows::GlobalHotkeys` registers exactly those chords with
  `RegisterHotKey` on its own thread, and on Linux `jstream_platform_linux::GlobalHotkeys`
  grabs them on the X root window with `xcb_grab_key`, so the app never sees any other
  key; a press
  runs the binding's action as an `action` call from that session, and nothing it
  hears is kept or sent anywhere else. A registered chord goes to JStream even while
  its window has the focus, so the page doesn't run it twice. A chord another
  application holds already is logged and heard only while JStream has the focus. The
  UI setting `hotkeys.global` turns it off (`false`; on otherwise), read through
  `Server::ui_settings`; the UI's Settings dialog shows it in the app. Every chord is
  let go when it is turned off and when the app stops. On a Wayland session the Linux
  listener doesn't start (Wayland has no grab; its GlobalShortcuts portal isn't wired
  yet), and the app has no Linux window yet ([Purpose](#purpose)), so the X11 listener
  runs once one lands. Wheel chords (ADR-0112 §3), the Wayland portal and the macOS
  listener aren't built yet.
- **The UI is read at run time.** The app registers its own URL scheme, `jstream`, and
  serves the bundle's files from a folder on each request, so the binary embeds nothing
  and the build doesn't need the bundle. WebView2 gives the scheme the origin
  `http://jstream.localhost` (other webviews `jstream://localhost`). Paths are checked
  segment by segment: percent-decoded, no `..`, `\`, `:` or
  NUL, and the canonical file must be inside the canonical folder, so no path or link
  reaches outside it. `GET /jstream-update-check` is not a file: the app answers it
  itself, on another thread, with GitHub's public releases list or an error
  (`updates.rs`). Each response has a `Content-Security-Policy`: the bundle's own
  files, WASM compiled from them, inline scripts and styles only with the response's
  nonce (trunk's placeholder, as `jstream-server` fills it), and connections only to
  the bundle and the engine's `ws://127.0.0.1:<port>`. The update check is that same
  origin, so the page never connects to GitHub itself.
- **Handing the page the engine.** Before the page's own scripts, a bootstrap script
  defines `window.__JSTREAM_DESKTOP__` as a frozen object that can't be reassigned,
  and only when the page's origin is the app's. `jstream-ui` checks for it
  at start (its `host.rs`) and connects at once; the browser client offers the token as
  its subprotocol, as in a browser ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)).
  The strings are written as escaped JavaScript literals. The global carries
  `url`, `token` and `vcam`, the virtual camera's status as the app read it at start:
  `registered`, `not_registered`, `outdated`, `off` (`--no-vcam`) or `unavailable`
  (not Windows). The UI uses `vcam` only to say whether the camera can start (its
  outputs panel); the engine still decides. It also carries `collection`, the data
  folder, `network`, whether "Allow devices on my network" was on at start,
  `networkOpen`, whether its listener then opened, and two functions. `browse` calls the app's command `pick_path` and resolves to the
  filesystem path the person picked, or null if they cancelled or the dialog failed.
  `allowNetwork(allow)` calls `set_network` and resolves to whether the setting was
  saved, false when the command fails too (see *Network access*).
- **The virtual camera.** On Windows the engine's backends carry `WindowsVcam` unless
  the app was started with `--no-vcam`; creating it opens nothing, and its `start`
  refuses an unregistered camera. `vcam_status` reads the registration with no
  elevation (`vcam_registration`, which compares the registered administrators-only
  copy with the `jstream_vcam_windows.dll` beside the exe by content) and logs a
  warning when it is outdated; it never registers or prompts
  ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)). Registering,
  which is optional, stays the daemon's `--register-vcam` for now, and so does the
  hardware check. The build script also
  delay-loads `mfsensorgroup.dll`, which `WindowsVcam` imports `MFCreateVirtualCamera`
  from, as the daemon's does, so a Windows without it still starts the app; on a
  Windows without that export (Windows 10), `WindowsVcam::start` probes for it first,
  so `output.start` fails with a reason rather than a delay-load exception.
- **The window stays on the app.** Navigation to anything but the app's scheme is
  refused, so no other page loads in a webview that runs the bootstrap script. A new
  window opens only for one of the UI's projector pages (`?projector=`, the UI's
  `projector.rs`): another window of the app with the same rules and script, starting
  on `about:blank` as a window handed back to `window.open` must, placed and sized as
  the page asked, and full screen when it asked for that (`&full=1`). A link to one of
  the project's own pages on GitHub (`links.rs`: the repository's URL from the
  workspace's `repository`, or a path, query or fragment under it written only with
  ASCII letters, digits and `/ # ? . _ = & -`, as the UI's Help menu, Check for updates
  and About dialog link to), or to Cisco's OpenH264 licence at exactly
  `https://www.openh264.org/BINARY_LICENSE.txt` (the link in About and in Outputs ›
  Encoders, #1165), opens in the system browser, through Explorer with the URL as one
  argument, and never in the window. So a page can make the app start one OS process,
  Explorer, for those URLs only; the Cisco URL is that one string, and no comma,
  backslash, quote, space or percent escape can reach Explorer's command line. Every
  other new window is refused, and a
  projector can open none. The page gets two Tauri commands, `pick_path` (`pick.rs`)
  and `set_network` (`network.rs`), and no other. `build.rs` declares them to
  `tauri-build`, which writes `allow-pick-path` and `allow-set-network` under
  `permissions/autogenerated/` (gitignored, as `gen/` is). The capabilities `browse`
  and `network` grant those permissions to the main window only, and do not grant
  `core:default`. A projector has no path fields or settings, so it is granted neither.
  The command opens a
  common-item dialog owned by the calling window and returns the path the person
  picked. It does not read the file. The UI calls it only through `browse` on the
  global. A browser of the daemon has no global, so it has no button and no command.
  The engine is still reached only over the WebSocket API. The one other request the
  scheme answers is the update check, which ignores whatever the page sends and always
  reads the same releases list.
- **Failures are a page, not a crash.** Media Foundation missing (an N edition), no
  data folder, a broken collection file or a missing bundle: the scheme serves a page
  with the reason (HTML-escaped, with a policy that allows nothing) and no bootstrap
  script. A missing bundle is found after the engine started, so the engine is stopped
  first. As in the daemon, the build script delay-loads the Media Foundation DLLs with
  MSVC, so the app starts on a Windows without them and can say so. The list
  (`mfplat.dll`, `mf.dll`, `mfreadwrite.dll`, `mfsensorgroup.dll`) is in
  `media_foundation.rs`, which `build.rs` and `tests/delay_load.rs` both include. That
  test reads the built `jstream.exe`'s import tables, as the daemon's does: each listed
  DLL must be in the delay-import table and none in the import table. So CI's Windows
  test job fails when `/DELAYLOAD` stops applying, or when a listed DLL is no longer
  imported (the linker only warns, LNK4199, which doesn't fail CI). A new Media
  Foundation import listed nowhere still gets past it.
- **Windows only, for now.** Tauri is a Windows-only dependency, so Linux CI builds
  and tests everything but the window (`shell.rs`, `pick.rs`) and `tests/delay_load.rs` without
  GTK or WebKitGTK. Tauri's other platforms still appear in `cargo metadata`, so
  `deny.toml` and the lint-deps allowlist list their duplicates and native crates,
  each marked as never built for Windows where that holds. `tauri.conf.json` names the
  app and its icon; the bundler is off (the portable folder is ADR-0066's), and so are
  Tauri's CSP injection and global API. `tauri-build` writes ACL schemas to `gen/`,
  which is not committed.
- **Browser sources, behind a feature.** With `cef`, the engine gets a CEF backend
  (`jstream-browser`) on the backends' media clock, so pages' audio is stamped on the
  clock the mixer runs on. Its pages paint onto a device of the media adapter the app
  opens for them (`WindowsGpu::open_device`) until a program stage runs; the engine then
  lends the backend the stage's own context in its place, so the mix draws the pages,
  and takes it back when the stage stops or leaves the GPU path mid-run, after which
  the pages deliver no frames for the rest of that stage. The backend is added after the OS backends, so sources open on the media
  clock. CEF runs only in the sandboxed browser helper, which the backend starts on the
  first page ([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)); the app
  doesn't link CEF, so a missing helper or missing CEF files fail that source, not the
  app. With no device, pages play audio but show no frames.
- **Logs and the token.** The token reaches only the bootstrap script. It is never
  logged, written to disk, or put in a URL (R-SEC-001).
- **Crash reports** ([`jstream-crash`](../../crates/jstream-crash/README.md#purpose),
  [R-SEC-004](../../docs/RULES.md#R-SEC-004)), as the daemon writes them
  ([jstream-daemon › Design](../jstream-daemon/README.md#design)): both apps use the one
  crate ([ADR-0067](../../docs/adr/0067-shared-crash-reports.md)). `main.rs` makes the
  `Secrets` list, passes it on through `app::start` to the host, and installs the panic hook as soon as it has the data folder, before the
  log file, into the `logs` folder (created if missing). A report holds the app and
  protocol versions, OS and architecture, the UTC time, the process ID, the thread
  name, the panic's location and message (cut at 16 KiB) and a backtrace; never the
  environment, the command line or settings. The control token (registered by
  `Host::start`) and every stream key the live engine stores or reads (its Credential
  Manager store is behind `jstream_crash::Redacting`) are replaced by `***` in the report and
  in its stderr line (R-SEC-001). Only their length and BLAKE3 digest are kept. The
  hook replaces the default one, whose stderr line would quote the message
  unredacted; `main.rs`'s `crash_line` prints the hook's redacted line instead. Each report is user-only: mode `0600` on Unix, and on Windows a protected DACL for
  SYSTEM and the user (`jstream_platform_windows::create_private_file`, passed to
  `jstream_crash::install_creating`, #656). Before each report the oldest beyond 10 are deleted; the log file and
  the audit log in the same folder are left alone. A native crash writes nothing yet
  (the daemon's open question 4 holds here too).
