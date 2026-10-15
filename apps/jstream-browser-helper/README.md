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
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*", "crates/jstream-browser/README.md#purpose"]
verified = { hash = "b3:60430623076dfd2b", by = "agent:claude@session_01YKRrREb5gUVhTU3pzaRVW8", note = "Stale via ARCH crate-map only (C8b4 B1: apps row now says only the daemon links jstream-browser, desktop wiring is #290); this section's own text makes no claim about that and holds" }
-->

CEF's subprocess exe for browser sources
([ADR-0046](../../docs/adr/0046-browser-crate-and-helper.md)). The CEF backend in
[`jstream-browser`](../../crates/jstream-browser/README.md) sets it as
`browser_subprocess_path`, so Chromium's renderer, GPU and utility processes start as
`jstream-browser-helper.exe` and the engine's own exe never does. It must sit next to the
host's exe.

Its `main` hands the process to CEF (`jstream_browser::run_subprocess`) and exits with
the code CEF returns. It is a GUI-subsystem exe, so no console window opens per
subprocess, and `build.rs` embeds `jstream-browser-helper.exe.manifest`, whose Windows 10 and 11
compatibility entry lets Chromium read the real Windows version.

Like `jstream-browser`, it builds CEF only with its `cef` feature (Windows). Without it,
it does nothing and exits with 1; `dox` compile-checks it without building CEF.
