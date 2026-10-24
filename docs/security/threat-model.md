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
| Stream keys | Anyone holding one can broadcast as the user's channel | OS credential store only ([R-SEC-001](../RULES.md#R-SEC-001)); Windows Credential Manager today. A macOS Keychain store is built ([ADR-0063](../adr/0063-macos-platform-support.md)) but no app uses it yet, and Linux has none yet. **Accepted** for Linux, not built ([ADR-0071](../adr/0071-linux-secret-store.md)): the Secret Service, else a `0600` plaintext file (see [Secrets](#secrets)) |
| YouTube OAuth refresh token | Manages the user's channel (the `youtube` scope) | OS credential store; access tokens stay in memory |
| Control token | Full control of the engine, by design ([ADR-0006](../adr/0006-mcp-full-access.md)), including every stream key's use | The daemon's token file (user-chosen path); the desktop app's is in memory and new on every start |
| Paired device tokens | Same power as the control token ([ADR-0044](../adr/0044-pairing-remote-browsers.md)) | A digest in the devices file beside the token file; the device's own copy; a browser's in a cookie (**in flight**, #288) |
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
  device's, accepted only on loopback until there is TLS, and `POST /api/v1/pair` is
  the one request answered without one, only while a pairing code is open (see
  [Pairing](#pairing)).
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
depends_on = ["docs/adr/0044-pairing-remote-browsers.md", "docs/adr/0047-browser-pairing-page-and-device-cookie.md"]
-->

**Built:** codes and device tokens ([`jstream-server`](../../crates/jstream-server/README.md#design)).
`pairing.start` and device tokens work only while the server listens on loopback, since
there is no TLS yet, so a paired device is a local client with its own revocable token.
**In flight:** the browser pairing page and cookie (#288). Until it merges, no browser
can pair, although [R-SEC-002](../RULES.md#R-SEC-002) and SECURITY.md already describe
that exception.

**Threats:** guessing a pairing code, a leaked device token, a cookie used cross-site,
a device that should no longer have access.

**Controls** ([ADR-0044](../adr/0044-pairing-remote-browsers.md),
[ADR-0047](../adr/0047-browser-pairing-page-and-device-cookie.md); the cookie's arrive
with #288): one open code at a time, 40 bits, two minutes, void after five wrong
attempts; 256-bit device tokens kept as digests, revocable, dropped after 30 days
unused; the browser's copy only as a
`__Host-` cookie that is `Secure`, `HttpOnly` and `SameSite=Strict`, on a TLS listener,
with `Sec-Fetch-Site` checked. A paired browser holds only its device token, and the
cookie never carries the control token.

**Accepted:** a device token has the control token's full power ("no tiers").

**Gaps:** the devices file has the same Windows ACL gap as the token file (#344).

<a id="browser"></a>
## Browser source
<!-- @section
depends_on = ["docs/adr/0021-browser-source.md"]
-->

**In flight** (#283, #289, #290, #291, #298 and ADR-0048 on that branch). Main has only
the decision ([ADR-0021](../adr/0021-browser-source.md)) and spike S4, which runs
unsandboxed and is never shipped.

**Threats:** a page exploiting the renderer to reach the engine, which holds the stream
keys; a page reading local files; a page driving the control API; a page using the
camera or microphone; a page probing the LAN.

**Controls (in flight):** pages run in a separate helper process with CEF's sandbox on;
profiles are in memory, with no cache or cookies on disk; a request policy allows only
web schemes, `data:`, `blob:` and `about:` pages, and `file:` only below a local page's
own folder; popups are not shown. The control server already refuses a page's
WebSocket upgrade, since its `Origin` is foreign or `null`.

**Gaps:** no explicit policy for permission requests (camera, microphone, location) and
no stance on requests to loopback and LAN addresses (#347). ADR-0021's per-source
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
depends_on = ["docs/RULES.md#R-SEC-003", "docs/adr/0035-clap-plugins-out-of-process.md", "crates/jstream-plugin-wasm/README.md#purpose"]
-->

| Kind | Trust | On main | Boundary |
|---|---|---|---|
| CLAP audio plugins | Trusted native code the user installed; adding one shows a warning | The shared-memory contract (`jstream-plugin-proto`) and the mixer's side; the host process is not built (#281 in flight) | One host process per instance, in a kill-on-close job, with no token, no protocol connection and a scrubbed environment ([ADR-0035](../adr/0035-clap-plugins-out-of-process.md)). It isolates crashes, not capabilities |
| WASM automation plugins | Untrusted | The `jstream-plugin-wasm` host (#486, #623, #722): a fixed set of five imports and no WASI, an empty capability list by default, a fuel budget, a wall-time deadline per entry (100 ms an invocation, 1 s the load), 64 MiB of memory, 10,000 table elements, a 512 KiB stack and 50 commands to `Control` a second, each edit of a batch counted. No app loads plugins yet | wasmtime with no ambient imports, so no filesystem, network or clock; a call reaches `Control` only when every command it names is granted ([R-SEC-003](../RULES.md#R-SEC-003)) |
| WGSL effect packs | Untrusted GPU code | Nothing (X1); only built-in shaders | naga validation ([ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline)) |

What the engine reads back from a CLAP host is already checked: the block's magic,
version and sizes, seqlocked rings with fixed slot indexing, a latency cap, and
non-finite samples turned to silence.

**Gaps:**
- CLAP (#351): the shared section's name and DACL are not specified; parameter values
  coming back are not validated (nothing reads them yet); plugin-reported names reach
  the UI and agents; the host can reset the mixer's miss counters.
- WASM (#348): ADR-0069 (accepted on `wave/m1-3`, #584) decided the limits. The host
  builds its CPU and memory limits (the 100 ms and 1 s deadlines, 64 MiB and 10,000
  table elements), the cap of 50 commands a second (answered with the ABI's
  `STATUS_LIMITED`) and the 1,000-event queue with its `events.dropped` notice (nothing
  feeds it yet), and adds a fuel budget of its own. Not built yet: the capability
  classes checked against the protocol's table and grants made only by people, the
  `forbidden` error,
  disabling a plugin that fails, and the untrusted-output handling (U1: plugin text about the
  plugin in `remote_*` fields, and MCP instructions saying state text can come from a
  plugin). Until grants exist a plugin is
  granted nothing; a host that granted output commands would let a plugin retarget an
  output (it can't take the old key along: #343 removes it on a new server).
- WGSL (#349): validation doesn't bound run time; a hanging shader resets the GPU, and
  device loss isn't handled.

<a id="mcp"></a>
## MCP agents
<!-- @section
depends_on = ["docs/adr/0006-mcp-full-access.md", "docs/adr/0036-mcp-events-polling-tool.md"]
-->

**Threats:** an agent following instructions hidden in text it reads (prompt
injection), and so using its full access against the user.

**Controls on main:** MCP goes through the same gate and token as the WebSocket API
(over stdio, through `jstream-cli mcp`, it uses the CLI's token). The desktop app keeps
MCP off. Tool arguments and results are not logged; the audit log records who changed
what, without arguments. Stream keys are never in a result, and a retarget to another
host removes the output's key rather than sending it there (#343).

**Accepted:** full access, with no tiers and no confirmation step
([ADR-0006](../adr/0006-mcp-full-access.md)). An agent sees the program through
`video.snapshot`, and any file the user can read through an image source. A key an
agent sets with `output.set_key` is in the agent's own transcript.

**Gaps:** results don't mark third-party text, and an RTMP server's error text reaches
the agent unbounded (#346).

<a id="secrets"></a>
## Secrets and the network
<!-- @section
depends_on = ["docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-004", "docs/adr/0071-linux-secret-store.md"]
-->

**Threats:** a key in a file, log, event or reply; a key sent to the wrong host; an
intercepted connection.

**Controls on main:**
- Keys and refresh tokens live in Windows Credential Manager. Where an app has no
  store (the macOS Keychain store is built but not wired into an app yet; Linux has
  none) `set_key` fails rather than fall back to a file. Secret types print `***` and
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
- `secrets_are_redacted` checks the engine's logs, replies, events and audit lines.

**Accepted:** a key pasted into the URL's path (`rtmp://host/app/<key>`) is stored as
the URL, as the target documentation warns.

**Accepted** ([ADR-0071](../adr/0071-linux-secret-store.md), nothing built): on Linux,
keys and refresh tokens go to the Secret Service, and only where none is reachable to
`secrets` in `~/.config/jstream`. That file is **plaintext at rest**: anyone with read
access to the user's account or its files, or to a backup or sync of that folder, can
read the keys in it, and use them to broadcast as the user's channel. The mitigations:
- the file is `0600` in a `0700` folder, and refused if group or others can read it;
- the UI shows a lasting warning while the fallback holds any secret, and the daemon
  logs one at startup;
- secrets move into the keyring, and the file is deleted, once a keyring is reachable;
- a later setting turns the fallback off, so a machine can be keyring-only.

A keyring that is locked, or whose unlock prompt is dismissed, fails the call and
never sends the secret to the file.

**Gaps:** `rtmp://` sends the key in cleartext with no warning, and the redaction test
covers the engine only (#353). The daemon's crash reports already redact the control
token and every stream key and refresh token that passes through its `Redacting`
stores; a native crash writes no report.

<a id="vcam"></a>
## Virtual camera
<!-- @section
depends_on = ["docs/adr/0020-portable-windows-distribution.md"]
-->

**Threats:** another session's user reading the camera feed or injecting frames; a
user-writable file loaded into Frame Server.

**Controls on main:** the control block, textures and fence are created in the
session's `Local\` namespace with a protected DACL (SYSTEM, Administrators, LOCAL
SERVICE for Frame Server, and the user). The DLL inside Frame Server checks that a
session block's owner is that session's user, SYSTEM or Administrators before using
it, and checks every texture's format.

**Accepted:** every app that opens the virtual camera sees its frames; LOCAL SERVICE
needs full access; `Global\` blocks, which only an administrator or service can create,
are trusted.

**Gaps:** a user-writable registered DLL runs in Frame Server (#242); a control mapping
created before the producer's keeps its own DACL (#356).

<a id="gaps"></a>
## Known gaps
<!-- @section
-->

Each is a GitHub issue. Severity is this model's estimate of impact on a default setup.

| Issue | Area | Gap | Severity |
|---|---|---|---|
| #344 | Server, pairing | Windows token, devices and log files only inherit their folder's ACL | Medium |
| #345 | Files | `absolute` asset paths accept UNC and device paths | Medium |
| #346 | MCP | Remote text reaches agents unbounded and unmarked | Medium |
| #347 | Browser | No permission policy; no stance on loopback and LAN requests | Medium |
| #348 | WASM | ADR-0069's capability classes, grants, `forbidden` error and untrusted-output handling are decided but not built (the CPU and memory limits, the command cap and the event queue are) | Medium |
| #349 | WGSL | No GPU-hang or device-lost handling | Medium |
| #351 | CLAP | Shared section naming, and validation of what the host sends back | Medium |
| #353 | Secrets | `rtmp://` cleartext, crash-report redaction, redaction test scope | Low |
| #355 | Files | A recording can replace a file created just before its rename | Low |
| #356 | Virtual camera | A pre-created control mapping keeps its own DACL | Low |
| #242 | Virtual camera | A user-writable registered DLL runs in Frame Server | Medium |
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
