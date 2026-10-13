---
id: ADR-0005
title: "Extensibility through WGSL effects and WASM plugins"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ADR-0004, ext:wasmtime]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0005: Extensibility through WGSL effects and WASM plugins


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
