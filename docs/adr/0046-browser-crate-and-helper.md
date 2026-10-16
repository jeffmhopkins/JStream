<!-- @doc
id = "ADR-0046"
title = "Browser sources live in their own crate, with a helper exe"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0046: Browser sources live in their own crate, with a helper exe

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0021](0021-browser-source.md) chose CEF off-screen rendering for browser sources and
said where the code lives: the CEF bindings and backend "live in the platform crates
(`jstream-platform-windows` first), under the `unsafe` rules there". Spike S4
([ARCH › Browser source](../ARCHITECTURE.md#browser-source)) then picked the bindings:
the `cef` crate, whose sys crate downloads the CEF binary distribution (about 400 MB)
and builds CEF's C++ wrapper with CMake and Ninja in its build script.

Putting that dependency in `jstream-platform-windows` would make every Windows build of
the engine, the daemon and the desktop app download and build CEF, CI's included, and
tie the platform crate's build to a CMake toolchain it otherwise doesn't need. Very
little of the backend is Windows-specific: the CEF API, the audio hand-off and the
custom CSS are the same on every platform CEF supports, and only opening the shared
texture on the GPU device differs. Where the code lives is architecture, which is agent
tier ([ADR-0043](0043-narrow-maintainer-tier.md)); everything else in ADR-0021 stands.

<a id="decision"></a>
## Decision
This ADR partially supersedes ADR-0021: it replaces only its "Where the code lives"
bullet.

1. **A crate of its own.** `jstream-browser` (layer 3) holds the `BrowserBackend` seam
   that `jstream-sources` opens pages through, the page's audio hand-off and the custom
   CSS script, and the CEF backend. `jstream-sources` gains the `browser` kind with the
   backend injected through its `Context`, as ADR-0021 says; the engine
   (`jstream-core`) gets the backend from its host and records each page's audio as a
   mixer input.
2. **CEF only behind a feature.** The CEF backend is behind the crate's `cef` feature,
   off by default, so no build downloads or builds CEF unless a host asks for it. A
   `dox` feature type-checks the backend against the bindings without the native build,
   and CI runs clippy over it on Windows that way. Hosts turn on `cef` when they ship
   browser sources.
3. **`unsafe` in that crate.** The backend needs `unsafe` to open CEF's shared texture on
   the engine's D3D12 device and to read the audio handler's planar pointers.
   `jstream-browser` joins the crates [R-UNSAFE-001](../RULES.md#R-UNSAFE-001) allows,
   with every block justified ([R-UNSAFE-002](../RULES.md#R-UNSAFE-002)).
4. **A helper exe.** `apps/jstream-browser-helper` builds `jstream-browser-helper.exe`,
   CEF's `browser_subprocess_path`, as S4 chose, so the engine's own exe never starts as
   a Chromium subprocess.
5. **The other platforms.** When the macOS and Linux platform crates land, their texture
   import goes into this crate beside the Windows one, behind the same feature.

<a id="consequences"></a>
## Consequences
- Builds without the `cef` feature (CI's tests among them) neither download nor build
  CEF, and they get only the seam and a fake backend for tests. The CEF backend itself is
  only compile-checked in CI; it runs on real hardware.
- `jstream-browser` depends on `wgpu` directly for the shared-texture import, as the
  platform crates do.
- The CEF build-time dependencies (the downloader's TLS roots and archive code) bring two
  licences to the graph when the feature is on. They are build-time only, and the
  licence decision is the maintainer's ([R-DEP-001](../RULES.md#R-DEP-001)).
- ADR-0021's sandbox requirement is untouched: the backend runs CEF unsandboxed today,
  as S4 did, so no host may ship it until the sandbox is adopted or a superseding ADR
  accepts running without it.
