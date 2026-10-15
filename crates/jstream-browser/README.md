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
verified = { hash = "b3:7dab300ad8693389", by = "agent:claude@session_01E4Mb9bbGDC2GsaQqW2Vb3L", note = "C8b2 merge of C8 round 1: seam paragraph keeps this PR's denied_requests and takes C8's use_device wording (a device the host opens, for now); RequestPolicy bullet unchanged" }
-->

The engine side of browser sources ([ADR-0021](../../docs/adr/0021-browser-source.md)):
web pages rendered off screen by CEF, each page's frames as textures on the engine's GPU
device and its audio as a stream the engine records like a device's. Layer 3, its own
crate rather than part of a platform crate, so that only builds that ask for CEF
download and build it ([ADR-0046](../../docs/adr/0046-browser-crate-and-helper.md)).

**The seam.** `BrowserBackend` is what `jstream-sources` opens pages through, injected
into its `Context` by the engine (`Engine::with_browser`). `open` takes a `PageSpec` (a
location, a size, a frame-rate cap and the custom CSS) and returns a `BrowserPage`:
its latest frame as a `GpuFrame`, its audio, a way to replace its custom CSS while it
runs, and how many of its requests were refused (`denied_requests`). `use_device` gives
the backend a GPU device to copy the pages' paints onto. Today that is one the host
opens on the media adapter, since the engine doesn't hand out its own device yet;
without one a page renders and plays but delivers no frames. Dropping the
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
  it). CEF's planar packets are interleaved (mono doubled, channels past two dropped)
  and split into chunks of at most `MAX_CHUNK_FRAMES`. Each chunk is stamped on the
  media clock: its packet's arrival time less the packet's length, plus the chunk's
  offset, since CEF's own `pts` isn't on a documented clock yet
  ([ARCH › Browser source](../../docs/ARCHITECTURE.md#browser-source)). The audio
  thread never waits: a packet that meets the handler's lock while the engine attaches
  or detaches is dropped and counted (`dropped`), and the device position still counts
  it, so the mixer sees the gap.
- `RequestPolicy` (`request.rs`): what a page may request, from its location (ADR-0021:
  no file access beyond the source's own file). Web URLs (`http`, `https`, `ws`,
  `wss`), `data:`, `blob:`, `about:blank` and `about:srcdoc` are allowed for every
  page. `file:` URLs are allowed only for a page loaded from a local file, and only for
  files in its folder or below. The path is compared by name first, with `..` applied,
  and only a path inside the folder is ever opened, so a URL naming another server (an
  SMB connection on Windows) or a device is refused untouched. It is compared again
  after resolving links on both sides, so a link inside the folder can't reach out; a
  missing file is refused. Every other scheme is refused. Each refusal is counted. The
  folder is trusted as an asset root is: something that can write in it could swap a
  file for a link between the check and the load.
- `fake` (feature `fake`, and in tests): `FakeBrowser`, a backend that records the
  pages it opens, their CSS changes and whether they were closed, never paints, and can
  be told to refuse. `FakePageState::request` plays a request through the page's
  policy.

**The CEF backend** (`cef/`, Windows and feature `cef` only; `dox` compile-checks it
without building CEF):
- `CefBackend::new` takes a `CefConfig` (the helper exe, the cache root and an optional
  log file; `CefConfig::beside_exe` puts the helper next to the running exe and the
  cache under the host's state folder) and the media clock. CEF starts on the first
  `open`, on a `jstream-cef` thread that runs its message loop with windowless rendering,
  `jstream-browser-helper.exe` as `browser_subprocess_path`, profiles in memory (no
  `cache_path`, no persisted cookies) and warnings-only logging. Dropping the backend
  quits the loop once every page has closed, or after ten seconds, and shuts CEF down on
  that thread. A missing helper exe fails the first `open` with `BrowserError::Start`.
- Every page is a windowless browser with shared textures on, painting at its spec's
  frame rate with a transparent background. Command-line switches let pages play sound
  without a click and turn off first-run and error dialogs. Pop-ups are blocked. The
  custom CSS is applied when the main frame finishes loading and again on each change.
  Every request a page makes, navigations included, goes through its `RequestPolicy`
  in `OnBeforeResourceLoad` and is cancelled when refused; the first refusal is logged
  at `warn` and later ones at `debug`, with the reason and count but never the URL,
  which may be a local path.
- **Frames** (`cef/gpu.rs`): each accelerated paint's shared texture is opened on the
  engine's D3D12 device (`OpenSharedHandle`), checked to be single-mip BGRA8, imported
  into wgpu, copied into the next of three textures the page owns, and waited for before
  the callback returns, since CEF takes the texture back then. Nothing is read back to
  the CPU. The frame is `Bgra8`, premultiplied, BT.709 full range, stamped with the
  paint's time on the media clock. A paint that can't be copied keeps the last frame and
  is logged once until one succeeds. Popup-widget paints are ignored.
- **Audio** (`cef/handlers.rs`): the audio handler asks for 48 kHz stereo in 10 ms
  packets and forwards each to the page's `PageAudio`. A stream error fails the
  attached input, which the engine reopens, except while the page is closing, when
  "Socket closed unexpectedly" is CEF's normal end of the stream.
- `run_subprocess` is the helper exe's whole `main`.

**The hardware check.** `examples/page_check.rs` opens the built-in test page (or
`--url`) through the CEF backend on the engine's own device and QPC clock, as the daemon
will, and reports the frames and audio that arrive, a custom-CSS change the page reacts
to, the requests refused (the built-in page loads one image from outside its folder),
and the shutdown time. It runs on Windows with the `cef` feature only.

<a id="open-items"></a>
## Open items
<!-- @section
depends_on = ["docs/adr/0021-browser-source.md", "docs/adr/0046-browser-crate-and-helper.md"]
verified = { hash = "b3:661681ac38fa86e7", by = "agent:claude@session_01E4Mb9bbGDC2GsaQqW2Vb3L", note = "C8b2: file-access item removed now that the request policy limits file pages to their folder; the sandbox and packaging items still block shipping" }
-->

What ADR-0021 asks for that this crate doesn't do yet. The first two block any host
from turning on the `cef` feature.

- **Sandbox.** CEF runs unsandboxed (`no_sandbox`), as spike S4 did. ADR-0021 requires
  the sandbox where supported; on Windows that means CEF's bootstrap layout, which may
  replace the helper exe, unless a superseding ADR accepts running without it.
- **Packaging and updates.** The CEF runtime isn't in the portable zip yet, with its
  licence notices, and nothing checks the pinned CEF against CEF stable.
- **Hosts.** The daemon and desktop app don't create a backend or call
  `Engine::with_browser`, and nothing hands the backend the engine's device.
- **Compositing.** The program mix doesn't draw a page's `GpuFrame` yet; a `browser`
  source draws a transparent frame of its size until the render import of GPU frames
  lands.
- **Settings and interaction.** "Shut down when not visible", "refresh when shown" and
  `source.interact` need the source-visibility hook and a protocol action. No page gets
  control-API access, and the per-source opt-in doesn't exist.
- **Audio timestamps.** Packets are stamped on arrival; if CEF's `pts` is QPC time, as S4
  suggests, it could replace that.
