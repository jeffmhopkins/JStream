# 0005: Extensibility through WGSL effects and WASM plugins

- Status: Accepted
- Date: 2026-10-13

## Context
Native dynamic plugins (OBS-style) need a stable ABI and can crash the process.

## Decision
Built-in sources/effects are Rust code in the workspace. Third-party extensions are:
(1) WGSL effect packs with an `effect.toml` manifest, hot-reloadable; and
(2) sandboxed WASM plugins (wasmtime) for automation and logic, using the protocol API.
No native dynamic plugins in v1.

## Consequences
- Plugins can't crash the engine or need an ABI.
- Third parties cannot add capture devices or encoders until a later ADR.
