<!-- @doc
id = "CRATE-jstream-browser-helper"
title = "jstream-browser-helper"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-browser-helper

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["apps/jstream-browser-helper/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*", "crates/jstream-browser/README.md#purpose", "docs/adr/0048-sandboxed-browser-helper.md"]
verified = { hash = "b3:bdca60cac80e476e", by = "agent:claude@session_01E4Mb9bbGDC2GsaQqW2Vb3L", note = "C8b1 round 1: stale only via the browser README's purpose; helper cdylib unchanged, text holds" }
-->

The sandboxed browser helper
([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)): a DLL, not an exe. It
ships as `jstream-browser-helper.dll` (Cargo builds it as `jstream_browser_helper.dll`)
beside CEF's `bootstrap.exe` renamed `jstream-browser-helper.exe`, both next to CEF's
runtime files. The CEF backend in
[`jstream-browser`](../../crates/jstream-browser/README.md) starts that exe; the bootstrap
sets up Chromium's sandbox and calls the DLL's `RunWinMain` export, which hands the
process to `jstream_browser::run_helper`. The process becomes CEF's browser process for
the engine's pages, and every renderer, GPU and utility process starts as the same exe,
sandboxed. The bootstrap brings its own manifest. If releases are signed, the exe and the
DLL must be signed with the same certificate.

The export is the crate's only code. It is unmangled (`#[unsafe(no_mangle)]`), so the
crate declares `unsafe_allowed` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)).

Like `jstream-browser`, it builds CEF only with its `cef` feature (Windows), which turns
on `jstream-browser`'s `helper` feature; without it, the DLL exports nothing. `dox`
compile-checks it without building CEF.
