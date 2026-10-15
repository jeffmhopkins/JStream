<!-- @doc
id = "SPIKE-S4"
title = "Spike S4: CEF browser source"
kind = "guide"
status = "draft"
review_every = "180d"
-->

# Spike S4: CEF browser source

Throwaway code for spike S4 in [ROADMAP M0](../../ROADMAP.md#m0), Track B. It checks the two
things [ADR-0021](../../docs/adr/0021-browser-source.md) says must be proven before the
browser source is built:

- a transparent 1080p60 page reaches the compositor through CEF's accelerated paint, as a
  shared D3D texture opened in wgpu, with no CPU copy of frame data;
- the page's audio arrives per browser through CEF's audio handler.

It also picks the Rust CEF bindings and the helper-process layout, and records what the
CEF runtime weighs, which codecs its pages can play and which notices it ships with.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`. The maintainer's Windows run passed.

Results don't go in this file. They are recorded in
[ARCH](../../docs/ARCHITECTURE.md#browser-source), and would go in a new ADR if they changed
ADR-0021's decision. Accepted ADRs are never edited
([R-DOC-009](../../docs/RULES.md#R-DOC-009)).

<a id="pipeline"></a>
## What it does
<!-- @section
-->

One browser, off-screen, on a built-in test page (or `--url`), for a warm-up and then a
measured window. CEF's UI thread is the main thread. Each paint does this:

1. **Accelerated paint:** CEF calls `OnAcceleratedPaint` with an NT handle to a shared
   D3D11 texture (BGRA8, premultiplied alpha, no keyed mutex) from its GPU process. CEF
   documents that the texture goes back to its pool when the callback returns.
2. **Open on wgpu's device:** `ID3D12Device::OpenSharedHandle` on wgpu's own D3D12 device,
   then `wgpu::hal::dx12::Device::texture_from_raw` and `create_texture_from_hal`, the
   same import as spike S1. A shared texture only opens on the adapter that made it, so the
   spike picks DXGI adapter 0, which Chromium uses by default. If the open fails, the
   report names the adapter that can open it.
3. **Copy out, inside the callback:** one GPU `copy_texture_to_texture` into a texture the
   source owns, submitted and waited for before the callback returns, so CEF can reuse
   its texture right away. This is the cost a real source pays per frame.
4. **Composite:** a render pass blends the source's copy over an opaque blue background
   (premultiplied "over"), as the compositor would.
5. **Probe readback:** only for the checks, one 640-pixel row of the page copy and of the
   composite is read back to the CPU. Frame data never is.

The test page draws, on that row, a 16-bit counter that it bumps on every
`requestAnimationFrame`, an opaque red cell, a 50 % green cell and a cell it never draws.
From those the spike counts page frames that were skipped or painted twice, and checks
that the page arrives transparent and blends correctly. Below the row a box moves, so every
frame changes.

**Audio:** the page plays 440 Hz on the left channel and 660 Hz on the right through
WebAudio, at -12 dBFS. The audio handler asks for 48 kHz stereo in 480-frame packets, and
the spike measures packet timing, the sample rate seen through `pts` and through the wall
clock, and each channel's level and pitch, which shows that the channels arrive in order.

**Also measured:** CPU and memory of the browser process and every CEF subprocess, the
size of the CEF runtime next to the exe and its file list, the user agent, and the codecs
the page's `canPlayType` and `MediaSource.isTypeSupported` accept.

<a id="choices"></a>
## Bindings and helper processes
<!-- @section
-->

**Bindings: `cef` from tauri-apps/cef-rs**, pinned to `=154.5.0` (CEF 154.0.34, Chromium
154.0.8037.98). It is generated from CEF's C API, released for each CEF version, MIT OR
Apache-2.0, and actively maintained. Its `cef-dll-sys` build script downloads the matching
CEF binary distribution from CEF's build server, builds CEF's C++ wrapper with CMake and
Ninja, and copies the runtime next to the exe. The spike turns its default features off:
`sandbox` needs the bootstrap layout below, and the bundling helpers aren't needed. Its
optional `accelerated_osr` texture import isn't used either: it wraps CEF's texture in
wgpu without copying it out of CEF's pool before the callback returns. The other options
were hand-written FFI to CEF's C API, which means hundreds of structs to update on every
CEF release, and older crates that stopped tracking CEF.

For the product this is a native C++ build dependency: the `links = "cef_dll_wrapper"` key
and the C++ build script need a lint-deps allowlist entry and a declared governance change
([R-DEP-002](../../docs/RULES.md#R-DEP-002)), as ADR-0021 already says.

**Helper processes.** CEF runs Chromium's renderer, GPU and utility processes as separate
processes. The spike runs both layouts ADR-0021 names, selected with `--helper`:

- **`same`:** CEF starts `s4-browser.exe` again with `--type=...`. The first thing `main`
  does is hand those processes to CEF, so nothing else runs in them. One exe, but every
  subprocess maps the whole engine exe.
- **`separate`:** `browser_subprocess_path` points at `s4-browser-helper.exe`, a small exe
  that only calls CEF. The engine exe never runs as a subprocess.

**Sandbox:** both layouts run with `no_sandbox`. On Windows, current CEF sandboxes its
subprocesses only in its bootstrap layout: CEF's own `bootstrap.exe` (or `bootstrapc.exe`)
is the exe, renamed, and loads the application as a DLL that exports `RunWinMain`. The
spike doesn't test that layout. ADR-0021 asks for the sandbox where supported, so the
browser source has to choose between that layout and running unsandboxed.

<a id="run"></a>
## How to run it (Windows)
<!-- @section
-->

You need Windows 10 or 11, the MSVC C++ build tools, Rust (both installed by
`spikes\run-spikes.bat`), CMake and Ninja (installed by the script below if missing), and
about 1.5 GB of disk for the CEF download and build. You don't need admin rights, and
nothing appears on screen.

Double-click `spikes\s4-browser\run-s4.bat`. It loads the MSVC environment, builds the
spike, runs it three ways for 30 s each (`same`, `separate`, and a 3840x2160 run), and zips
the JSON reports, stderr logs, CEF's own logs, the runtime's file list and the machine's
details into `spikes\s4-browser\s4-out\`. Use `-Seconds 60` for longer windows, or
`-Skip4K`.

By hand, from a "x64 Native Tools" prompt:

```powershell
cd spikes\s4-browser
cargo build --release
target\release\s4-browser.exe --json
```

Other options are `--seconds`, `--warmup`, `--size <WxH>`, `--fps`, `--helper same|separate`,
`--url <URL>` and `--cef-log <PATH>`; `--help` lists them. Run it from any folder: CEF finds
its runtime next to the exe.

On stderr the spike prints an `s4 stage` line at each step: GPU, CEF initialized, browser
created, first paint, audio stream started, window start and end, shutdown. A crash in the
browser process prints an `s4 FAULT` line, as in spike S1.

<a id="paste-back"></a>
## What to paste back
<!-- @section
-->

The zip from `run-s4.bat`, or its three JSON lines and `notes.md`. What the fields answer:

- **Accelerated paint works:** `video.accelerated_paints` > 0, `software_paints` = 0 and
  `failed_paints` = 0. A failure has `first_error`, which names the adapter CEF's texture
  belongs to.
- **1080p60:** `video.paint_fps` near 60, `paint_interval_ms` p95 near 16.7, and
  `page_frames.skipped` and `repeats` near 0.
- **The cost per frame:** `open_ms` plus `copy_and_wait_ms` is the time the callback
  holds CEF's UI thread; `callback_ms` adds the spike's own composite and probe readback.
- **Transparent, no CPU copy:** in `probes`, `page_clear_ok`, `page_half_alpha_ok`,
  `composite_clear_ok` and `composite_blend_ok` equal `frames`. `page_premultiplied` says
  whether the colours arrive premultiplied, as CEF documents. `last_rgba` shows the pixels.
- **Audio:** `audio.started` shows what CEF agreed to; `channels[0]` near 440 Hz and
  `channels[1]` near 660 Hz, both near -12 dBFS peak; `rate_by_wall_clock` near 48000.
- **Cost of the browser:** `processes` per process and in total; `runtime.mb` and
  `runtime.files` for the zip size and the notices (ADR-0021 expects 150–250 MB).
- **Codecs:** `page.report.codecs` (`canPlayType`: `""`, `"maybe"` or `"probably"`).
  ADR-0021 expects H.264 and AAC to be unsupported (`proprietary_codecs` off).
- **Layout:** compare `same` and `separate` for startup (`cef_initialize_ms`,
  `video.first_paint_ms`) and memory.

<a id="risks"></a>
## Known risks and uncertainties
<!-- @section
-->

This code was compile-checked and linted from Linux for `x86_64-pc-windows-msvc`, with
`cef`'s `dox` feature standing in for the CEF build, then run by the maintainer on one
AMD machine, where it passed ([ARCH](../../docs/ARCHITECTURE.md#browser-source)). These were
the risks before that run, roughly in order:

- **Building CEF's wrapper.** The first build needs CMake, Ninja and the MSVC environment
  in the shell; `run-s4.ps1` sets that up. CEF's CMake may want a newer CMake than an
  older install has.
- **Adapter match.** On a PC with two GPUs, Chromium's GPU process may render on another
  adapter than wgpu's, and the open fails. The error names the right adapter; a fix would
  pick wgpu's adapter to match, or tell Chromium which adapter to use.
- **Synchronisation.** CEF's texture has no keyed mutex and the callback gets no fence.
  The spike relies on Chromium having finished drawing before it calls back, as OBS does.
  Torn or stale frames would show up as `page_frames` going backwards or as wrong probes.
- **Resource state.** The opened texture is assumed to start in `COMMON` and is
  transitioned back to it after the copy, as in spike S1.
- **Audio threads.** CEF delivers packets on its own thread. If CEF ignores the requested
  480-frame packets, `packet_frames_min_max` shows what it used.
- **Not measured:** GPU-only timestamps (the times are wall clock and include queueing),
  pages with video, several browsers at once, input events and the sandboxed layout.
