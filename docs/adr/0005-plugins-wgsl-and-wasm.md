<!-- @doc
id = "ADR-0005"
title = "Extensibility through WGSL effects and WASM plugins"
kind = "adr"
status = "superseded"
superseded_by = "docs/adr/0035-clap-plugins-out-of-process.md"
-->

# ADR-0005: Extensibility through WGSL effects and WASM plugins

<!-- @generated begin adr-status -->
> **Superseded** by [ADR-0035: CLAP audio plugins, out of process](0035-clap-plugins-out-of-process.md). This decision no longer applies.
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Native dynamic plugins (OBS-style) need a stable ABI and can crash the process.

<a id="decision"></a>
## Decision
Built-in sources and effects are Rust code in the workspace. Third-party extensions are
either (1) WGSL effect packs with an `effect.toml` manifest, hot-reloadable, or
(2) sandboxed WASM plugins ([wasmtime](https://wasmtime.dev/)) for automation and logic,
which call the API through `Control`. No native dynamic plugins in v1.

<a id="consequences"></a>
## Consequences
- Plugins can't crash the engine, and there is no ABI to keep stable.
- Third parties can't add capture devices or encoders until a later ADR allows it.
