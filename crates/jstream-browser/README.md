<!-- @doc
id = "CRATE-jstream-browser"
title = "jstream-browser"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-browser

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-browser/src/**", "crates/jstream-browser/examples/**", "crates/jstream-browser/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-browser", "docs/ARCHITECTURE.md#browser-source"]
-->

The engine side of browser sources ([ADR-0021](../../docs/adr/0021-browser-source.md)):
web pages rendered off screen by CEF in a sandboxed helper process, each page's frames
as textures on a GPU device the host hands it and its audio as a stream the engine
records like a device's. Layer 3, its own crate rather than part of a platform crate, so that only
the helper's build downloads and builds CEF
([ADR-0046](../../docs/adr/0046-browser-crate-and-helper.md),
[ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)).

**The seam.** `BrowserBackend` is what `jstream-sources` opens pages through, injected
into its `Context` by the engine (`Engine::with_browser`). `open` takes a `PageSpec` (a
location, a size, a frame-rate cap, the custom CSS and whether a page from the
internet may reach the local network) and returns a `BrowserPage`:
its latest frame as a `GpuFrame`, its audio, a way to replace its custom CSS while it
runs, and how many of its requests were refused (`denied_requests`). `use_device` gives
the backend a GPU device to copy the pages' paints onto. The host gives it one it opens
on the media adapter, for paints before any program stage; while the program stage
runs, the engine lends it the stage's own context in its place, so the compositor
samples the pages there, and takes it back (leaving none) when the stage stops
([jstream-core › Design](../jstream-core/README.md#design), *Live sources*). Without
a device a page renders and plays but delivers no frames. Dropping the
page closes it.

**Built everywhere:**
- `PageLocation` and `PageSpec` (`page.rs`). A web address must be `http://` or
  `https://` with a host, at most 8 KiB, without whitespace or control characters; an
  empty one is a blank page. A local file becomes a percent-encoded `file:///` URL,
  including UNC and `\\?\` paths. The custom CSS (at most 64 KiB) goes into the page as
  one `<style id="jstream-custom-css">` that every change replaces, through a script
  that quotes it as a JavaScript string (`css_script`). The default CSS makes the page's
  background transparent, as OBS does.
- `PageAudio` (`audio.rs`): a page's audio, 48 kHz stereo `f32`, handed to whichever
  `AudioInputHandler` the engine attached (`open`; dropping the returned stream detaches
  it). Packets come interleaved from the helper (`push_interleaved`) or planar
  (`push_planar`: mono doubled, channels past two dropped), and are split into chunks of
  at most `MAX_CHUNK_FRAMES`. Each chunk is stamped on the media clock: its packet's
  arrival time less the packet's length, plus the chunk's offset, since CEF's own `pts`
  isn't on a documented clock yet
  ([ARCH › Browser source](../../docs/ARCHITECTURE.md#browser-source)). Pushing never
  waits: a packet that meets the handler's lock while the engine attaches or detaches
  is dropped and counted (`dropped`), and the device position still counts it, so the
  mixer sees the gap.
- `RequestPolicy` (`request.rs`): what a page may request, from its location (ADR-0021:
  no file access beyond the source's own file). Web URLs (`http`, `https`, `ws`,
  `wss`), `data:`, `blob:`, `about:blank` and `about:srcdoc` are allowed for every
  page, with one exception (#347). A page loaded from the internet may not send web
  URLs to this computer or the local network unless its spec sets `local_network`
  (`RequestPolicy::for_page`). Local here means a loopback, private, link-local,
  shared-space (100.64/10) or unspecified address, IPv4-mapped and IPv4-compatible
  forms included, `localhost` and names under it, single-label names, and names under
  `.local`, `.lan`, `.home.arpa` or `.internal`. A page from a local file, a blank page
  and a page whose own host is local have no such limit. The check reads the host
  Chromium has already canonicalised, so it can't catch a public name that resolves to
  a local address, whether fixed (`*.nip.io`) or by DNS rebinding. `file:` URLs are
  allowed only for a page loaded from a local file, and only for files in its folder or
  below. The path is compared by name first, with `..` applied,
  and only a path inside the folder is ever opened. So a URL naming another server (an
  SMB connection on Windows), a drive-relative path or a device (a `\\.\` path, or on
  Windows a reserved name such as `COM1` or `nul.txt`) is refused untouched. It is
  compared again after resolving links on both sides, so a link inside the folder can't
  reach out; a missing file is refused. Every other scheme is refused. Each refusal is counted. The
  folder is trusted as an asset root is: something that can write in it could swap a
  file for a link between the check and the load.
- `fake` (feature `fake`, and in tests): `FakeBrowser`, a backend that records the
  pages it opens, their CSS changes and whether they were closed, never paints, and can
  be told to refuse. `FakePageState::request` plays a request through the page's
  policy.

**The CEF backend** (`cef/`, Windows and feature `cef` only): pages run in the sandboxed
browser helper process ([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)),
never in the engine's. This side builds no CEF; `dox` compile-checks it with the rest.
- `CefBackend::new` takes a `CefConfig` (the helper exe, the cache root and an optional
  log file; `CefConfig::beside_exe` puts the helper next to the running exe and the
  cache under the host's state folder) and the media clock. The helper starts on the
  first `open`: `jstream-browser-helper.exe --jstream-pipe=<name>`, after the backend has
  created the two named pipes (`cef/pipe.rs`): one each way, each a single instance that
  refuses remote clients, under a fresh random name. Once both are connected, the
  process on the other end must be the helper the backend started. The backend then sends
  the cache and log settings and waits up to 30 s for CEF to start. A missing helper exe,
  a helper that exits or doesn't connect, or CEF failing to start fails that `open` and
  every later one with `BrowserError::Start`. A helper that stops later fails its pages,
  and the next `open` starts a new one. Dropping the backend asks the helper to close its
  pages and exit, and kills it after 15 s.
- **The pipe** (`wire.rs`, `link.rs`, built everywhere and tested over in-memory pipes):
  length-prefixed messages; a malformed or oversized one ends the link. The engine
  opens a page and waits up to 10 s for the helper to confirm it, sets its CSS, closes
  it, tells the helper which adapter to paint on, and shuts it down. The helper sends
  frames, audio, audio failures, refused-request counts (never URLs) and warnings for
  the engine's log. A reader thread hands each to its page, stamping frames and audio on
  the media clock as they arrive. When the helper's pipe ends, every page's audio fails
  (`DeviceLost`), unless the engine was shutting it down.
- **Frames** (`cef/gpu.rs`): each shared texture the helper names is duplicated out of
  the helper's process, opened once per slot on the engine's D3D12 device
  (`OpenSharedHandle`), checked to be a single-mip BGRA8 texture of the size the helper
  said, imported into wgpu, and each paint copied from it into the next of three
  textures the page owns, waiting for the copy before the slot goes back to the helper.
  Nothing is read back to the CPU. The frame is `Bgra8`, premultiplied, BT.709 full
  range, stamped on arrival. A paint that can't be copied keeps the last frame and is
  logged once until one succeeds. `use_device` also tells the helper the device's adapter
  LUID; without a device, or one without a LUID, pages deliver no frames.

**The helper's side** (`helper/`, Windows and feature `helper`, which only
`jstream-browser-helper` turns on; it builds CEF with its sandbox):
- `run_helper` is all the helper DLL's `RunWinMain` does. In a CEF subprocess it hands
  the process to CEF. In the browser process it connects to the pipes, starts CEF with
  the sandbox on, windowless rendering, profiles in memory (no `cache_path`, no persisted
  cookies) and warnings-only logging, and runs CEF's message loop. The engine's messages
  are read on a thread of their own; a new adapter and shutting down go to CEF's UI
  thread. The engine's `Shutdown`, its pipe closing, or a write to it failing closes
  every page, and the loop quits once they have closed, or after ten seconds.
- Every page is a windowless browser with shared textures on, painting at its spec's
  frame rate with a transparent background. Command-line switches let pages play sound
  without a click and turn off first-run and error dialogs. Pop-ups are blocked. Every
  permission a page asks for (camera, microphone, screen capture, geolocation,
  notifications, clipboard, MIDI and the rest) is refused by the page's permission
  handler rather than left to CEF's default: a media request is cancelled and a
  permission prompt answered `DENY`. The answer comes from `permission.rs`, whose test
  pins it for every permission bit (#347). The
  custom CSS is applied when the main frame finishes loading and again on each change.
  Every request a page makes, navigations included, goes through its `RequestPolicy`
  in `OnBeforeResourceLoad` and is cancelled when refused, and so does a redirect's
  target in `OnResourceRedirect`, which can't cancel: a refused one is pointed at
  `about:jstream-refused`, which loads nothing. The engine logs the first refusal per
  page at `warn` and later ones at `debug`, with the count only.
- **Frames** (`helper/d3d11.rs`): a D3D11 device on the engine's adapter. Each
  accelerated paint's texture is opened, checked to be single-mip BGRA8, and copied into
  the next of three textures the page shares with the engine through NT handles; the
  copy is waited for before the callback returns, since CEF takes its texture back then.
  A slot the engine hasn't released isn't painted into: with all three held, the paint is
  skipped. Popup-widget paints are ignored.
- **Audio** (`helper/handlers.rs`): the audio handler asks for 48 kHz stereo in 10 ms
  packets and sends each interleaved (mono doubled, channels past two dropped). A stream
  error fails the page's audio in the engine, which reopens its input, except while the
  page is closing, when "Socket closed unexpectedly" is CEF's normal end of the stream.

**The hardware check.** `examples/page_check.rs` (features `cef` and `helper`) lays the
helper out in the build's folder as it ships (CEF's `bootstrap.exe` copied as
`jstream-browser-helper.exe` beside the helper's DLL), then opens the built-in test page
(or `--url`) through the CEF backend on a device of the media adapter and the QPC clock,
as the daemon's pages do before a program stage lends its own context, and reports the frames and audio that
arrive, a custom-CSS change the page reacts to, the requests refused (the built-in page
loads one image from outside its folder), and the shutdown time. It runs on Windows only.

<a id="open-items"></a>
## Open items
<!-- @section
depends_on = ["docs/adr/0021-browser-source.md", "docs/adr/0046-browser-crate-and-helper.md", "docs/adr/0048-sandboxed-browser-helper.md"]
-->

What ADR-0021 asks for that this crate doesn't do yet. The first blocks any host from
shipping the `cef` feature.

- **Packaging and updates.** The CEF runtime and the helper (the renamed bootstrap exe
  and its DLL) aren't in the portable zip yet, with CEF's licence notices, and nothing
  checks the pinned CEF against CEF stable.
- **Measurements.** Spike S4 measured CEF in the engine's process; the sandboxed
  helper's frames take a second copy and a pipe round trip, which haven't been measured.
- **Hosts.** The desktop app and the daemon each create a backend with their `cef`
  feature, on a device of the media adapter they open
  ([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)); the engine
  replaces it with the program stage's context while the stage runs, and the host's
  device is not given back when it stops.
- **Compositing.** The program mix draws a page's `GpuFrame`, imported premultiplied
  on the stage's context, but only the fake backend's textures have been through it:
  a real CEF texture on Windows hardware is #336.
- **Settings and interaction.** "Shut down when not visible", "refresh when shown" and
  `source.interact` need the source-visibility hook and a protocol action. No page gets
  control-API access, and the per-source opt-in doesn't exist.
- **Audio timestamps.** Packets are stamped when they reach the engine; if CEF's `pts` is
  QPC time, as S4 suggests, the helper could send it and the engine use it instead.
