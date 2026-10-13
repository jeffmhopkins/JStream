# Engineering rules

These rules apply to every change, whether a human or an agent wrote it. "MUST" rules are
enforced by CI or review. To change a rule, open a PR against this file and explain why.

## 1. Architecture

- **MUST** expose every new capability as a command, query or event in `jstream-protocol`
  first. UI, MCP and CLI features are built on the protocol, never beside it.
- **MUST** respect the crate layering in [ARCHITECTURE.md §3](ARCHITECTURE.md#crate-map)
  (`cargo xtask lint-deps`).
- **MUST NOT** make `jstream-ui` depend on anything but `jstream-protocol` and web crates.
- **MUST** record a decision that changes structure, a dependency boundary, the protocol's
  compatibility, or a platform minimum as an ADR in `docs/adr/`.
- New crates need a line in the crate map and a reason. Prefer a module in an existing crate.

## 2. Rust code

- Toolchain: pinned stable in `rust-toolchain.toml`, edition 2024. Bumps go in their own PR.
- **MUST** pass `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`
  on Windows, Linux and macOS.
- Errors: `thiserror` enums in libraries, `anyhow` only in `apps/*` and `xtask`.
  Errors carry context; no stringly-typed errors across crate boundaries.
- **MUST NOT** use `unwrap()`/`expect()` in non-test code, except `expect("invariant: …")`
  where the message states the invariant. (`clippy::unwrap_used` is denied.)
- **MUST NOT** use `println!`/`eprintln!` in libraries. Use `tracing`, with spans around
  commands, outputs and sessions.
- Public items in library crates have doc comments (`missing_docs` warns in L0–L2).
- Prefer plain data plus functions over trait hierarchies. Introduce a trait only at a real
  seam (platform, encoder, source kind, effect).
- No global mutable state. Things are passed explicitly or owned by `Engine`.

## 3. `unsafe` and FFI

- `unsafe` is allowed **only** in `jstream-platform-windows`, `jstream-platform-macos`,
  `jstream-platform-linux` and `*-sys` crates. Every other crate has `#![forbid(unsafe_code)]`.
- Every `unsafe` block has a `// SAFETY:` comment explaining why it is sound
  (`clippy::undocumented_unsafe_blocks` is denied).
- FFI wrappers expose safe APIs. Raw handles do not leak past the platform crate's public API.
- COM / Objective-C / C resources are wrapped in RAII types. No manual release calls in business logic.
- Prefer the official bindings (`windows` crate, `objc2-*`, `pipewire`) over hand-written `extern` blocks.

## 4. Real-time code (audio, render, encode threads)

- **MUST NOT** allocate, lock a contended mutex, block on I/O, or `.await` in steady state.
  Debug builds wrap the audio callback in `assert_no_alloc`.
- Communication with the control plane: lock-free SPSC/MPSC queues, `arc-swap` snapshots,
  triple buffers. No `Mutex` shared with tokio tasks.
- Overload is handled by **dropping and counting** (frames/packets), never by stalling the clock.
  Drops are reported in `stats.tick`.
- Changes to RT code include a note on their per-frame cost, and a benchmark if they touch the hot loop.

## 5. Dependencies

- Licenses: MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode, MPL-2.0 (file-level) only.
  No GPL/LGPL/AGPL, no unlicensed crates (`cargo deny check`).
- **No FFmpeg** and no bundled C/C++ media libraries (ADR-0003). OS and GPU-vendor
  APIs are fine.
- Shared versions live in `[workspace.dependencies]`. Crates use `dep.workspace = true`.
- Adding a dependency: say why in the PR, check maintenance and transitive weight, and prefer
  what the workspace already uses.
- Default features off where practical. No duplicate major versions without a `deny.toml` exception and a reason.

## 6. Testing

| Kind | Where | Required for |
|---|---|---|
| Unit | in-crate `#[cfg(test)]` | all logic |
| Protocol snapshot | `jstream-protocol` (`insta` over the JSON Schema) | any API change. The diff *is* the API review |
| Golden frame | `tests/golden/` | compositor, transforms, crop, effects. Rendered headless with a software adapter (WARP / lavapipe), compared with a tolerance |
| Audio offline | `jstream-audio` | mixer/filters: deterministic buffers in, expected buffers out |
| Mux conformance | `jstream-mux` | byte-exact fixtures, and CI validates outputs with `ffprobe` (test tool only, never linked) |
| Engine scenario | `tests/scenarios/` | end-to-end command scripts against a headless `Engine` with fake devices and a fake clock |
| Platform | `#[ignore]` + `--features hw-tests` | real devices/GPUs, run locally or on a self-hosted runner |

- Bug fixes come with a regression test.
- Golden images are only updated through `cargo xtask golden --update`, and the PR shows the before/after.
- Tests must not need a network, real devices or a GPU unless marked as platform tests.

## 7. Protocol and persistence compatibility

- `jstream-protocol` is semver'd separately. Adding fields or commands is minor. Removing or
  renaming anything is major and needs an ADR plus a deprecation period of one minor release.
- Command names are `noun.verb` in snake_case (`scene.create`, `item.set_crop`). IDs are opaque strings.
- Collection files carry `schema_version`. Every bump ships a migration and a fixture test
  that loads the previous version.

## 8. Security and privacy

- Stream keys and OAuth tokens live in the OS credential store only. They never appear in
  collection files, logs, events, snapshots or MCP responses (they're redacted as `"***"`).
- Network listeners (WebSocket API, MCP HTTP, web UI) bind to `127.0.0.1` by default.
  Binding elsewhere needs explicit config and an auth token.
- WASM plugins get no filesystem or network access unless the manifest declares it and the user grants it.

## 9. Git and pull requests

- `main` is protected. All changes land through PRs with green CI. **Squash merge.**
- Branches: `feat/…`, `fix/…`, `refactor/…`, `docs/…`, `chore/…` (agent-created branches may use their own prefix).
- Commit/PR titles: [Conventional Commits](https://www.conventionalcommits.org/) with the
  crate as scope: `feat(render): add perspective crop`, `fix(platform-windows): release WGC frame pool`.
- One logical change per PR. Mechanical refactors are separate from behavior changes.
- PR description: what, why, how it was tested, and the ADR link if one applies.
- Required checks: `fmt`, `clippy` (win/linux/mac), `test` (win/linux/mac), `deny`, `doc`, `lint-deps`.

## 10. Documentation

- `docs/ARCHITECTURE.md` is updated in the same PR as the change that makes it wrong.
- ADR format: `docs/adr/NNNN-kebab-title.md` with Status / Context / Decision / Consequences.
  Superseded ADRs stay and link to their replacement.
- Each crate has a short `README.md`: purpose, what it may depend on, and its key types.
