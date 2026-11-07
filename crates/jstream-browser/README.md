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
as textures on a GPU device the host hands it (Windows) or as CPU frames (Linux), and its
audio as a stream the engine records like a device's. Layer 3, its own crate rather than
part of a platform crate, so that only the helper's build downloads and builds CEF
([ADR-0046](../../docs/adr/0046-browser-crate-and-helper.md),
[ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md),
[ADR-0095](../../docs/adr/0095-browser-helper-on-linux.md)).

**The seam.** `BrowserBackend` is what `jstream-sources` opens pages through, injected
into its `Context` by the engine (`Engine::with_browser`). `open` takes a `PageSpec` (a
location, a size, a frame-rate cap, the custom CSS and whether a page from the
internet may reach the local network, and what the page API grants it, if anything)
and returns a `BrowserPage`: its latest frame as a `GpuFrame`, its audio, a way to
replace its custom CSS while it runs, a way to send a granted page what the engine
shows it (`send_data`), and how many of its requests were refused (`denied_requests`). `use_device` gives
the backend a GPU device to copy the pages' paints onto. The host gives it one it opens
on the media adapter, for paints before any program stage; while the program stage
runs, the engine lends it the stage's own context in its place, so the compositor
samples the pages there, and takes it back (leaving none) when the stage stops, or
when the stage leaves the GPU path mid-run, after which the pages deliver no frames for
the rest of that stage
([jstream-core › Design](../jstream-core/README.md#design), *Live sources*). Each device
is a wgpu instance of its own, so a page never hands out a frame it copied onto an
earlier device: it delivers none until it paints on the new one, and the helper asks
every page to paint again whenever it is given a device (#1183). Without
a device a page renders and plays but delivers no frames. A page that `paints_on_cpu`
(CEF on Linux) needs no device: its latest paint is its `cpu_frame`, straight-alpha
RGBA8, which `jstream-sources` shows as the source's frame and looks at again every
frame of the page's rate (ADR-0095). Dropping the page closes it. On Windows the helper
runs in a job object that kills it, and every process CEF starts from it, once the
engine's handle closes, so no helper outlives the engine, even after a crash
(`cef/job.rs`). The helper starts CEF with `--no-startup-window`, so the Chrome runtime
opens no window of its own (#1186).

**Built everywhere:**
- `PageLocation` and `PageSpec` (`page.rs`). A web address must be `http://` or
  `https://` with a host, at most 8 KiB, without whitespace or control characters; an
  empty one is a blank page. A local file becomes a percent-encoded `file:///` URL,
  including UNC and `\\?\` paths. A page can also be given as its HTML
  (`PageLocation::html`, at most 128 KiB, `MAX_HTML_BYTES`; blank text is a blank
  page), which loads from `HTML_PAGE_URL` (`https://html.jstream.invalid/`), a reserved
  name the helper answers itself with the HTML, so the page has an origin of its own
  (#1035). Being `https`, it is a secure context, and Chromium refuses its plain `http`
  scripts and frames as mixed content. The custom CSS (at most 64 KiB) goes into the page as
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
  page, with one exception (#347). A page from the internet may not send web URLs to
  this computer or the local network. Local here means a loopback, private,
  link-local, shared-space (100.64/10) or unspecified address, IPv4-mapped and
  IPv4-compatible forms included, `localhost` and names under it, single-label names,
  and names under `.local`, `.lan`, `.home.arpa` or `.internal`. It is decided by the
  origin that makes each request, CEF's `request_initiator` (`check_from`, #721), not
  by where the source started. A document whose own origin is local may. A document
  from the source's own origin may when the source's location is itself local, or
  when its spec sets `local_network` (`RequestPolicy::for_page`); the opt-in covers
  that origin exactly (scheme, host and port), so a third-party frame inside the page,
  or a page it navigates or redirects to, is refused. A local file's or a blank page's
  documents have an opaque origin, which may only while the main frame is still on a
  file or blank page: a main-frame navigation the policy allows tells it where the
  frame now is (`main_frame_to`), before the new document exists, so a file or blank
  page that navigates to the web loses the access. A page given as HTML loads no files.
  Its own origin is the reserved one it loads from, which may when its spec sets
  `local_network`, exactly as a web page's own origin may (#1035); only its main frame
  may load its URL, and every other request to the reserved name is refused
  (`html_page`), so none reaches the network. CEF can't tell opaque origins apart, so a
  sandboxed or `data:` frame made by a third-party frame inside a file page counts as
  the file page's. A request with no initiator is one the browser started (the
  source's own load, a reload) and is judged as the source's own. CEF on Linux names
  the opaque origin `null` instead for the source's own first load (#1069), so a
  main-frame load the helper started, to the source's own web origin with a `null`
  initiator while the main frame hasn't left it, counts as started by the browser
  (`check_main_frame_from`). The helper marks the URL it loads (`expect_own_load`): the
  location when the page opens, the page's URL when it reloads it, and the target when
  that load is redirected, as CEF asks again for a redirect's target. Only a `GET` of the
  marked URL takes the mark (`take_own_load`), so any other `null` main-frame load, such
  as a sandboxed frame a third-party frame made navigating the top, stays opaque, even
  while a reload is pending. The check reads the
  host Chromium has already canonicalised, so it can't catch a public name that
  resolves to a local address, whether fixed (`*.nip.io`) or by DNS rebinding.
  Chromium's Local Network Access checks do, at the network layer, for requests the
  resource handler never sees too (#347). In the pinned CEF's windowless pages they
  refuse a public origin's request to a more private address without asking the
  permission handler, and refuse every one from a page that isn't a secure context. So
  an opt-in reaches them as a setting: `local_network_grant` names the source's own
  origin when its source opted in and it isn't itself local, and the helper allows it
  there (below). An opted-in page must therefore be `https`. `file:` URLs are
  allowed only for a page loaded from a local file, and only for files in its folder or
  below. The path is compared by name first, with `..` applied,
  and only a path inside the folder is ever opened. So a URL naming another server (an
  SMB connection on Windows), a drive-relative path or a device (a `\\.\` path, or on
  Windows a reserved name such as `COM1` or `nul.txt`) is refused untouched. It is
  compared again after resolving links on both sides, so a link inside the folder can't
  reach out; a missing file is refused. Every other scheme is refused. Each refusal is counted. The
  folder is trusted as an asset root is: something that can write in it could swap a
  file for a link between the check and the load.
- `page_api.rs`: the page API's shared parts
  ([ADR-0102](../../docs/adr/0102-page-api-for-browser-sources.md)). `PageApi` is what a
  spec grants (`page_api: None` grants nothing, and the page never has
  `window.jstream`): the clock and the page's own visibility always, the scenes, the
  shared values and a bus's levels (`jstream.levels()` and a `levels` event) if
  granted. `PageData` is one update: a clock anchor (time and
  lead), the visibility, scenes and levels as JSON text, and each changed value's JSON text
  (`None` for one removed); `split` cuts one larger than `MAX_PAGE_DATA_BYTES` (192 KiB)
  into several, each value whole in one of them. For the helper: `ApiHome`, the
  documents the API is defined in, which is the source's own web origin (scheme, host
  and port, `same_web_origin` in `request.rs`), its local file's folder and below, or
  the reserved origin of a page given as HTML; `PageCache`, the updates merged, which
  a new document is fed whole, its clock moved on by the time since its anchor; and
  `shim`, the JavaScript (`page_api.js`) that defines a frozen `window.jstream` with
  only the granted members and returns the one function updates go in through.
- `cpu_frames.rs` (Linux, and in tests): what both sides of the Linux frame path agree
  on: three slot files per page, the number that names each one's file, the largest
  frame (16384 pixels a side), and the conversion of CEF's premultiplied BGRA8 to
  straight RGBA8, rounded and capped.
- `fake` (feature `fake`, and in tests): `FakeBrowser`, a backend that records the
  pages it opens, their CSS changes and whether they were closed, and can be told to
  refuse. Its pages paint only when told to paint on the CPU (`paint_on_cpu`) and handed
  a frame (`FakePageState::paint`). `FakePageState::request` plays a request through the page's
  policy, and `take_data` returns the page API updates a granted page was sent, split
  as the pipe would send them.

**The CEF backend** (`cef/`, Windows and Linux, feature `cef` only): pages run in the
sandboxed browser helper process ([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)),
never in the engine's. This side builds no CEF; `dox` compile-checks it with the rest.
What follows is Windows; **On Linux** below says what differs.
- `CefBackend::new` takes a `CefConfig` (the helper exe, the cache root and an optional
  log file; `CefConfig::beside_exe` finds the helper in the `browser` folder beside the
  running exe, as the portable zip lays it out, else next to the exe itself, and puts the
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
  length-prefixed messages of at most 256 KiB (room for a page opened with 128 KiB of
  HTML and 64 KiB of CSS); a malformed or oversized one ends the link. The engine
  opens a page and waits up to 10 s for the helper to confirm it, sets its CSS, reloads
  it (`BrowserPage::reload`, which the helper does with CEF's own reload, the custom CSS
  going in again on load), hides or shows it (`BrowserPage::set_hidden`, which the
  helper does with CEF's `WasHidden` on its UI thread, so a hidden page stops painting;
  a hide sent before the browser exists applies when it arrives, #1071), closes it,
  tells the helper which adapter to paint on, and shuts it down.
  A reload hides the page until the reloaded document paints for real: the helper skips
  the page's paints from the reload until the main frame's load has ended or failed and
  the first paint since its start has been skipped, 500 ms after that start counting
  as the load's end (`reload_hold.rs`), since Chromium's first paint of a new document can be its
  blank, opaque white (#1151). It asks for a paint 100 ms after the hold ends if none
  came, so a static page shows again. A new page starts held the same way, so its first
  load's white paint never reaches the engine either (#1155); a first load that fails
  before its document starts ends that hold at once, as no paint is coming to wait for. It says when it took the reload (`Reloading`), and the engine
  drops paints that left before that, so a `restart_on_show` page is transparent while
  it reloads, never the end of its last run nor a white frame. An `Open` carries the
  page API grant in one flags byte, and a granted page's updates go as `PageData`,
  already split to fit. The helper sends
  frames, reload acknowledgements, audio, audio failures, refused-request counts (never URLs) and warnings for
  the engine's log. A reader thread hands each to its page, stamping frames and audio on
  the media clock as they arrive. When the helper's pipe ends, every page's audio fails
  (`DeviceLost`), unless the engine was shutting it down.
- **Frames** (`cef/gpu.rs`): each shared texture the helper names is duplicated out of
  the helper's process, opened once per slot on the engine's D3D12 device
  (`OpenSharedHandle`), checked to be a single-mip BGRA8 texture of the size the helper
  said, imported into wgpu, and each paint copied from it into the next of three
  textures the page owns, waiting for the copy before the slot goes back to the helper.
  The copy is submitted under the context's `SubmitLock`, so it can't take a fence wait
  or signal that render staged for its own submit on the same queue. The lock is let go
  before the wait. Nothing is read back to the CPU. The frame is `Bgra8`, premultiplied, BT.709 full
  range, stamped on arrival. A paint that can't be copied keeps the last frame and is
  logged once until one succeeds. `use_device` also tells the helper the device's adapter
  LUID; without a device, or one without a LUID, pages deliver no frames.
- **On Linux** (`cef/cpu.rs`, ADR-0095): the helper is `jstream-browser-helper`, started
  with `--jstream-frames=<folder>` and one end of a Unix socket pair as its standard
  input; the link runs over the other end, so there are no names and no peer check. The
  folder is made fresh for each helper, mode 0700, under `$XDG_RUNTIME_DIR`, else
  `/dev/shm`, else the temp folder, and removed with everything in it once the helper
  has stopped. Each paint names a slot file there; the engine reads the whole frame from
  it (a size larger than the page, a slot past the third, a missing or short file is
  skipped, keeping the last frame, and logged once) into a new RGBA8 CPU frame, BT.709
  full range, stamped on arrival, then releases the slot. `use_device` does nothing, and
  no adapter is sent.

**The helper's side** (`helper/`, Windows and Linux, feature `helper`, which only
`jstream-browser-helper` turns on; it builds CEF with its sandbox):
- `run_helper` is all the helper DLL's `RunWinMain` does on Windows, and all the
  helper executable's `main` does on Linux. In a CEF subprocess it hands
  the process to CEF. In the browser process it connects to the pipes (on Linux it
  takes the socket that is its standard input and puts `/dev/null` in its place, so
  CEF's subprocesses never inherit it), starts CEF with
  the sandbox on, windowless rendering, profiles in memory (no `cache_path`, no persisted
  cookies) and warnings-only logging, and runs CEF's message loop. The engine's messages
  are read on a thread of their own; a new adapter and shutting down go to CEF's UI
  thread. The engine's `Shutdown`, its pipe closing, or a write to it failing closes
  every page, and the loop quits once they have closed, or after ten seconds.
- Every page is a windowless browser, with shared textures on Windows and software
  paints on Linux, painting at its spec's frame rate with a transparent background.
  Command-line switches let pages play sound without a click and turn off first-run and
  error dialogs, and on Linux with neither `DISPLAY` nor `WAYLAND_DISPLAY` set, start
  Chromium on its headless platform. On Linux Chromium also keeps its in-memory
  profiles' key itself (`--password-store=basic`), never opening the desktop's keyring.
  Pop-ups are blocked. Every
  permission a page asks for (camera, microphone, screen capture, geolocation,
  notifications, clipboard, MIDI and the rest) is refused by the page's permission
  handler rather than left to CEF's default: a media request is cancelled and a
  permission prompt answered `DENY`. The one exception is Chromium's Local Network
  Access prompt (CEF's `LOCAL_NETWORK`, `LOOPBACK_NETWORK` and the deprecated
  `LOCAL_NETWORK_ACCESS` bits) asked alone: it is answered `ACCEPT` when the page's
  `RequestPolicy` lets the prompt's requesting origin (opaque when CEF names none)
  make local requests, and `DENY` otherwise, a third-party frame's included. The
  pinned CEF never asks it in windowless pages: its checks refuse a public origin's
  local request outright. So for a source that opted in with `local_network`, the
  helper gives the page a request context of its own and sets Chromium's
  `LOCAL_NETWORK`, `LOOPBACK_NETWORK` and `LOCAL_NETWORK_ACCESS` content settings to
  allow for the source's own origin as the top-level origin (`local_network_grant`):
  it reaches an app on `127.0.0.1` from that origin, while a third-party frame inside
  it, and every other source, is still refused at the network layer, a public name
  that resolves to a local address included (#347). The Linux helper's end-to-end
  tests check all of this (`tests/linux_page/local_network.rs`). The answers come from
  `permission.rs` (`answer_prompt` and `answer_media`), whose tests pin them for every
  permission bit, and the helper checks the three bits against CEF's when it compiles.
  A page given as HTML is answered by the request handler (`HtmlPage`): its main
  frame's `GET` of its URL gets `200`, `text/html` in UTF-8, uncached (#1035). The
  custom CSS is applied when the main frame finishes loading and again on each change.
  Every request a page makes, navigations included, goes through its `RequestPolicy`
  in `OnBeforeResourceLoad`, as the origin `GetResourceRequestHandler` names for it
  (each request gets a handler that holds it), and is cancelled when refused; an
  allowed main-frame navigation updates where the main frame is. A redirect's target
  goes through it too, in `OnResourceRedirect`, which can't cancel: a refused one is pointed at
  `about:jstream-refused`, which loads nothing. The engine logs the first refusal per
  page at `warn` and later ones at `debug`, with the count only.
- **The page API** (`helper/page_api.rs`, ADR-0102). A granted page gets a request
  context of its own, so no other page's renderer process holds it, and its grant and
  `ApiHome` go to the renderer in the browser's `extra_info`. The browser process keeps
  each page's updates merged (`PageCache`); once the browser exists it sends the whole
  cache, then each update, as a `jstream.page_data` process message to the main frame,
  and the whole cache again whenever the main frame starts loading a document, which
  may be in a renderer process that has none of it.
  In the renderer, the render-process handler keeps each granted browser's grant, home
  and cache; when a JavaScript context is made in a main frame whose URL the home
  covers, it evaluates the shim before any page script runs, feeds it the cache, and
  feeds it each later update, until that context is released (only that one: an old
  document's context can go after the new one's is made). A cross-origin navigation makes the renderer's new browser object before it destroys the old one with
  the same ID, so the entry is counted and goes only with the last. The shim reports
  only what changed, so the state sent again fires no events. A subframe, a popup, or a main frame on any other
  document gets no `window.jstream`. It takes messages only from the browser process,
  and the client drops every process message a renderer sends, so nothing reaches the
  engine from a page.
- **Frames** (`helper/d3d11.rs`): a D3D11 device on the engine's adapter. Each
  accelerated paint's texture is opened, checked to be single-mip BGRA8, and copied into
  the next of three textures the page shares with the engine through NT handles; the
  copy is waited for before the callback returns, since CEF takes its texture back then.
  A slot the engine hasn't released isn't painted into: with all three held, the paint is
  skipped. Popup-widget paints are ignored.
- **Frames on Linux** (`helper/shm.rs`): each software paint is converted to straight
  RGBA8 and written into the next free of the page's three slot files (mode 0600) in the
  engine's folder, with the same release and skip rules; the page's files are removed
  when its browser closes. CEF failing to start reports that it needs `libcef.so` beside
  the helper and a sandbox that can start (not root; user namespaces or a setuid
  `chrome-sandbox`).
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

**The Linux check.** `tests/linux_page.rs` (feature `cef`, ignored by default) opens a
file page through the real helper named by `JSTREAM_BROWSER_HELPER`, as `cargo xtask
browser-linux` lays it out, and waits for a paint whose left half is opaque red and
right half transparent, then shuts the helper down. A second test opens a page granted
the page API, which paints one colour without `window.jstream`, another with it and
no data, a third once an update has arrived (and a fourth if a member it wasn't
granted exists), and checks the update survives a reload. CI's `browser-linux` job
runs both, sandboxed, headless and without a GPU.

<a id="open-items"></a>
## Open items
<!-- @section
depends_on = ["docs/adr/0021-browser-source.md", "docs/adr/0046-browser-crate-and-helper.md", "docs/adr/0048-sandboxed-browser-helper.md", "docs/adr/0095-browser-helper-on-linux.md"]
-->

What ADR-0021 asks for that this crate doesn't do yet.

- **Updates.** Nothing checks the pinned CEF against CEF stable. (The portable zip ships
  the helper and CEF's runtime, with `CREDITS.html` and the CEF version:
  [ARCH › Packaging](../../docs/ARCHITECTURE.md#packaging).)
- **Measurements.** Spike S4 measured CEF in the engine's process; the sandboxed
  helper's frames take a second copy and a pipe round trip, which haven't been measured.
- **Hosts.** The desktop app and the daemon each create a backend with their `cef`
  feature, on a device of the media adapter they open
  ([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)); the engine
  replaces it with the program stage's context while the stage runs on the GPU path,
  and the host's device is not given back when the stage stops or leaves the GPU path
  mid-run.
- **Compositing.** The program mix draws a page's `GpuFrame`, imported premultiplied
  on the stage's context, but only the fake backend's textures have been through it:
  a real CEF texture on Windows hardware is #336.
- **Settings and interaction.** "Shut down when not visible", "refresh when shown" and
  `source.interact` need the source-visibility hook and a protocol action. No page gets
  control-API access, and the per-source opt-in doesn't exist.
- **Linux.** Frames are copied on the CPU (ADR-0095): CEF's accelerated paint shared as
  DMA-BUFs onto the engine's Vulkan device would avoid the copies on a machine with a
  GPU, and neither path has been measured. The desktop app has no Linux build, so only
  the daemon wires the Linux backend.
- **Audio timestamps.** Packets are stamped when they reach the engine; if CEF's `pts` is
  QPC time, as S4 suggests, the helper could send it and the engine use it instead.
