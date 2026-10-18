<!-- @doc
id = "THREAT-MODEL"
title = "Threat model"
kind = "reference"
status = "active"
review_every = "90d"
-->

# Threat model

What JStream protects, from whom, where the trust boundaries are, and which controls
hold each one. It describes `main`. Work still in a pull request is named by its PR
number and marked **in flight**; a control is only counted once it has merged.

The promises a vulnerability report is judged against are in
[SECURITY › What JStream promises](../../SECURITY.md#scope), and the binding rules are
`R-SEC-*` in [RULES](../RULES.md#security). This page explains why those rules exist
and what they leave open. Each open gap is a GitHub issue, listed in
[Known gaps](#gaps); none of them breaks a promise in SECURITY.md today.

<a id="assets"></a>
## Assets
<!-- @section
depends_on = ["docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-004"]
-->

| Asset | Why it matters | Where it lives |
|---|---|---|
| Stream keys | Anyone holding one can broadcast as the user's channel | OS credential store only ([R-SEC-001](../RULES.md#R-SEC-001)); Windows Credential Manager today, no store on macOS or Linux yet |
| YouTube OAuth refresh token | Manages the user's channel (the `youtube` scope) | OS credential store; access tokens stay in memory |
| Control token | Full control of the engine, by design ([ADR-0006](../adr/0006-mcp-full-access.md)), including every stream key's use | The daemon's token file (user-chosen path); the desktop app's is in memory and new on every start |
| Paired device tokens | Same power as the control token ([ADR-0044](../adr/0044-pairing-remote-browsers.md)) | A digest in the devices file beside the token file; the device's own copy; a browser's in a cookie, once there is a TLS listener |
| The program and preview video | What the user chose not to broadcast yet (the preview scene) | Engine memory, the virtual camera's shared textures, `video.snapshot` |
| Local files | Asset paths and recording paths reach the file system | Wherever the user (or a token holder) points them |
| The machine itself | Native plugins, decoders and a browser engine run inside or beside the engine | — |

No telemetry leaves the machine, and logs and crash reports stay local
([R-SEC-004](../RULES.md#R-SEC-004)).

<a id="attackers"></a>
## Who we defend against
<!-- @section
-->

| Attacker | What they start with | In scope? |
|---|---|---|
| A web page in the user's own browser | JavaScript on a foreign origin; can try `ws://127.0.0.1:<port>` and DNS rebinding | Yes: it must get nothing |
| A device on the LAN or internet | Network access to the machine | Yes: listeners bind `127.0.0.1` by default; remote access only through pairing, whose device tokens are refused beyond loopback until there is TLS |
| A page shown by a browser source | Arbitrary HTML and JavaScript, rendered by the engine's own helper | Yes: untrusted content ([ADR-0021](../adr/0021-browser-source.md)) |
| A crafted file | A collection file, image, font or media file someone shares | Yes: parsing must not crash the engine or reach outside its asset roots |
| A hostile streaming server or network | Controls the RTMP(S) or YouTube endpoint's replies | Yes: TLS is verified; its text is untrusted |
| Text an MCP agent reads | Source names, chat (later), server error text | Yes: it may carry instructions (prompt injection) |
| A WASM plugin | Code the user installed | Yes: sandboxed by default ([R-SEC-003](../RULES.md#R-SEC-003)) |
| A CLAP plugin or WGSL effect pack | Native code, or GPU code, the user installed | Partly: CLAP gets crash isolation, not a security sandbox ([ADR-0035](../adr/0035-clap-plugins-out-of-process.md)); WGSL is validated, not bounded |
| Another process of the same user | Everything the user can do | No: it can read the token file and the credential store anyway |
| Another local user, or a service | Its own account on the same machine | Yes for files and shared memory JStream creates |
| An administrator or SYSTEM | Full control of the machine | No |

<a id="boundaries"></a>
## Trust boundaries
<!-- @section
-->

```text
 user's browser ──┐                          ┌── RTMP(S) server, YouTube API (TLS verified)
 LAN (paired) ────┤  Host/Origin/token gate  │
 CLI, desktop UI ─┼─► jstream-server ─► engine (jstream-core) ─┤
 MCP client ──────┘   WS, /mcp, UI files     │   │   │         └── credential store (OS)
                                             │   │   └── shared memory ─► Frame Server (vcam)
               collection, asset files ──────┘   └── helper processes: browser pages, CLAP hosts
```

Every arrow into the engine crosses one of five boundaries:

1. **The control API** (WebSocket, MCP over HTTP, static UI files): one gate for all of
   them. See [Control server](#server).
2. **Files**: the collection, assets, recordings. See [Files and decoders](#files).
3. **Helper processes**: browser pages and CLAP hosts. See [Browser source](#browser)
   and [Plugins](#plugins).
4. **Shared memory to other processes**: the virtual camera. See [Virtual camera](#vcam).
5. **The network out**: streaming servers and Google. See [Secrets](#secrets).

<a id="server"></a>
## Control server
<!-- @section
covers = [
  "crates/jstream-server/src/lib.rs",
  "crates/jstream-server/src/gate.rs",
  "crates/jstream-server/src/config.rs",
  "crates/jstream-server/src/listener.rs",
  "crates/jstream-server/src/session.rs",
  "apps/jstream-daemon/src/token.rs",
  "apps/jstream-daemon/src/args.rs",
]
depends_on = ["docs/RULES.md#R-SEC-002", "docs/adr/0006-mcp-full-access.md"]
-->

**Threats:** a foreign web page driving the API (CSRF, DNS rebinding), a network peer,
a stolen token, local denial of service.

**Controls on main:**
- One listener in [`jstream-server`](../../crates/jstream-server/README.md) carries the
  WebSocket API, MCP and (when configured) the UI's static files. Every request passes
  the same gate: `Host` must name the listener (`127.0.0.1`, `localhost` or `[::1]` with
  its port, or a configured extra), which defeats DNS rebinding; `Origin`, when present,
  must be that host or a configured extra, so a foreign page's WebSocket upgrade is
  refused; then the token, as a bearer token, a Basic password or a WebSocket
  subprotocol ([R-SEC-002](../RULES.md#R-SEC-002)). The token may also be a paired
  device's, accepted only on loopback or a TLS listener, and there is no TLS listener
  yet. `POST /api/v1/pair` is the one request answered without one, only while a
  pairing code is open (see [Pairing](#pairing)).
- The server keeps only a BLAKE3 digest of the token and compares digests in constant
  time. Tokens are 32 bytes from the OS random source. Header values are never logged.
- The daemon binds `127.0.0.1` and has no flag to bind elsewhere. There is no TLS yet.
- Limits: 4 MiB messages and MCP bodies, 128 open TCP connections of any kind
  (authenticated or not), 32 WebSocket connections, 64 calls in flight per connection,
  16 MCP sessions, 10 s to send headers and finish the handshake, a bounded event
  outbox.
- Failed tokens are logged one by one only up to 10 a minute, then counted, so a flood
  of them can't fill the log (#354).
- On Unix the token file is created `0600` and refused if group or others can read it.

**Accepted:** the token is full access with no tiers ([ADR-0006](../adr/0006-mcp-full-access.md)).
It doesn't expire or rotate; deleting the file and restarting makes a new one. A browser
can be given the control token through the `Basic` prompt, and then holds it as a cached,
ambient credential until it is closed. Failed tokens are not delayed or locked out:
256 bits can't be guessed, and a lockout would let any local process shut out the real
clients.

**Gaps:** the Windows token file only inherits its folder's ACL (#344).

<a id="pairing"></a>
## Pairing and remote browsers
<!-- @section
covers = [
  "crates/jstream-server/src/devices.rs",
  "crates/jstream-server/src/admitted.rs",
  "crates/jstream-server/src/pair.rs",
  "crates/jstream-server/src/pair_page.rs",
  "crates/jstream-server/src/gate.rs",
]
depends_on = ["docs/adr/0044-pairing-remote-browsers.md", "docs/adr/0047-browser-pairing-page-and-device-cookie.md"]
-->

**Built:** codes, device tokens, and the browser pairing page and device cookie (#288),
in [`jstream-server`](../../crates/jstream-server/README.md#design). `pairing.start`
answers and device tokens are accepted only while the server listens on loopback or on a
TLS listener. The server has no TLS listener yet, so a paired device is a local client
with its own revocable token, and the pairing page, which only a TLS listener serves, is
not reachable: no browser can pair today, although [R-SEC-002](../RULES.md#R-SEC-002)
and SECURITY.md already describe that exception.

**Threats:** guessing a pairing code, a leaked device token, a cookie used cross-site,
a device that should no longer have access.

**Controls** ([ADR-0044](../adr/0044-pairing-remote-browsers.md),
[ADR-0047](../adr/0047-browser-pairing-page-and-device-cookie.md)): one open code at a
time, 40 bits, two minutes, void after five wrong attempts, kept only as a digest;
256-bit device tokens kept as digests, revocable, dropped after 30 days unused; the
browser's copy only as a `__Host-` cookie that is `Secure`, `HttpOnly` and
`SameSite=Strict`, set and read only on a TLS listener, refused when a request carries
it twice or without `Sec-Fetch-Site: same-origin` or `none`. A paired browser holds only
its device token, and the cookie is never compared with the control token.

**Accepted:** a device token has the control token's full power ("no tiers").

**Gaps:** the devices file has the same Windows ACL gap as the token file (#344).

<a id="browser"></a>
## Browser source
<!-- @section
covers = [
  "crates/jstream-browser/src/request.rs",
  "crates/jstream-browser/src/helper/mod.rs",
  "crates/jstream-browser/src/helper/handlers.rs",
  "crates/jstream-browser/src/permission.rs",
]
depends_on = ["docs/adr/0021-browser-source.md", "docs/adr/0048-sandboxed-browser-helper.md"]
-->

**Built** (C8, on Windows), as [ADR-0021](../adr/0021-browser-source.md) and
[ADR-0048](../adr/0048-sandboxed-browser-helper.md) decide. Its first run on Windows
hardware is still to come (#341).

**Threats:** a page exploiting the renderer to reach the engine, which holds the stream
keys; a page reading local files; a page driving the control API; a page using the
camera or microphone; a page probing the LAN.

**Controls:** pages run in a separate helper process with CEF's sandbox on; profiles are
in memory, with no cache or cookies on disk; every request a page makes, navigations
included, passes a request policy that allows only `http`, `https`, `ws`, `wss`,
`data:`, `blob:`, `about:blank` and `about:srcdoc`, and `file:` only in a local page's
own folder or below it, compared by name and again after resolving links; popups are not
shown. The control server refuses a page's WebSocket upgrade, since its `Origin` is
foreign or `null`.

**Controls (in flight, #549):** a page from a public origin is also refused loopback,
private, link-local and local-name targets, redirects to them included, unless its
source sets `local_network`.

**Gaps:** permission requests are refused: the helper's permission handler cancels every
camera, microphone and screen request and denies every permission prompt
(`crates/jstream-browser/src/permission.rs`). What Chromium grants without asking, such as
a focused page's clipboard writes, never reaches that handler; the helper never focuses a
page or sends it input (#347).
The local-network refusal reads only the URL's host: a public name that resolves to a
local address, whether fixed (`*.nip.io`) or by DNS rebinding, passes until Chromium's
Private Network Access checks are on, and so does any request the helper's request
handler doesn't see, such as a service worker's (#347). ADR-0021's per-source
"allow page to control JStream" opt-in must not hand the page the control token (#347).

<a id="files"></a>
## Files and decoders
<!-- @section
-->

**Threats:** a crafted collection or media file crashing the engine or exhausting
memory; an asset path escaping its root; a recording overwriting a file; a path that
makes Windows contact a remote host.

**Controls on main:**
- Asset references name a root: `collection` and `app` paths are relative, confined
  after resolving links (by inode on Unix, by reparse-point walk on Windows); `absolute`
  paths are refused if they contain `..`.
- Image formats are sniffed from their bytes; each decoder has a memory limit; files are
  capped at 256 MiB, 16384 pixels a side and 1 GiB decoded. The image and font
  decoders are memory-safe Rust.
- Recordings must be absolute paths without `..`, not UNC, device or NT object paths,
  with no `:` but a drive letter's, and not ending in a DOS device name (`NUL`,
  `nul.mp4`, `CON`, `COM1`, …), which Windows opens in place of a file. They are written to a `.part` file created
  new, and never replace an existing file.
- Fuzz targets cover the protocol, source and output kind settings, collection loading,
  the image decoders and font loading, AMF0, H.264, HEVC, AV1, AAC and the muxers
  (`fuzz/`). The scheduled `fuzz` workflow runs each of them nightly and files an issue on
  a crash.

**Accepted:** a token holder may read any file the user can, as an image or font, and
write recordings anywhere the user can. That follows from ADR-0006's full access.

**Gaps:** `absolute` paths accept UNC and device paths, so a shared collection can make
Windows send the user's NTLM hash to a remote host (#345); a file created between the
recording's last check and its rename is replaced (#355). The Media Foundation file
reader (in flight, #245) will parse untrusted media in native code inside the engine.

<a id="plugins"></a>
## Plugins
<!-- @section
depends_on = ["docs/RULES.md#R-SEC-003", "docs/adr/0035-clap-plugins-out-of-process.md"]
-->

| Kind | Trust | On main | Boundary |
|---|---|---|---|
| CLAP audio plugins | Trusted native code the user installed; adding one shows a warning | The shared-memory contract (`jstream-plugin-proto`) and the mixer's side; the host process (`jstream-plugin-host`) and its launcher; running a filter from the graph is not wired yet | One host process per instance, in a kill-on-close job, with no token, no protocol connection and a scrubbed environment ([ADR-0035](../adr/0035-clap-plugins-out-of-process.md)). It isolates crashes, not capabilities |
| WASM automation plugins | Untrusted | Nothing (WA1) | wasmtime, no filesystem or network unless declared and granted ([R-SEC-003](../RULES.md#R-SEC-003)) |
| WGSL effect packs | Untrusted GPU code | Nothing (X1); only built-in shaders | naga validation ([ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline)) |

What the engine reads back from a CLAP host is already checked: the block's magic,
version and sizes, seqlocked rings with fixed slot indexing, a latency cap, and
non-finite samples turned to silence.

**Gaps:**
- CLAP (#351): the shared section's name and DACL are not specified; parameter values
  coming back are not validated (nothing reads them yet); plugin-reported names reach
  the UI and agents; the host can reset the mixer's miss counters.
- WASM (#348): [ADR-0069](../adr/0069-wasm-plugin-limits-and-capabilities.md)
  decides the limits: per-call epoch deadlines, a 64 MiB memory cap, capability classes
  on a scoped `Control` (hotkey bindings included), and installs and grants by people
  only. None of it is built yet (WA1), and R-SEC-003 names an enforcing test in a crate
  that doesn't exist yet.
- WGSL (#349): validation doesn't bound run time; a hanging shader resets the GPU, and
  device loss isn't handled.

<a id="mcp"></a>
## MCP agents
<!-- @section
depends_on = ["docs/adr/0006-mcp-full-access.md", "docs/adr/0036-mcp-events-polling-tool.md", "docs/adr/0059-mcp-annotations-and-remote-text.md"]
-->

**Threats:** an agent following instructions hidden in text it reads (prompt
injection), and so using its full access against the user.

**Controls on main:** MCP goes through the same gate and token as the WebSocket API
(over stdio, through `jstream-cli mcp`, it uses the CLI's token). The desktop app keeps
MCP off. Tool arguments and results are not logged; the audit log records who changed
what, without arguments. Stream keys are never in a result, and a retarget to another
host removes the output's key rather than sending it there (#343).
Third-party text is marked as data ([ADR-0059](../adr/0059-mcp-annotations-and-remote-text.md)):
the server instructions and the `output` tool's description say that fields named
`remote_*`, and the whole `action_failed` reason of a start a streaming server refused,
are data and never instructions. That rejection text, today the only third-party text
an agent reads, has the key replaced, is cut to 512 characters and has control
characters replaced. Every tool carries MCP annotations (`readOnlyHint`,
`destructiveHint`, `openWorldHint`), so a client can ask the user before an agent
retargets an output, sets its key, goes live or deletes something (#346).

**Accepted:** full access, with no tiers and no confirmation step
([ADR-0006](../adr/0006-mcp-full-access.md)). An agent sees the program through
`video.snapshot`, and any file the user can read through an image source. A key an
agent sets with `output.set_key` is in the agent's own transcript. Annotations are per
tool, and tools are grouped by noun, so a client that asks before `output.set_target`
also asks before `output.list`; and whether a client asks at all is up to the client.

<a id="secrets"></a>
## Secrets and the network
<!-- @section
depends_on = ["docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-004"]
-->

**Threats:** a key in a file, log, event or reply; a key sent to the wrong host; an
intercepted connection.

**Controls on main:**
- Keys and refresh tokens live in Windows Credential Manager; with no store (macOS,
  Linux) `set_key` fails rather than fall back to a file. Secret types print `***` and
  are zeroed on drop. The collection never holds a key, and target URLs that could carry
  one (user info, query, fragment) are refused.
- RTMPS and the YouTube API use rustls with the OS roots and hostname checks; nothing
  turns verification off. An RTMP server's text has the key replaced before it is kept.
- A key is bound to its server: pointing an output at another scheme, host or port,
  or deleting it, removes its key, and the output can't start until that removal has
  happened. A start already past that check when the retarget lands opens the target
  it checked, not the new one, so a retarget never sends the old key to the new host
  (#343). While that start is opening, `output.set_key` is refused, so a key sent for
  the new server isn't read with the old one either.
- OAuth uses PKCE and a `state` check on a one-shot loopback listener.
- The daemon's and the desktop app's crash reports replace the control token, and every
  stream key and refresh token their credential stores hold or load, with `***`.
- `secrets_are_redacted` checks the engine's logs, replies, events and audit lines.
  Further tests check the server's WebSocket and MCP layers, the daemon's RTMP opener and
  its crash reports.

**Accepted:** a key pasted into the URL's path (`rtmp://host/app/<key>`) is stored as
the URL, as the target documentation warns.

**Gaps:** `rtmp://` sends the key in cleartext with no warning (#353). The daemon's and
the desktop app's crash reports redact the control token and the keys and refresh
tokens that pass through their credential stores, but not a YouTube-bound output's key
or the OAuth access token, which pass through neither (#353).

<a id="vcam"></a>
## Virtual camera
<!-- @section
covers = [
  "crates/jstream-vcam-proto/src/names.rs",
  "crates/jstream-platform-windows/src/vcam/shared.rs",
  "crates/jstream-platform-windows/src/vcam/sink.rs",
  "crates/jstream-vcam-windows/src/trust.rs",
  "crates/jstream-vcam-windows/src/win/session.rs",
  "crates/jstream-vcam-windows/src/win/frames.rs",
]
depends_on = ["docs/adr/0066-portable-windows-distribution-v2.md"]
-->

**Threats:** another session's user reading the camera feed or injecting frames; a
user-writable file loaded into Frame Server.

**Controls on main:** the control block, textures and fence are created in the
session's `Local\` namespace with a protected DACL (SYSTEM, Administrators, LOCAL
SERVICE for Frame Server, and the user). The DLL inside Frame Server checks that a
session block's owner is that session's user, SYSTEM or Administrators before using
it, and checks every texture's format. Registering the camera is optional, and only
the explicit step registers it
([ADR-0066](../adr/0066-portable-windows-distribution-v2.md)). The registered DLL is
an administrators-only copy, never the one in the app's folder (#242). In its one UAC
prompt, the step creates `%ProgramFiles%\JStream\vcam\` and gives it and the copy a
protected DACL: SYSTEM and Administrators full control, LOCAL SERVICE and Users read
and execute, nothing inherited. It then points `InprocServer32` at the copy. So, for a
camera registered by this step, a program running as the user can't change the code
Frame Server loads from the copy. The host
compares the copy with the DLL it ships by content and reports an outdated copy;
registering again is a new opt-in. Unregistering removes the keys and the copy.
The producer refuses a control mapping that already existed unless its owner is the
user, SYSTEM or Administrators and its DACL lets no one but them and LOCAL SERVICE in;
the DLL checks the owner of a session block's textures and fence as it does the
block's (#540, #356).

**Accepted:** every app that opens the virtual camera sees its frames; LOCAL SERVICE
needs full access; `Global\` blocks, which only an administrator or service can create,
are trusted.

**Accepted, registration:** the copy is as trustworthy as the portable folder was when the user
approved the prompt: the elevated step copies whatever DLL is there then. An update
whose DLL changed leaves Frame Server loading the old copy until the user registers
again. A camera registered before ADR-0066, from the app's own folder, stays
registered there until the user runs the step again; the host reports it as outdated.
A DLL registered by hand with `regsvr32` is registered where it is.

**Gaps:** the copy's DACL and Frame Server loading it are not yet checked on Windows
hardware (#563).

<a id="gaps"></a>
## Known gaps
<!-- @section
-->

Each is a GitHub issue. Severity is this model's estimate of impact on a default setup.

| Issue | Area | Gap | Severity |
|---|---|---|---|
| #344 | Server, pairing | Windows token, devices and log files only inherit their folder's ACL | Medium |
| #345 | Files | `absolute` asset paths accept UNC and device paths | Medium |
| #347 | Browser | Permissions Chromium grants without a prompt; public names resolving locally (`*.nip.io`, DNS rebinding) and requests the handler doesn't see | Medium |
| #348 | WASM | CPU, memory and API-scope limits decided ([ADR-0069](../adr/0069-wasm-plugin-limits-and-capabilities.md)) but not built (WA1) | Medium |
| #349 | WGSL | No GPU-hang or device-lost handling | Medium |
| #351 | CLAP | Shared section naming, and validation of what the host sends back | Medium |
| #353 | Secrets | `rtmp://` cleartext; crash reports miss a YouTube-bound key and the access token | Low |
| #355 | Files | A recording can replace a file created just before its rename | Low |
| #356 | Virtual camera | A pre-created control mapping keeps its own DACL | Low |
| #273 | Server | A hung engine hello escapes the handshake deadline | Low |

<a id="upkeep"></a>
## Keeping this current
<!-- @section
-->

- A PR that adds a listener, a helper process, a file input, a plugin kind or a new
  source of third-party text updates the matching section here, and files an issue for
  any gap it leaves.
- When an in-flight control merges, drop its **in flight** mark. When a gap's issue is
  closed, remove its row and the sentence that cites it.
- Sections cite the rules and ADRs they rely on, so changing one of those makes the
  section stale and asks for a re-read ([DOCS › Hash-bound verification](../DOCS.md#verification)).
