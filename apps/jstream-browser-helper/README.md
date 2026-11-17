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
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*", "crates/jstream-browser/README.md#purpose", "docs/adr/0048-sandboxed-browser-helper.md", "docs/adr/0095-browser-helper-on-linux.md", "docs/adr/0157-browser-sources-on-macos.md"]
-->

The sandboxed browser helper
([ADR-0048](../../docs/adr/0048-sandboxed-browser-helper.md)): on Windows a DLL, not an
exe. It
ships as `jstream-browser-helper.dll` (Cargo builds it as `jstream_browser_helper.dll`)
beside CEF's `bootstrap.exe` renamed `jstream-browser-helper.exe`, both next to CEF's
runtime files. The CEF backend in
[`jstream-browser`](../../crates/jstream-browser/README.md) starts that exe; the bootstrap
sets up Chromium's sandbox and calls the DLL's `RunWinMain` export, which hands the
process to `jstream_browser::run_helper`. The process becomes CEF's browser process for
the engine's pages, and every renderer, GPU and utility process starts as the same exe,
sandboxed. The bootstrap brings its own manifest. If releases are signed, the exe and the
DLL must be signed with the same certificate.

The export is the DLL's only code. It is unmangled (`#[unsafe(no_mangle)]`), so the
crate declares `unsafe_allowed` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)).

On Linux ([ADR-0095](../../docs/adr/0095-browser-helper-on-linux.md)) the helper is the
crate's executable, `jstream-browser-helper` (`src/main.rs`), whose `main` hands the
process to `jstream_browser::run_helper`. It ships with `libcef.so`, CEF's runtime files
and `chrome-sandbox` beside it, and finds `libcef.so` there through its `$ORIGIN` run
path (`build.rs`). Chromium sandboxes the subprocesses itself; there is no bootstrap.
`cargo xtask browser-linux` builds it and lays out the `browser` folder. On Windows the
executable does nothing, and the zip builds the DLL alone (`--lib`), as the two would
share a PDB name.

On macOS ([ADR-0157](../../docs/adr/0157-browser-sources-on-macos.md)) it is the same
executable, in `JStream.app/Contents/MacOS` beside the app, with CEF's framework in
`Contents/Frameworks` and copies of itself as the helper apps there (`JStream Helper.app`
and its GPU, renderer, plugin and alerts variants), which CEF's subprocesses run from.
Each process loads the framework itself and each subprocess enters CEF's sandbox first
(`jstream_browser::run_helper` does both). `scripts/mac/build-browser-helper.sh` builds it
and `scripts/mac/bundle-app.sh` lays it out and signs it ad hoc. There is no run path
to set: the framework is loaded at run time. `build.rs` links the C++ standard library,
which the framework loader and sandbox entry compiled into `cef-dll-sys` need.

Like `jstream-browser`, it builds CEF only with its `cef` feature (Windows, Linux and macOS),
which turns on `jstream-browser`'s `helper` feature; without it, the DLL exports nothing
and there is no executable. `dox` compile-checks it without building CEF.
