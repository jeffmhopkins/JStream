<!-- @doc
id = "ADR-0095"
title = "Browser sources on Linux: the helper paints in software and hands the engine CPU frames"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0095: Browser sources on Linux: the helper paints in software and hands the engine CPU frames

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0021](0021-browser-source.md) chose CEF for browser sources and said the same
approach carries over to Linux, which CEF supports off screen.
[ADR-0060](0060-linux-platform-support.md) left the browser source on Linux for a later
decision. [ADR-0048](0048-sandboxed-browser-helper.md) runs CEF in a sandboxed helper
process that the engine drives over pipes, and is written for Windows: the helper is a
DLL that CEF's bootstrap exe loads, the pipes are named pipes, and frames cross as
D3D11 shared textures opened on the engine's D3D12 device.

None of that exists on Linux, and the first Linux hosts are often machines with no GPU at
all: a server, a container, a CI runner. The daemon there composites on its own software
device and has no platform GPU context to lend a page (`open_capture`). A GPU-only frame
path would leave those machines with blank browser sources. CEF's software paint
(`OnPaint`) works everywhere, with or without a GPU and with or without a display, and
hands over the page as BGRA pixels in CPU memory.

On Linux Chromium sandboxes its subprocesses itself, with unprivileged user namespaces or
the setuid `chrome-sandbox` beside the executable, so no bootstrap exe is needed, and the
browser process and every subprocess can be one ordinary executable.

This ADR is maintainer tier ([ADR-0043](0043-narrow-maintainer-tier.md)). It brings the
browser source into Linux's supported scope, which ADR-0060 held back as its own later
decision, and that is product scope. It also makes browser sources on some Linux systems
depend on a setuid-root `chrome-sandbox` that an install creates, and refuses root users,
which is security posture.

<a id="decision"></a>
## Decision
This ADR partially supersedes ADR-0048 and ADR-0021, on Linux only. It replaces
ADR-0048's first, second and third decisions there. It also replaces ADR-0021's
*Rendering* decision and its requirement that frames reach the compositor on the GPU
without a CPU copy: on Linux pages paint in software and frames cross on the CPU. The
rest of ADR-0021, and all of both ADRs on Windows, stand unchanged.

1. **The helper is an executable.** On Linux `jstream-browser-helper` is a plain
   executable, built from `apps/jstream-browser-helper` (its `bin`, with the `cef`
   feature), with `libcef.so`, CEF's runtime files and `chrome-sandbox` beside it. It is
   CEF's browser process, and every renderer, GPU and utility process starts as the same
   executable.
2. **The sandbox stays on.** CEF starts with `no_sandbox` off, as on Windows. Chromium uses
   unprivileged user namespaces where the kernel allows them, else the setuid
   `chrome-sandbox` beside the helper, which an install makes setuid root (Ubuntu 24.04's
   AppArmor policy, for one, needs that). Chromium refuses to run as root with the sandbox
   on, so the helper fails to start for a root user, and the error says why. There is no
   switch that turns the sandbox off.
3. **The engine drives it over a socket pair.** The backend makes a Unix socket pair and
   starts the helper with one end as its standard input. Nothing else can be on the line,
   so no name or peer check is needed. The helper takes the socket and puts `/dev/null` in
   its place before CEF starts, so no subprocess inherits it and the engine sees the
   helper's end close when it exits. The messages are ADR-0048's, unchanged.
4. **Frames cross as CPU frames through a private folder.** The backend makes a fresh
   folder for each helper under `$XDG_RUNTIME_DIR` (else `/dev/shm`, else the temp
   folder), with mode 0700, and removes it once the helper has stopped. Pages paint in
   software (`OnPaint`). The helper converts each paint to straight-alpha RGBA8 and writes
   it into the next free of three slot files per page, then sends the slot and the number
   that names its file. The engine reads the frame from that file into a new CPU frame,
   then releases the slot, as ADR-0048 releases a texture. The helper removes a page's
   files when the page closes.
5. **Browser sources show CPU frames.** `BrowserPage` gains `paints_on_cpu` and
   `cpu_frame`. A page that paints on the CPU gives the source its latest paint as the
   source's frame, and the source asks to be looked at again every frame of its frame
   rate. The compositor uploads it as it uploads any source's frame. A Linux backend
   needs no GPU device, so `use_device` does nothing there.
6. **No display needed.** With neither `DISPLAY` nor `WAYLAND_DISPLAY` set, the helper
   starts Chromium on its headless platform (`--ozone-platform=headless`).
7. **Packaging.** `cargo xtask browser-linux` builds the helper with CEF and lays out the
   `browser` folder a Linux host looks for beside its executable, as the Windows zip does.
   A Linux tarball or other package remains ADR-0060's later decision.

<a id="consequences"></a>
## Consequences
- Browser sources work on any Linux machine the daemon runs on, GPU or not, display or
  not, including CI, which builds the helper with CEF and opens a page through it end to
  end.
- Each frame is copied on the CPU: CEF's paint, the conversion, the slot file, the read,
  and the compositor's upload. A 1080p page at 30 frames a second moves about 250 MB a
  second through memory. That is fine for overlays. On a machine with a GPU, CEF's
  accelerated paint shared as DMA-BUFs would avoid the copies; that is a later decision
  for when it is measured.
- WebGL and other GPU work inside a page run on whatever Chromium finds, which on a
  machine without a GPU is its software fallback, and may be slow.
- A root user can't run browser sources. Containers need a normal user and either user
  namespaces or a setuid `chrome-sandbox`.
- The helper crate builds both a DLL (Windows) and an executable (Linux). On Windows the
  executable does nothing, and the zip builds the DLL alone.
