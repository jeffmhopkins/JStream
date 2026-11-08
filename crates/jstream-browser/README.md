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

This README is the crate's map. How each part behaves (limits, timeouts, threads, the
request and permission rules) is in its module's rustdoc, next to the code:
`cargo doc -p jstream-browser --features fake --document-private-items --open`.

**The seam.** `BrowserBackend` is what `jstream-sources` opens pages through, injected
into its `Context` by the engine (`Engine::with_browser`). `open` takes a `PageSpec` and
returns a `BrowserPage`: its latest frame, its audio, its custom CSS, the page API data
the engine sends it, and how many of its requests were refused. `use_device` gives the
backend a GPU device to copy the pages' paints onto: the host's at first, and while the
program stage runs, the stage's own context, which the engine lends it and takes back
([jstream-core › Design](../jstream-core/README.md#design), *Live sources*). A page never
hands out a frame copied onto an earlier device. Without a device a page renders and
plays but delivers no frames, unless it `paints_on_cpu` (CEF on Linux), whose latest paint
is its `cpu_frame`. Dropping the page closes it.

**What a page may do.** Every request a page makes, navigations and redirects included,
goes through its `RequestPolicy` (`request.rs`), which allows web URLs, refuses a page
from the internet any request to this computer or the local network unless its source
opted in, and allows `file:` only inside a local file page's own folder. Every permission
a page asks for is refused (`permission.rs`), but for the Local Network Access prompt the
policy decides. Pop-ups are blocked, and nothing a page sends reaches the engine.

| Part | Module | Built | What it is |
|---|---|---|---|
| `PageLocation`, `PageSpec`, `css_script` | `page.rs` | everywhere | where a page loads from (a web address, a local file, or HTML with an origin of its own), its size, rate and custom CSS |
| `PageAudio` | `audio.rs` | everywhere | a page's audio, 48 kHz stereo `f32`, stamped on the media clock |
| `RequestPolicy` | `request.rs` | everywhere | what a page may request (ADR-0021, #347, #721, #1035) |
| `answer_prompt`, `answer_media` | `permission.rs` | everywhere | what a page gets when it asks for a permission |
| `PageApi`, `PageData`, `ApiHome`, `shim` | `page_api.rs`, `page_api.js` | everywhere | the page API's shared parts ([ADR-0102](../../docs/adr/0102-page-api-for-browser-sources.md)) |
| (the pipe) | `wire.rs`, `link.rs` | everywhere | the messages between the engine and the helper, and the engine's end of the link |
| (CPU frames) | `cpu_frames.rs` | Linux, tests | slot files and the premultiplied-to-straight conversion |
| `FakeBrowser` | `fake.rs` | feature `fake` | an in-memory backend for tests |
| `CefBackend`, `CefConfig` | `cef/` | feature `cef`, Windows and Linux | starts and drives the helper: named pipes and shared textures on Windows, a socket pair and slot files on Linux, a job object so no helper outlives the engine |
| `run_helper` | `helper/` | feature `helper`, Windows and Linux | the helper's CEF code: windowless pages, the request and permission handlers, the page API in the renderer, frames and audio |
| (reload hold) | `reload_hold.rs` | feature `helper` | no paints from a reload until the new document paints for real (#1151, #1155) |

Only `jstream-browser-helper` turns on `helper`, so no other build downloads or compiles
CEF. The engine side builds no CEF; `dox` compile-checks it with the rest.

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
