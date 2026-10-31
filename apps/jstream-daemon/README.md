<!-- @doc
id = "CRATE-jstream-daemon"
title = "jstream-daemon"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-daemon

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["apps/jstream-daemon/src/**", "apps/jstream-daemon/tests/**", "apps/jstream-daemon/Info.plist", "apps/jstream-daemon/build.rs"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*"]
-->

The headless engine host ([ARCH › Shape](../../docs/ARCHITECTURE.md#shape)): it runs a
`jstream-core` `Engine` and serves it through `jstream-server`'s authenticated WebSocket
API on `127.0.0.1`, so `jstream-cli --url …` and the stdio MCP proxy `jstream-cli mcp`
can drive it, and turns on the server's MCP transport over streamable HTTP at `/mcp`, so
an agent can connect without the proxy. Like every host it only wires the engine to its interfaces
([ADR-0001](../../docs/adr/0001-library-first-engine.md)): each connection and each MCP
session gets its own `InProcessControl` session through the server's `Connector`. Layer 6, an app that links
core and server ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut, the M1a host of [ROADMAP › M1](../../ROADMAP.md#m1)):
- **Startup** (`main.rs`): parses the command line, creates the log directory, installs
  the crash-report hook and the log subscriber (stderr and the log files, filtered by
  `--log-level` or `RUST_LOG`), opens the audit log, loads or creates the token, runs
  the tokio runtime,
  prints where it listens, and maps failures to exit codes.
- **Command line** (`args.rs`): a hand-rolled, pure parser ([Usage](#usage)).
- **YouTube** (`youtube.rs`): gives the engine YouTube with JStream's built-in OAuth
  client ([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)), or with a developer's client from `--youtube-client` or the
  environment ([Design](#design)).
- **Token file** (`token.rs`): reuses the token in an existing user-only file, or
  creates the file with a new random token, and names the device file beside it
  ([Design](#design)).
- **Serving** (`serve.rs`): builds the engine (with the audit log, the `--collection`
  file, the plugin folder and the placeholder outputs; on Windows a live one, through `live.rs`, that scans
  the CLAP plugins, and with the `cef` feature also opens browser sources, and on
  macOS a live one on the macOS backends built so far, [Design](#design)), installs the signal handlers that stop it, then binds the server
  with MCP on and, with `--ui-dir`, the web UI's files.
- **Media Foundation check** (`main.rs`, `components.rs`, Windows only): exits with
  advice when Windows lacks Media Foundation or its AAC encoder, as N editions do
  ([Design](#design)).
- **Virtual camera registration** (`register.rs`, Windows only): the optional
  `--register-vcam` registers an administrators-only copy of the media source DLL
  shipped beside the daemon, `--unregister-vcam` removes it, and startup only logs the
  registration's state ([Design](#design)).
- **Live engine wiring** (`live.rs`, in the crate's library target `lib.rs`): the
  backends, the stream key store and the publish opener that make an engine live
  ([Design](#design)). It is a library so the integration tests build the daemon's own
  wiring on the platform fakes; nothing else depends on it.
- **Build script** (`build.rs`): delay-loads the Media Foundation DLLs on Windows
  (MSVC), so that check can run at all. On macOS it embeds `Info.plist`, the camera,
  microphone and system audio recording usage strings, in the binary's
  `__TEXT,__info_plist` section (ADR-0063 §11), so macOS can show those permission
  prompts without an app bundle. Its bundle identifier is
  io.github.jeffmhopkins.jstream.daemon<!--fact:macos-daemon-bundle-id-->, under the
  app ID io.github.jeffmhopkins.jstream<!--fact:macos-app-id-->. A daemon started from
  a terminal is attributed to the terminal app, which then needs the grant.
- **Log files** (`logs.rs`): the data directory, and the size-capped rotating log files
  ([Design](#design)).
- **Crash reports**: `jstream-crash`'s panic hook, which writes a redacted report into
  the log directory ([Design](#design)).
- **Tests** (`serve.rs`, `token.rs`, `args.rs` and `logs.rs` unit tests, the token
  registered for redaction, and the stream key registered through the live engine's
  `Redacting` store; `set_up` registering the stored YouTube token through the setup's
  secret hook, in `youtube.rs`; the real `connect_rtmp` failing against loopback servers without
  naming the key, through `serve::live_redacting`, in `serve.rs`; the publish opener and
  the live engine on the platform fakes, in `live.rs`; `tests/daemon.rs`, the built binary driven by
  `jstream-control`'s WebSocket client, a raw HTTP MCP client and raw HTTP UI
  requests, and stopped with
  SIGINT; `tests/e2e.rs`, the headless end to end on every OS: the engine as the
  daemon makes it live, on the platform fakes with the CPU compositor, behind the
  server with MCP on, where MCP sets the canvas to the fake display's size, builds a
  scene and creates an RTMP output to a local
  loopback sink and a recording, the WebSocket client sets the key and starts the
  stream, MCP starts the recording, and the test checks `stats.tick`, what the sink
  received and the recorded MP4. The binary has no backends on Linux, so it runs
  in process, on the library's `live` and `rtmp`, the wiring `serve.rs` uses;
  `tests/delay_load.rs`, Windows MSVC only, which reads the built binary's PE headers to check that the
  backends' Media Foundation DLLs are delay-loaded and none is a load-time import;
  `tests/info_plist.rs`, which checks on every OS that `Info.plist` has each usage
  string and the bundle identifier `docs/facts.toml` names, and on macOS that the built
  binary carries it).

Not here yet: YouTube sign-in for users (it waits on JStream's own OAuth client,
[ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)), YouTube outside Windows (it needs an OS credential store the daemon has
only there), a default collection file, a
default port and a default token file
location ([Design › Open questions](#open-questions)).

Why a crate: it is a host binary of its own (ADR-0066 ships `jstream-daemon.exe` next
to the desktop app), and only an app may link both `jstream-core` and `jstream-server`
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)).

<a id="usage"></a>
## Usage
<!-- @section
covers = ["apps/jstream-daemon/src/args.rs", "apps/jstream-daemon/src/main.rs", "apps/jstream-daemon/src/token.rs"]
-->

```sh
jstream-daemon --token-file ~/.config/jstream/token --port 4460 --output out_1
# token file: /home/me/.config/jstream/token (created, new token)
# mcp endpoint: (the same port, path /mcp)
# listening on ws://127.0.0.1:4460/api/v1/ws
JSTREAM_TOKEN_FILE=~/.config/jstream/token jstream-cli --url ws://127.0.0.1:4460 scene list
```

| Option | Meaning |
|---|---|
| `--token-file <path>` | The control token file. Default: the `JSTREAM_TOKEN_FILE` environment variable, which `jstream-cli` reads too. One of them is required. |
| `--port <port>` | The port on `127.0.0.1`. Default `0`: the OS picks a free one, and the URL line says which. |
| `--collection <path>` | Loads the collection from this JSON file and saves every change to it, so the next run starts where this one stopped. Created, holding the empty collection, if it doesn't exist. Default: none, the collection starts empty and is lost on exit. |
| `--plugin-dir <dir>` | Where installed WASM plugins and their grants are saved (`plugins.json`) and loaded from at startup, created if missing ([ADR-0076](../../docs/adr/0076-saved-wasm-plugins-and-manifest-requests.md)). A file there that can't be read, or that a newer version wrote, is logged at `error` and left as it is, and plugins aren't loaded or saved that run. Default: `plugins` in the platform data directory; without a data directory, plugins aren't saved. |
| `--ui-dir <dir>` | Serves the web UI's built files (`trunk build`'s `dist`) from this directory at `http://127.0.0.1:<port>/`. The browser asks for a user name and password: the password is the token (see **Web UI** below). A path that isn't a directory fails with exit code 1, before the token or collection file is written. Default: no UI. |
| `--output <id>` | Adds a stopped placeholder output (repeatable), so `output start`, `output stop` and `output status` have something to act on. On Windows and macOS it has no target, so starting it fails: create outputs with `output.create` instead. |
| `--log-dir <dir>` | Where the log files, crash reports and the audit log go, created (with its parents) if missing. Default: `logs` in the platform data directory (see **Log files** below). |
| `--log-level <spec>` | The log filter, in `RUST_LOG`'s syntax, which it overrides. |
| `--youtube-client <path>` | Windows, for developers: a Google OAuth client to use for YouTube sign-in instead of JStream's built-in one, the JSON Google Cloud gives for a "Desktop app" client (see **YouTube** below). Default: the `JSTREAM_YOUTUBE_CLIENT_ID` and `JSTREAM_YOUTUBE_CLIENT_SECRET` environment variables, else the built-in client. |
| `--register-vcam` | Windows, optional: registers the virtual camera for this machine from an administrators-only copy, then exits (see **Virtual camera** below). Takes no other options. |
| `--unregister-vcam` | Windows: removes the virtual camera's registration and its copy, then exits (see **Virtual camera** below). Takes no other options. |
| `-V`, `--version` | Prints the daemon and protocol versions. |
| `-h`, `--help` | Prints the help. |

- **The token file.** If it exists, its token (the file's text without surrounding
  whitespace) is reused, so clients keep working across restarts. If it doesn't, it is
  created with a new random token and a newline. An existing file that is empty, holds
  no valid token (visible ASCII, no spaces), is not a regular file, or that other users
  may access (on Unix, any group or other permission, mode bits `077`; on Windows, an
  owner other than you, SYSTEM or Administrators, or a DACL letting anyone else in), is
  refused with exit code 1, and left as it is. A new file is user-only on both.
  The parent directory must exist. The token itself is never printed.
- **Paired devices.** Devices paired with `jstream-cli pair`
  ([ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)) are kept in
  `<token file>.devices` beside the token file (for example `token.devices`): their
  names and token digests, never a token, user-only (mode `0600` on Unix, a protected
  DACL for SYSTEM and the user on Windows). It is created at the first pairing, so
  devices stay paired across restarts; deleting it unpairs them all. A device file that
  can't be read or parsed, or that other users may access (on Unix, any group or other
  permission; on Windows, another owner or a DACL entry letting anyone but the user,
  SYSTEM or Administrators in), is a startup failure (exit 1). A device token works only while the daemon listens on
  `127.0.0.1`, which it always does for now.
- **Output.** On stdout: `token file: <path> (created, new token)` or
  `… (existing token)`, then `mcp endpoint: http://127.0.0.1:<port>/mcp` (point an MCP
  client there, with the token as `Authorization: Bearer <token>`), then
  `ui: http://127.0.0.1:<port>/` with `--ui-dir`, then
  `listening on ws://127.0.0.1:<port>/api/v1/ws` once it
  serves (a script waits for this line), and `stopped` after a graceful stop. Errors go
  to stderr as `error: <message>`.
- **Web UI.** With `--ui-dir`, open the `ui:` address in a browser on the same
  machine. It asks for a user name and password: enter anything as the user name and
  the token file's contents as the password. The browser keeps them for the session
  and sends them on the UI's WebSocket connection too
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)).
- **Logs.** The libraries' log lines go to stderr, never stdout, and to log files:
  connections accepted and refused (with the reason, never the token), handshakes,
  output actions, and how connections end. `--log-level`, else `RUST_LOG`, filters
  both with `target=level` directives and an optional bare default level, for example
  `--log-level jstream_core=debug,info`; unset or empty means `info`. A value that
  doesn't parse is a usage error (exit 2), reported before anything is created.
- **Log files.** Each run writes `jstream-daemon-<UTC time>-<pid>-<n>.log` in the log
  directory, and starts a new file when one would pass 10 MiB; the newest 10 log files
  are kept and older ones deleted, so the logs stay under about 100 MiB. The default
  directory is `logs` in the data directory: `%APPDATA%\JStream` on Windows,
  `~/Library/Application Support/JStream` on macOS, `$XDG_DATA_HOME/jstream` (else
  `~/.local/share/jstream`) elsewhere, or a `data` folder beside the exe when a file
  named `portable` sits there (ADR-0066). At `info` and above, the first line logged
  names the file. A log directory that can't be created, or a data directory that can't
  be found (no portable marker, and no `HOME` or `APPDATA`) without `--log-dir`, is a startup failure (exit 1), before the token
  file is touched. On Unix the directories it creates are mode `0700` and the files
  `0600`; on Windows the files (logs and crash reports, which `jstream-crash` makes through
  `logs::create_new`, #656) are created with a protected DACL for SYSTEM and the user,
  and the directories keep what they inherit.
- **Crash reports.** A panic writes `crash-<UTC time>-<pid>-<n>.txt` into the log
  directory and prints `crash report: <path>` to stderr; the newest 10 are kept.
  Nothing is uploaded ([R-SEC-004](../../docs/RULES.md#R-SEC-004)): attach the report
  to a bug report yourself. What it holds is in [Design](#design).
- **Stopping.** Ctrl-C (SIGINT) or SIGTERM on Unix, Ctrl-C or Ctrl-Break on Windows:
  it stops accepting connections, closes the open ones (their clients see
  `Disconnected`, and event streams end) and exits 0. Without `--collection` the
  engine's state is lost.
- **YouTube (Windows).** Streaming to YouTube needs no sign-in: use **Going live**
  below with YouTube Studio's RTMPS URL and stream key. Signing in, for the
  `youtube.*` calls, uses JStream's own OAuth client
  ([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)). This build's built-in client is still
  empty, so `youtube.status` answers `unavailable` and the startup log says so. With a
  client (the built-in one, or a developer's below), `youtube.status` answers
  `signed_out` and `youtube.sign_in` returns Google's consent URL; once approved, the
  account's refresh token is kept in Credential Manager, so a restart stays signed in. The `youtube.*` calls
  are a person's tools, over the WebSocket API: MCP has no `youtube` tool ([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md) §3).
  A developer can test with their own Google Cloud OAuth client of type "Desktop
  app", with the YouTube Data API v3 enabled: pass its downloaded JSON with
  `--youtube-client`, or set `JSTREAM_YOUTUBE_CLIENT_ID` (and
  `JSTREAM_YOUTUBE_CLIENT_SECRET`, if it has one). Never commit it. Off Windows,
  YouTube is `unavailable`. A client file that can't be read or holds no `client_id`,
  or a secret set without an ID, is a startup failure (exit 1); the message names the
  file but never its contents or the secret
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).
- **Going live (Windows and macOS).** `output.create` with an RTMP(S) server URL, then
  `output.set_key` (stored in Windows Credential Manager, or the login keychain on
  macOS), then `output.start`
  connects and streams. A start fails with `action_failed` and a reason that says what
  to fix when the output has no target (a `--output` placeholder), no stream key, or
  the key can't be read. Elsewhere, `output.start` only steps the placeholder state
  machine and `output.set_key` fails: there is no key store.
- **Media Foundation (Windows).** On a Windows without Media Foundation (an N or KN
  edition without the Media Feature Pack, or Windows Server without its "Server Media
  Foundation" feature), the daemon logs the problem, prints `error: Media Foundation
  is not installed: <what to install>` and exits 3, after creating the log directory
  and before touching the token file. If Media Foundation is there but has no AAC
  encoder, it does the same with `error: Media Foundation's AAC encoder is not
  installed: …`. Listing the codecs fails with exit 1.
- **Virtual camera (Windows).** Registering the camera is optional, and only these
  two flags do it or ask for elevation
  ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)).
  `jstream-daemon --register-vcam` copies the `jstream_vcam_windows.dll` beside the
  daemon to `%ProgramFiles%\JStream\vcam\`, a folder only administrators can write to,
  and registers that copy machine-wide, as the Windows camera Frame Server requires. The
  DLL in the daemon's own folder is never registered. From a normal prompt it shows one
  UAC prompt and runs itself elevated, without a window. It prints `virtual camera:
  registered <copy>, a copy of <dll>`, adding the old path when something else was
  registered, or `virtual camera: already registered (<copy>)` without asking for
  elevation when the copy already has this DLL's content. After an update whose DLL
  changed, running it again replaces the copy: a new opt-in, with its own prompt.
  `jstream-daemon --unregister-vcam` removes the registration and the copy's folder,
  with one prompt, or prints `virtual camera: not registered` without one. A copy the
  camera still has loaded can't be deleted: the registration is removed, it exits 1
  and says to close the camera's apps and run it again. A missing DLL, a declined
  prompt or a failed step exits 1; from an administrator prompt the error says what
  failed. At startup the daemon only reports: a warning when the registration names a
  DLL outside that folder or a copy whose content differs from this DLL's, saying
  registering again is optional, and an `info` line when there is none. Elsewhere both
  flags fail with exit 1.
- **Collection file.** With `--collection`, the engine loads the file at startup
  (upgrading an older version) and replaces it atomically after every applied batch,
  undo and redo, before the call returns. A file that can't be read, holds no
  collection the engine can load, or can't be created is a startup failure (exit 1)
  naming the file and what is wrong, and the file is left as it is. A save that fails
  later is logged at `error` and retried with the next change; the call still
  succeeds. Undo history is not saved. Nothing stops two daemons from using the same
  file: each saves over the other's changes, so give each its own.
- **Plugin folder.** Installed WASM plugins are saved in `plugins.json` in
  `--plugin-dir`, by default `plugins` in the platform data directory, and each that was
  enabled is loaded again at startup, once the engine is built. A plugin that no longer
  loads stays disabled, and the log says why. Unlike the collection file, a plugins file
  the engine can't read doesn't stop the start: it is logged and left as it is. As with
  the collection file, nothing stops two daemons from using the same folder, which the
  default makes likely: both run the saved plugins and each saves over the other's
  changes, so give each its own `--plugin-dir`. No command removes an installed plugin
  yet.
- **Audit log.** Every batch, undo, redo and action, by any client, is one JSON line
  in `audit.jsonl` in the log directory: the time, the client's ID and `hello` name
  (`device:<name>` for a paired device),
  the call and its edits by name, the output an action names, the `rev` before and
  after, and the result code. Never a call's arguments, so never a stream key
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). When it would pass 8 MiB it becomes
  `audit.1.jsonl`, replacing the one before. Both files are created user-only (mode
  `0600` on Unix, a protected DACL for SYSTEM and the user on Windows), and so is each
  save of the collection file. A log that can't be opened is a startup failure (exit 1),
  and so, on Windows, is an existing `audit.jsonl` whose owner or DACL lets anyone but
  you, SYSTEM or Administrators in; the error says why.

| Exit code | Meaning |
|---|---|
| 0 | Stopped by a signal, printed the help or version, or registered or unregistered the virtual camera (`--register-vcam`, `--unregister-vcam`) |
| 1 | Failed: `--register-vcam` or `--unregister-vcam` could not finish (or isn't on Windows), the log directory, a log file or the audit log can't be created, the collection file can't be read, loaded or created, the YouTube client file can't be read or holds no client, the token file is unusable or can't be created, the device file is unusable, the media clock can't be opened, or on macOS a backend refuses to open (a release below the floor), the port can't be bound, the `--ui-dir` directory isn't one, the signal handlers or runtime can't be set up, or the server failed while serving |
| 2 | Usage: the command line could not be parsed, names no token file, or `--log-level` or `RUST_LOG` doesn't parse |
| 3 | Windows only: Media Foundation or its AAC encoder is not installed (see **Media Foundation** above) |

<a id="design"></a>
## Design
<!-- @section
covers = ["apps/jstream-daemon/src/**", "apps/jstream-daemon/Cargo.toml", "apps/jstream-daemon/build.rs"]
depends_on = ["crates/jstream-server/README.md#design", "crates/jstream-core/README.md#design", "docs/ARCHITECTURE.md#persistence"]
-->

- **One engine, a session per connection.** `serve.rs` builds one `Engine` and hands
  the server a `Connector` that calls `Engine::connect()`, so every WebSocket client
  gets its own `InProcessControl` (its own `client_id`) on the shared engine, and sees
  every other client's events ([jstream-core › Design](../../crates/jstream-core/README.md#design)).
  The daemon never touches the protocol itself. The connector holds the engine in an
  `Arc`; the server, and through it every session, keep it alive.
- **Live on Windows** (`serve.rs`). Under `cfg(windows)` the engine gets the
  `jstream-platform-windows` backends (`QpcClock`, `WgcCapture`, `MfVideoDevices`,
  `WasapiAudio`, `MfCodecs`, `WindowsGpu`, `MfMediaDecoders`, and `WindowsVcam` as the
  virtual camera, so a `virtual_cam` output's `output.start` publishes the program as a
  webcam; that start fails with the sink's reason on a Windows without
  `MFCreateVirtualCamera` (before Windows 11) or before `--register-vcam`),
  `WindowsCredentials` as its
  stream key store (`with_key_store`; `serve.rs`'s `live_redacting` puts it behind
  `jstream_crash::Redacting` before calling `live`, so each key is registered with the crash reports' redaction, [Crash reports](#design)), and a
  publish opener (`with_publish_opener`).
  Creating the backends opens no device; each opens what it needs when a pipeline
  starts, or for the capture and video-device backends, when a preview shows a display,
  window or webcam (a webcam's one open is shared with the output's). A `jstream-clap-scan` thread then scans the CLAP plugins with
  `WindowsPluginHosts` (`Engine::scan_clap_plugins`), so they appear in `kind.list` as
  audio-filter kinds once found, and logs each file it couldn't scan; the daemon
  serves meanwhile. The engine keeps the hosts, and from then on starts a
  `jstream-plugin-host.exe` for each CLAP filter in an audio chain (a source's, the
  master's or an output's). Without
  `jstream-plugin-host.exe` beside the daemon every file fails to scan, and is logged.
- **Live on macOS** (`serve.rs`). Under `cfg(target_os = "macos")` the engine gets
  the `jstream-platform-macos` backends built so far (`macos_backends`):
  - `MachClock`, `ScreenCaptureKitCapture`, `AvFoundationVideoDevices`, `CoreAudioIo`,
    `VideoToolboxCodecs` and `AvMediaDecoders`;
  - the stand-in `NoGpuInterop`, which sends the engine to its own device and CPU
    frames until the Metal interop does;
  - no virtual camera, no CLAP scan and no YouTube sign-in;
  - `MacosKeychain` as its stream key store, behind `jstream_crash::Redacting` with
    the daemon's `Secrets` through `live_redacting` as on Windows, and the same
    publish opener.

  Capture, webcams and Core Audio refuse a release below the floor when created, which fails
  startup. Creating them asks for no permission: that happens when a pipeline first
  uses them.
- **Elsewhere** (Linux) there are no backends yet, so outputs stay the placeholder
  state machine ([jstream-core › Design](../../crates/jstream-core/README.md#design)).
- **The publish opener.** On each `output.start` it reads the output's server URL with
  `Engine::output_kind` and its key from the store, then calls `connect_rtmp`, so each
  start connects anew. It fails the start with its own reason (`PublishError::new`)
  when the output is not in the collection, is a kind it can't publish, has no key, or
  the store fails; no reason carries the key
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)), and `connect_rtmp`'s errors don't
  either. The engine stores the opener, so the opener holds only a `Weak` to the
  engine's `Arc`, set once the engine is built: a strong handle would be a cycle that
  keeps the engine and its running pipeline alive after the server lets go. The
  opener upgrades it only to read the target. `live` (`live.rs`, in the library
  target) is that wiring over any backends, key store and connect function, and `rtmp`
  the `connect_rtmp` connect function: `serve.rs` passes them the Windows backends and
  Credential Manager (behind `jstream_crash::Redacting`, which the binary adds, so the
  library never sees it), and the tests (`live.rs`'s and `tests/e2e.rs`) the platform
  fakes, so the end-to-end test runs the daemon's wiring, not a copy of it.
- **Media Foundation check and delay-load.** The `windows` crate imports Media
  Foundation's DLLs as load-time imports, so on a Windows without Media Foundation
  the loader would refuse to start the binary, naming a DLL, before `main` runs.
  `build.rs` links the binary (and its unit-test harness, which `link-arg-bins` also
  reaches, but not the integration tests) with `/DELAYLOAD` for each DLL
  the daemon's backends import from (`mfplat.dll` for `MfCodecs`, `MfVideoDevices`
  and `MfMediaDecoders`, `mf.dll` for `MfVideoDevices`, `mfreadwrite.dll` for
  `MfVideoDevices` and `MfMediaDecoders`, `mfsensorgroup.dll` for `WindowsVcam`) and
  `delayimp.lib` when the target is MSVC, so each loads on the first call into it.
  `main.rs` runs `jstream_platform_windows::check_media_foundation` right after
  logging starts, before the engine is built or anything else calls Media Foundation,
  then lists `MfCodecs`' codecs and requires an AAC encoder (`components.rs`, plain
  data in and out, tested everywhere), since every stream's audio needs one. Either
  failure is a `MissingComponent`, mapped to exit code 3 with its advice.
  Only DLLs the binary imports from are listed. The linker ignores one it doesn't with
  warning LNK4199, which doesn't fail CI: rustc reports it through the `linker_messages`
  lint, which ignores `-D warnings`. `tests/delay_load.rs` is what catches a stale entry
  (below). The startup check
  doesn't cover `mfsensorgroup.dll`, since the daemon serves without a camera:
  `WindowsVcam::start` checks that it exports `MFCreateVirtualCamera` before calling
  it, so a Windows 10 gets an `output.start` error, not a delay-load
  exception.
  `tests/delay_load.rs` checks the built binary's import tables: each DLL in its own
  copy of the list must be in the delay-import table and none in the import table. So
  CI's Windows job proves `/DELAYLOAD` applies to the `windows` crate's raw-dylib
  imports, and fails when a listed DLL is no longer imported. It checks only the DLLs
  it lists, so a DLL added to `build.rs` alone, or a new Media Foundation import listed
  nowhere, gets past it.
- **Virtual camera registration** (`register.rs`). `--register-vcam` and
  `--unregister-vcam` are separate modes that exit without serving, so they create no
  logs or token. The copy, DACL and registry work is `jstream-platform-windows`'s
  (`register_vcam`, `unregister_vcam`), because the daemon has no `unsafe`. A
  non-elevated daemon doesn't write: it relaunches itself elevated with the same fixed
  argument (`run_elevated`), then reads the registration back
  (`vcam_registration`, which compares the copy with the shipped DLL by content), so
  success is judged by what the registry and the copy hold rather than by the hidden
  child's output. A registration whose copy already has this DLL's content is left
  alone, so repeating the command never prompts. The startup check (`log_status`) only
  logs, and never prompts: the daemon serves without the camera, and
  `WindowsVcam::start` refuses to start an unregistered camera on its own.
- **YouTube** (`youtube.rs`, `serve.rs`). `youtube::client` reads a developer's OAuth
  client from `--youtube-client`'s file (Google's `{"installed": {…}}` download, or the two
  fields at the top level), else from the environment, holding the secret as a
  `Secret` so it never prints. A JSON error reports only its line and column, because
  serde_json's messages can quote a value. `main` reads it after the audit log opens
  and before the token is touched. `serve.rs` then calls `Engine::with_youtube` with
  that client, else with `YoutubeSetup::built_in` (JStream's client, [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md), `None`
  while it is empty), over `jstream_youtube::HttpsTransport` (the OS's root store) with
  `WindowsCredentials` behind `jstream_crash::Redacting` as the token store (so each refresh
  token is redacted from crash reports, [Crash reports](#design)), before the engine
  goes live; core keeps the
  token under its fixed account ID. Off Windows there is no OAuth token store, so a
  developer's client is dropped with a warning. Neither an empty built-in client nor a
  transport that can't be built stops the daemon: YouTube is optional (ADR-0058).
  `youtube::set_up` is the developer's-client call over any transport and store, which
  is how the test runs it on `jstream-youtube`'s fake.
- **Collection file and audit log** (`serve.rs`, `main.rs`). The daemon only passes
  them to the engine: `Engine::with_collection_file` loads and saves the collection,
  and `with_audit` takes a `jstream_core::AuditFile`, whose writer thread appends
  entries off the engine's locks, so a slow disk never delays a call
  ([jstream-core › Design](../../crates/jstream-core/README.md#design)). The `--output`
  placeholders are declared outputs, which a loaded collection never removes, so they
  are added after the load only for reading order. The audit log lives in the log
  directory, which the daemon already creates (user-only on Unix) and which `--log-dir`
  moves, rather than in a directory of its own. Its entries are written as they come;
  on a graceful stop the writer finishes the queue when the engine is dropped. Without `--collection` nothing is saved, as before
  ([Open questions](#open-questions), 6).
- **Plugin folder** (`main.rs`, `serve.rs`). `main` fills in `--plugin-dir`'s default
  from `jstream_platform::paths::data_dir`, so `serve::engine` saves plugins only where
  `Options` says; the daemon's tests, which build engines from default `Options`, save
  none. `serve::engine` calls `Engine::open_plugin_dir` last, after the backends, the
  credential stores and the browser, because a saved plugin keeps a handle to the engine
  as it is then and must act through its media host; it does the rest
  ([jstream-core › Design](../../crates/jstream-core/README.md#design), *WASM plugins*).
- **Browser sources** (`serve.rs`, feature `cef`, Windows only). Built with
  `--features cef`, the daemon gives the engine `jstream-browser`'s CEF backend
  (`Engine::with_browser`), so `browser` sources open their pages
  ([jstream-browser › Purpose](../../crates/jstream-browser/README.md#purpose)). It does
  so after `with_backends`, through a clone of the live engine (clones share its
  sources), so pages start on the media clock. First
  it opens a GPU device on the media adapter with the backends' `GpuInterop::open_device`
  and hands it over with `use_device`, so a page's first paint already lands on it. CEF
  runs only in the sandboxed browser helper, which the backend starts with the first
  page ([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)): the helper
  (`jstream-browser-helper.exe`, CEF's bootstrap exe renamed, and
  `jstream-browser-helper.dll`) and CEF's runtime files must be in the `browser` folder
  beside `jstream-daemon.exe`, as the portable zip has them, or next to it; its cache is the data directory's `cef` folder (else the log
  directory's) and its log the log directory's `cef.log`. Nothing about the browser
  stops the daemon: a device that won't open leaves pages playing audio with their
  frames dropped, a daemon whose own path is unknown leaves browser sources empty, and
  a missing helper or missing CEF files fail the page that opens, not the daemon; the
  first two are logged at `warn`. While the program stage runs, the engine lends the
  backend the stage's own GPU context in place of that device (`Engine::use_gpu`), so
  pages paint where the compositor samples them and are drawn into the mix; it takes
  the context back when the stage stops, or when the stage leaves the GPU path mid-run
  (its pages then deliver no frames for the rest of that stage), leaving the backend
  with no device, so the device the daemon opens covers only paints before the first
  stage
  ([jstream-core › Design](../../crates/jstream-core/README.md#design), *Live sources*).
  The daemon doesn't build or link CEF (only the helper does); the
  feature is off by default until the helper and CEF's runtime are packaged
  ([jstream-browser › Open items](../../crates/jstream-browser/README.md#open-items)).
  `dox` type-checks the wiring without downloading CEF, as CI's Windows clippy does. Without the feature, `browser` sources show
  nothing, as on other platforms. `browse` is that wiring over any backend, which is how
  the tests run it on `jstream-browser`'s fake.
- **MCP over streamable HTTP.** `serve.rs` sets `ServerConfig::mcp`, so the server
  answers MCP at `/mcp` on the same port, with its default MCP session cap and idle
  timeout. Each MCP session is one more engine client through the same `Connector`, so
  an agent's edits form their own undo history
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)). There is
  no option to turn it off: it is behind the same token as the WebSocket API, which
  already gives full control ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)),
  except YouTube's account features, which MCP leaves out ([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md) §3).
- **Listening.** Always `127.0.0.1` (`ServerConfig::new`, with only the port, MCP and the UI directory changed):
  the server's `Host`, `Origin` and token checks apply unchanged
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)). There is no
  option to listen on other addresses until remote browsers need it (with TLS,
  [ROADMAP › M2](../../ROADMAP.md#m2)). Pairing is served already, and its device tokens
  are accepted on loopback only until then. `ServerConfig::tls` stays off, so the browser
  pairing page and the device cookie
  ([ADR-0047](../../docs/adr/0047-browser-pairing-page-and-device-cookie.md)) wait on
  TLS too: `/pair` is 404.
- **The device file** (`token.rs`, `main.rs`). The daemon names
  `ServerConfig::device_file` as the token file's path plus `.devices`, so the devices
  live and move with the token they stand beside, and deleting the two together resets
  every credential. The server owns the file: it loads it at bind, writes it, and keeps
  only BLAKE3 digests in it
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)).
- **The token** ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence): a random
  token in a user-only file). New tokens are 32 bytes from the OS's random source
  (`getrandom`), written as 64 lowercase hex characters, so they fit an `Authorization`
  header unchanged. The file is created new (`CREATE_NEW` on Windows), so a file that
  appears meanwhile is never replaced, and user-only: mode `0600` on Unix, and on
  Windows a protected DACL for SYSTEM and the user set by the same `CreateFileW` call
  (`jstream_platform_windows::create_private_file`, #344), so it never has a shared
  folder's inherited entries. An existing file is refused, with "delete it to get a new
  token", if other users may access it: on Unix any group or other permission; on
  Windows (`check_private_file`) an owner other than the user, SYSTEM or Administrators,
  a missing or null DACL, or an allow entry giving anyone else more than reading its
  descriptor or attributes. Administrators are trusted as root is on Unix. The random bytes are drawn
  before the file is created, and a failed write removes the file, so no empty or
  partial token is left to be reused. Reusing an existing token keeps configured
  clients working; rotating it means deleting the file and restarting.
- **Signals.** The handlers are installed before the server binds, so a failure is a
  startup error (exit 1) rather than a daemon that can't be stopped gracefully. The
  signal future is `serve_with_shutdown`'s, which closes open connections with close
  code 1001.
- **Output through writers, errors with `anyhow`.** Clippy denies `print!`
  ([R-RS-005](../../docs/RULES.md#R-RS-005)), so output goes through locked stdout and
  stderr handles. Output is best effort: with stdout closed the server still runs.
  Failures are `anyhow` errors printed with their causes (apps may use `anyhow`,
  [R-RS-003](../../docs/RULES.md#R-RS-003)); only the exit code's kind matters, so no
  typed error is needed, unlike `jstream-cli`'s one code per protocol error.
- **Logging.** The libraries log through `tracing`; as the host, the daemon installs
  the one subscriber ([R-RS-005](../../docs/RULES.md#R-RS-005)): two of
  `tracing-subscriber`'s `fmt` layers writing the same text lines, one to stderr (so
  stdout keeps only the lines scripts read) and one to the log files, under one
  `Targets` filter parsed from `--log-level` or `RUST_LOG`. It is installed after the
  token file path is known and before the token is loaded, and nothing is logged before
  it, so usage errors still start with `error:`. `Targets` takes `target=level`
  directives, not `env-filter`'s span and field filters, which need `regex`; that is
  enough to turn one crate up. The spans and fields are each library's
  ([jstream-server › Design](../../crates/jstream-server/README.md#design),
  [jstream-core › Design](../../crates/jstream-core/README.md#design)); the files get
  exactly what stderr gets, so the libraries' redaction
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001): the token is never logged) holds for
  both, and the end-to-end test checks the file for the token too.
- **Log rotation by size, hand-rolled** (`logs.rs`, [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence):
  rotating logs in the data directory). A new file per run and per 10 MiB, and the
  newest 10 kept, bound the disk used however verbose the filter or long the run; a
  file per run also keeps two daemons from writing one file. `tracing-appender` was
  the alternative: it rotates by time only (a day of `debug` lines is unbounded), and
  would add `time`, `crossbeam-channel` and `symlink` for what is about 100 lines over
  a mutex-guarded `File`. The subscriber writes each event with one `write` call, so a
  file only ever switches between lines. Writes are synchronous, like stderr's; the
  libraries don't log on real-time threads ([R-RS-005](../../docs/RULES.md#R-RS-005)).
  Pruning skips the file being written but may delete another running daemon's (on
  Unix it keeps writing to the unlinked file; on Windows `std` opens files with
  `FILE_SHARE_DELETE`, so the same is expected, untested).
- **The data directory** comes from `jstream-platform`'s `paths` module, the desktop
  app's too: `%APPDATA%\JStream`, or a `data` folder beside the exe when a `portable`
  marker file sits there ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md),
  [jstream-platform › Purpose](../../crates/jstream-platform/README.md#purpose)).
- **Crash reports** ([`jstream-crash`](../../crates/jstream-crash/README.md#purpose),
  [ADR-0067](../../docs/adr/0067-shared-crash-reports.md),
  [R-SEC-004](../../docs/RULES.md#R-SEC-004)). `main` makes the `Secrets` list and hands
  it to the hook, to `token.rs` and to `serve.rs`. The panic
  hook, installed as soon as the log directory exists, writes the daemon and protocol
  versions, OS and architecture, the UTC time, the process ID, the thread name, the
  panic's location and message (cut at 16 KiB), and a backtrace (`std`'s, always
  captured). It never writes the environment, the command line or settings. Secrets
  registered with it are replaced by `***` anywhere in the report and in its stderr
  line ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): the control token, by `token.rs`,
  and every stream key the live engine stores or reads, since `serve::live_redacting` hands
  `live`, and through it the engine and the publish opener, its key store behind
  `Redacting`, which
  registers each secret as it is stored or loaded, and every YouTube refresh token,
  since `serve.rs` hands YouTube its Credential Manager token store behind
  `Redacting` too. The secrets that pass through neither store are registered by the
  YouTube setup's secret hook (`Secrets::hook(SecretKind::slot)`, #353): every OAuth
  access token and rotated refresh token the account's client gets, and every stream
  key YouTube sends it, such as the one a `youtube` output's `Youtube::ingest` reads for
  its connect. The client's access tokens share one slot, which keeps the newest and
  the one it superseded, so hourly refreshes don't grow the list; a sign-in's access
  token is kept, since the store may refuse it and leave the client on its own (#844).
  Only their length and BLAKE3 digest are kept, and the text is scanned for windows
  with that digest, so the hook holds no plaintext. It replaces the default hook,
  whose stderr line would quote the message unredacted; `main.rs`'s `crash_line` prints
  the hook's redacted line instead (the crate never prints). A panic in a tokio task writes
  a report too, even though the daemon carries on. The oldest reports beyond 10 are
  deleted before each new one.
- **Argument parsing is hand-rolled**, for the reasons `jstream-cli` gives
  ([jstream-cli › Design](../jstream-cli/README.md#design)): a handful of options
  (`--token-file`, `--port`, `--output`, `--collection`, `--plugin-dir`, `--ui-dir`,
  `--log-dir`,
  `--log-level`, `--youtube-client`, `--register-vcam`, `--unregister-vcam`) don't pay for a
  parser crate.
- **Dependencies.** `jstream-core` and `jstream-server` (what a host links),
  `jstream-platform` (`Backends`, the key store trait, the data directory),
  `jstream-browser` (the `BrowserBackend` seam, and its CEF backend with the `cef`
  feature; already in the tree through core), `jstream-platform-windows` on Windows only
  (the backends, the key store, the Media Foundation check and the user-only token
  and log files), `jstream-platform-macos` on macOS only (the backends built so far,
  the GPU-interop stand-in and the Keychain key store),
  `jstream-protocol` (`OutputId`, `OutputKind`), `jstream-media` (`Codec`, for the AAC
  check), `jstream-youtube` on Windows only (the HTTPS transport) and `serde_json` (the
  OAuth client file), `anyhow` and `tracing` (the startup line), all already in the
  tree through core and the server; `jstream-crash` (the crash reports,
  [ADR-0067](../../docs/adr/0067-shared-crash-reports.md)), a workspace crate only the
  two apps use; tokio (`rt-multi-thread` for the runtime,
  `signal` for Ctrl-C, which adds `signal-hook-registry` on Unix), `tracing-subscriber`
  (features `fmt` and `std` only: no `ansi`, `env-filter` or `tracing-log`, so it adds
  only `sharded-slab` and `thread_local`), and `getrandom`
  (OS randomness; already in the tree through tungstenite's `rand`). `std` has no
  portable source of cryptographic randomness, and the alternatives without a crate
  (`RandomState`'s hash keys, reading `/dev/urandom`) are either not documented as
  cryptographic or Unix only. Dev-dependencies: `jstream-control` (feature `ws`),
  `futures-util` and tokio's `time`, to drive the daemon as a client would;
  `jstream-platform` (feature `fake`), for the live engine and opener tests;
  `jstream-browser` (feature `fake`), for the browser wiring's test;
  `jstream-youtube` (feature `fake`), for the YouTube setup's test.

<a id="open-questions"></a>
### Open questions

1. **Default port.** No port is chosen yet, so the default is a free one and the URL
   line reports it. The docs' examples use 4455, which is also OBS WebSocket's default
   port, so a JStream default should avoid it. A fixed default would let `jstream-cli`
   default its `--url` ([jstream-cli › Design](../jstream-cli/README.md#design), open
   question 5).
2. **Default token file location.** The platform config directory
   ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence); `%APPDATA%\JStream`
   or the portable folder per [ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)),
   once the engine's settings code locates it. Until then the path is required, and the
   same variable serves the daemon and the CLI.
3. **Windows ACL on real hardware.** The token, devices and log files are now created
   with a user-only DACL and an existing token or devices file's DACL is checked (#344),
   but that has only run in CI's `windows-latest` job, never on a machine with a token
   in a shared folder, a domain account or a synced folder.
4. **Native crash dumps.** Only panics write a report. A native crash (an access
   violation in a driver or FFI code, an abort) writes nothing yet, though
   [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence) plans local minidumps.
   On Windows that needs either `MiniDumpWriteDump` from an unhandled-exception filter,
   which is `unsafe` and so belongs in `jstream-platform-windows`
   ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001); the daemon forbids `unsafe`), or
   an out-of-process monitor (`crash-handler` and `minidumper`), whose attach call is
   `unsafe` too and which adds a helper process. Either needs testing on Windows, and
   would be shared with the desktop app through `jstream-crash` (ADR-0067).
5. **Data directory.** The log directory's default is `logs` in the data directory
   (`jstream-platform`'s `paths`, which handles ADR-0066's `portable` marker), and
   `--log-dir` overrides it. The token file (question 2) and the collection
   (question 6) don't default to it yet.
6. **Default collection file.** Without `--collection` the collection is not saved.
   A default (in the data directory, as [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)
   places collections) would make every run persist, but needs the same settings code as
   question 2 to find that directory, and a way to start from scratch.
