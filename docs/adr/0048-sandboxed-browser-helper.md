<!-- @doc
id = "ADR-0048"
title = "Browser pages run in a sandboxed helper process"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0048: Browser pages run in a sandboxed helper process

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0021](0021-browser-source.md) requires CEF to run sandboxed where supported.
[ADR-0046](0046-browser-crate-and-helper.md) put CEF's browser process inside the
engine's own process, with `jstream-browser-helper.exe` as the exe for CEF's
subprocesses only, as spike S4 measured. That layout can't be sandboxed on Windows:
- Since CEF 138, the Windows sandbox is available to applications built outside
  Chromium's tree only through CEF's bootstrap exe. The bootstrap exe sets up the
  sandbox and then loads the application as a DLL of the same name, calling its exported
  `RunWinMain` with the sandbox information.
- With the sandbox on, the browser process and every subprocess must be the same exe:
  `browser_subprocess_path` can't be used.

So with the sandbox, CEF's browser process has to be the bootstrap exe. Making the
daemon and the desktop app into bootstrap-loaded DLLs would change both hosts and the
portable zip for one source kind. The maintainer chose to run pages in a sandboxed
helper process over accepting no sandbox. This ADR carries out ADR-0021's sandbox
requirement and changes no security posture, so it is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
This ADR partially supersedes ADR-0046. It replaces ADR-0046's fourth decision (the
helper exe as `browser_subprocess_path`) and narrows its second (which feature builds
CEF). The rest of ADR-0046 stands.

1. **CEF runs in a helper process.** `jstream-browser-helper.exe` is CEF's bootstrap
   exe, renamed. It loads `jstream-browser-helper.dll`, built from
   `apps/jstream-browser-helper` as a `cdylib` that exports `RunWinMain`. That process
   is CEF's browser process, and every renderer, GPU and utility process starts as the
   same exe with the sandbox on. No CEF code runs in the engine's process, so pages
   share no process with stream keys or credentials.
2. **The engine drives it over pipes.** `jstream-browser`'s CEF backend starts one
   helper and talks to it over two private named pipes, one each way: open a page, set
   its CSS, close it, use this GPU adapter, shut down. The helper sends back page events,
   frames, audio and refused-request counts. Each pipe has a single instance, and the
   backend checks that the process connected to it is the helper it started. The helper
   exits when the engine's pipe closes, so it never outlives the engine. A helper that
   stops fails its pages, and the next page starts a new one.
3. **Frames cross as shared textures.** The helper opens a D3D11 device on the engine's
   adapter (by LUID). Inside each accelerated paint it copies CEF's texture into the next
   free one of three shared NT-handle textures it owns for the page, waits for the copy,
   and sends the texture's slot and handle. The engine duplicates the handle out of the
   helper's process, opens it once on its D3D12 device, copies it into its own texture as
   before, and releases the slot. Nothing is read back to the CPU.
4. **Audio crosses the pipe** as interleaved 48 kHz stereo `f32`. The engine stamps
   frames and audio on its own media clock as they arrive; the pipe carries no times.
5. **The request policy runs in the helper**, from the page's location the engine sends.
   The engine gets refusal counts, never URLs.
6. **Features.** `jstream-browser`'s `cef` feature is the engine's side (the backend,
   pipes and texture import) and no longer builds CEF. Its `helper` feature is CEF's side
   (with the `cef` crate's `sandbox` feature) and only the helper crate turns it on, so
   only the helper's build downloads and builds CEF.
7. **`unsafe` in the helper crate.** The `RunWinMain` export must be unmangled, which the
   `unsafe_code` lint counts as `unsafe`, so `jstream-browser-helper` joins the crates
   [R-UNSAFE-001](../RULES.md#R-UNSAFE-001) allows. The export is its only code.
8. **Packaging.** The bootstrap exe comes from the pinned CEF distribution and is copied
   and renamed when the app is packaged, beside the renamed DLL and CEF's runtime files.
   If releases sign their binaries, the exe and the DLL must be signed with the same
   certificate.

<a id="consequences"></a>
## Consequences
- Pages run sandboxed, and the engine's process loads no CEF code. Hosts that turn on
  `cef` no longer build CEF.
- Each frame costs a second GPU copy (CEF's texture to the helper's, then to the
  engine's) and a pipe round trip. Spike S4's measurements need a re-run on this path.
- Frames and audio are stamped when they reach the engine, a little later than when
  they were made.
- The pipe protocol lives in `jstream-browser` and is tested without CEF over in-memory
  pipes; the helper and the import still only run on real hardware.
- The helper crate is a `cdylib`, not an exe: the shipped exe is CEF's own bootstrap,
  which brings its own manifest.
