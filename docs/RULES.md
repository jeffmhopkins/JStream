---
id: RULES
title: Engineering rules
kind: normative
status: active
rev: 2
review_every: 90d
depends_on: [ARCH, DOCS]
verified: {date: 2026-10-13, by: "agent:claude", rev: 2, note: "Rules converted to permanent IDs with lifecycle blocks; added R-DOC-*"}
---

# Engineering rules

These rules apply to every change, whether a human or an agent wrote it. Each rule
has a permanent ID. Cite rules as `[[RULES#R-<AREA>-NNN]]`. The rule format and lifecycle
are described in [[DOCS#rules-format]].

To change a rule, edit it in a PR (its rev is bumped by tooling) and explain why. To
remove one, retire it; never delete it or reuse its ID.

Until `xtask docs` exists ([[DOCS#bootstrap]]), `ci:xtask-*` enforcement is
done by review.

## Architecture
<!-- @section
id: architecture
rev: 1
-->

### R-ARCH-001: Protocol first
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/**]
-->
Every new capability is added first as a command, query or event in `jstream-protocol`.
UI, MCP and CLI features are built on the protocol, never beside it. There are no
UI-only features. See [[ARCH#principles]].

### R-ARCH-002: Crate layering
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-lint-deps]
applies_to: [crates/*/Cargo.toml, apps/*/Cargo.toml]
-->
A crate depends only on crates in strictly lower layers ([[ARCH#crate-map]]).

### R-ARCH-003: UI depends only on the protocol
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-lint-deps]
applies_to: [crates/jstream-ui/**]
-->
`jstream-ui` depends only on `jstream-protocol` and web crates, never on `jstream-core`.

### R-ARCH-004: Structural decisions need an ADR
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [Cargo.toml, crates/**, apps/**]
-->
Changes to structure, dependency boundaries, protocol compatibility or platform
minimums are recorded as an ADR (format: [[path:docs/adr/0000-template.md]]). Contradicting an accepted
ADR requires a new ADR that supersedes it.

### R-ARCH-005: New crates are justified
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-lint-deps, review]
applies_to: [crates/*/Cargo.toml]
-->
A new crate declares `[package.metadata.jstream]` (layer, summary), which feeds the
generated crate map, and the PR gives the reason it isn't a module in an existing crate.

## Rust code
<!-- @section
id: rust-code
rev: 1
-->

### R-RS-001: Pinned toolchain
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:toolchain-file, review]
applies_to: [rust-toolchain.toml]
-->
The toolchain is pinned in `rust-toolchain.toml` (Rust 1.97.0 [[fact:rust-toolchain]],
edition 2024 [[fact:rust-edition]]). Toolchain bumps go in their own PR.

### R-RS-002: Format and lint clean on all platforms
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:fmt, ci:clippy]
applies_to: ["**/*.rs"]
-->
`cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass on
Windows, Linux and macOS.

### R-RS-003: Error types
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-lint-deps, review]
applies_to: [crates/**]
-->
Libraries use `thiserror` enums with context. `anyhow` is allowed only in `apps/*` and
`xtask` (lint-deps rejects it elsewhere). No stringly-typed errors across crate boundaries.

### R-RS-004: No unwrap outside tests
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [lint:clippy::unwrap_used, review]
applies_to: [crates/**, apps/**]
-->
No `unwrap()` in non-test code. `expect()` only as `expect("invariant: …")` where the
message states the invariant.

### R-RS-005: Logging through tracing
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [lint:clippy::print_stdout, lint:clippy::print_stderr]
applies_to: [crates/**]
-->
Libraries never print. Use `tracing`, with spans around commands, outputs and sessions.

### R-RS-006: Public API is documented
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [lint:missing_docs]
applies_to: [crates/**]
-->
Public items in library crates have doc comments. The lint is `warn` in layers 0–2,
and `-D warnings` makes that blocking.

### R-RS-007: Traits only at real seams
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/**]
-->
Prefer plain data plus functions. Introduce traits only at real seams: platform,
encoder, source kind, effect, control transport.

### R-RS-008: No global mutable state
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/**]
-->
State is passed explicitly or owned by `Engine`. No `static mut`, and no global
`OnceLock<Mutex<…>>` for application state.

## Unsafe and FFI
<!-- @section
id: unsafe
rev: 1
-->

### R-UNSAFE-001: Unsafe only in platform crates
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-lint-deps]
applies_to: [crates/**]
-->
`unsafe` is allowed only in `jstream-platform-windows`, `jstream-platform-macos`,
`jstream-platform-linux` and `*-sys` crates. Every other crate has
`#![forbid(unsafe_code)]`, and lint-deps checks that the attribute is present.

### R-UNSAFE-002: Every unsafe block is justified
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [lint:clippy::undocumented_unsafe_blocks]
applies_to: [crates/jstream-platform-*/**]
-->
Every `unsafe` block has a `// SAFETY:` comment explaining why it is sound.

### R-UNSAFE-003: Safe wrappers, RAII resources
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/jstream-platform-*/**]
-->
FFI wrappers expose safe APIs, and raw handles don't leak past a platform crate's public
API. COM / Objective-C / C resources are wrapped in RAII types.

### R-UNSAFE-004: Official bindings first
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/jstream-platform-*/**]
-->
Prefer maintained bindings (`windows`, `objc2-*`, `pipewire`) over hand-written `extern` blocks.

## Real-time code
<!-- @section
id: real-time
rev: 1
depends_on: [ARCH#threading]
-->

### R-RT-001: No blocking work on real-time threads
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [test:assert_no_alloc, review]
applies_to: [crates/jstream-audio/**, crates/jstream-render/**, crates/jstream-encode/**]
-->
Audio, render and encode threads do not allocate, take contended locks, block on I/O,
or `.await` in steady state. Debug builds wrap the audio callback in `assert_no_alloc`.

### R-RT-002: Lock-free hand-off to the control plane
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/jstream-core/**, crates/jstream-audio/**, crates/jstream-render/**]
-->
Real-time threads communicate with the control plane through lock-free queues,
`arc-swap` snapshots or triple buffers. No `Mutex` shared with tokio tasks.

### R-RT-003: Overload drops, never stalls
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [test:scenario-overload, review]
applies_to: [crates/jstream-render/**, crates/jstream-encode/**, crates/jstream-outputs/**]
-->
Under overload, frames and packets are dropped and counted, never stall the clock.
Drops are reported in `stats.tick`.

### R-RT-004: Hot-loop changes state their cost
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/jstream-audio/**, crates/jstream-render/**]
-->
A PR that touches a real-time hot loop states its per-frame cost, and includes a
benchmark when it changes the loop's work.

## Dependencies
<!-- @section
id: dependencies
rev: 1
-->

### R-DEP-001: Permissive licenses only
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:cargo-deny]
applies_to: [Cargo.toml, deny.toml]
-->
Dependencies use only the licenses allowed in `deny.toml` ([[ext:cargo-deny]]).
No GPL, LGPL or AGPL.

### R-DEP-002: No FFmpeg or bundled C media libraries
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:cargo-deny]
applies_to: [Cargo.toml, deny.toml]
-->
Per [[ADR-0003]]. Known FFmpeg/GStreamer binding crates are banned in `deny.toml`.
OS and GPU-vendor APIs are fine.

### R-DEP-003: Workspace-managed versions
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-lint-deps]
applies_to: [crates/*/Cargo.toml, apps/*/Cargo.toml]
-->
Dependency versions live in `[workspace.dependencies]`. Member crates use `dep.workspace = true`.

### R-DEP-004: New dependencies are justified
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [Cargo.toml]
-->
A PR that adds a dependency says why, notes its maintenance and transitive weight, and
prefers what the workspace already uses. Default features are off where practical.

### R-DEP-005: No duplicate major versions
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:cargo-deny, review]
applies_to: [Cargo.lock]
-->
Duplicate major versions need a `deny.toml` skip entry with a reason. (`multiple-versions`
is `warn` until M1; it becomes `deny` once the dependency tree settles.)

## Testing
<!-- @section
id: testing
rev: 1
-->

Test kinds and where they live:

| Kind | Where | Required for |
|---|---|---|
| Unit | in-crate `#[cfg(test)]` | all logic |
| Protocol snapshot | `jstream-protocol` (`insta` over the JSON Schema) | any API change |
| Golden frame | `tests/golden/` | compositor, transforms, crop, effects (software adapter, tolerance compare) |
| Audio offline | `jstream-audio` | mixer and filters, deterministic buffers |
| Mux conformance | `jstream-mux` | byte-exact fixtures; CI also validates with `ffprobe` (test tool only, never linked) |
| Engine scenario | `tests/scenarios/` | end-to-end command scripts on a headless `Engine` with fake devices and clock |
| Platform | `#[ignore]` + `--features hw-tests` | real devices/GPUs, run locally or on a self-hosted runner |

### R-TEST-001: Bug fixes ship a regression test
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: [crates/**]
-->
A bug fix includes a test that fails without the fix.

### R-TEST-002: API changes ship a schema snapshot
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:test]
applies_to: [crates/jstream-protocol/**]
-->
Any protocol change updates the `insta` schema snapshot. The snapshot diff is the API review.

### R-TEST-003: Golden images change only through tooling
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-golden, review]
applies_to: [tests/golden/**]
-->
Golden images are updated only by `cargo xtask golden --update`, and the PR shows before/after.

### R-TEST-004: Tests are hermetic
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:test, review]
applies_to: [crates/**, tests/**]
-->
Tests need no network, real devices or physical GPU, unless they are marked as
platform tests behind `hw-tests`.

## Protocol and persistence compatibility
<!-- @section
id: compatibility
rev: 1
depends_on: [ARCH#protocol, ARCH#persistence]
-->

### R-API-001: Protocol semver
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:test, review]
applies_to: [crates/jstream-protocol/**]
-->
`jstream-protocol` has its own semver. Adding something is a minor change. Removing or
renaming anything is a major change and needs an ADR plus one minor release of deprecation.

### R-API-002: Command naming
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-gen-schema]
applies_to: [crates/jstream-protocol/**]
-->
Commands are `noun.verb` in snake_case (`scene.create`, `item.set_crop`). IDs are opaque strings.

### R-API-003: Versioned collections with migrations
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:test]
applies_to: [crates/jstream-graph/**]
-->
Collection files carry `schema_version`. Every bump ships a migration and a fixture
test that loads the previous version.

## Security and privacy
<!-- @section
id: security
rev: 1
-->

### R-SEC-001: Secrets stay in the OS credential store
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [test:redaction, review]
applies_to: [crates/**]
-->
Stream keys and OAuth tokens live only in the OS credential store. They never appear in
collection files, logs, events, snapshots or MCP responses (they're redacted as `"***"`).

### R-SEC-002: Localhost by default
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [test:bind-defaults]
applies_to: [crates/jstream-server/**, crates/jstream-mcp/**]
-->
Network listeners bind to `127.0.0.1` by default. Binding elsewhere requires explicit
config and an auth token.

### R-SEC-003: Plugins are sandboxed by default
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [test:plugin-capabilities]
applies_to: [crates/jstream-plugin-wasm/**]
-->
WASM plugins get no filesystem or network access unless the manifest declares it and the
user grants it.

## Git and pull requests
<!-- @section
id: git
rev: 1
-->

### R-GIT-001: Protected main, squash merges
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:branch-protection]
applies_to: []
-->
`main` is protected. All changes land through PRs with green CI, squash-merged.

### R-GIT-002: Branch naming
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: []
-->
`feat/…`, `fix/…`, `refactor/…`, `docs/…`, `chore/…`. Agent-created branches may use
their own prefix.

### R-GIT-003: Conventional Commit titles
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:pr-title]
applies_to: []
-->
PR and commit titles follow [[ext:conventional-commits]], with the crate (minus
`jstream-`) as scope: `feat(render): add perspective crop`.

### R-GIT-004: One logical change per PR
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: []
-->
Mechanical refactors are kept separate from behavior changes.

### R-GIT-005: PR description
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [review]
applies_to: []
-->
What, why, how it was tested, what was **not** run, and ADR/doc references
(per `.github/pull_request_template.md`).

### R-GIT-006: Required checks
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:branch-protection]
applies_to: [.github/workflows/**]
-->
`fmt`, `clippy` (win/linux/mac), `test` (win/linux/mac), `deny`, `doc`, `lint-deps`, `docs`.

## Documentation
<!-- @section
id: documentation
rev: 1
depends_on: [DOCS]
-->

### R-DOC-001: Every doc and section is identified
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**/*.md"]
-->
Every tracked doc has valid front matter with a permanent ID, and every tracked
section has a metadata block ([[DOCS#front-matter]], [[DOCS#sections]]).

### R-DOC-002: Checked references only
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**/*.md", "**/*.rs"]
-->
Cross-references use `[[…]]` and must resolve. External URLs live only in
`docs/external.toml` ([[DOCS#references]]).

### R-DOC-003: Coverage gate
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**"]
-->
A PR that changes files covered by a section also edits or re-verifies that section
([[DOCS#coverage-gate]]).

### R-DOC-004: One-hop propagation
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**/*.md"]
-->
When a section's content changes, every section that depends on it is edited or
re-verified in the same PR. Verification alone does not propagate.

### R-DOC-005: Tool-written metadata
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**/*.md"]
-->
Revs, dates, `verified` and revision histories are written by `xtask docs`, never by hand
([[DOCS#revisions]]). Bootstrap exception: [[DOCS#bootstrap]].

### R-DOC-006: Verification needs evidence
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs, review]
applies_to: ["**/*.md"]
-->
Every verification carries a note saying what was checked. Stock or empty notes are
rejected ([[DOCS#verification-notes]]). If a section is wrong, edit it, don't verify it.

### R-DOC-007: Generate what can be derived
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**/*.md"]
-->
Derivable content is generated, and generated blocks are never edited by hand
([[DOCS#generated]]).

### R-DOC-008: One source per fact
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs, review]
applies_to: ["**/*.md"]
-->
A fact that appears in more than one place is registered in `docs/facts.toml` and
cited as `[[fact:…]]`. It is restated only on the same line as that citation.

### R-DOC-009: Code cites its governing docs
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["crates/*/src/lib.rs", "crates/*/src/main.rs", "apps/*/src/main.rs"]
-->
Crate roots carry a `Docs:` line citing every section that covers them
([[DOCS#back-references]]). Citations of superseded ADRs or retired rules fail.

### R-DOC-010: Time-based review
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs, ci:docs-sweep]
applies_to: ["**/*.md", docs/external.toml, docs/facts.toml]
-->
Docs and externals are re-verified within `review_every`. CI warns past 1× and fails past 2×
([[DOCS#ttl]]).

### R-DOC-011: ADRs are immutable once accepted
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: [docs/adr/**]
-->
Only `status` and `superseded_by` change after acceptance. A changed decision is recorded
as a new ADR.

### R-DOC-012: Rule IDs are permanent
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: [docs/RULES.md]
-->
Rule IDs are never deleted, renumbered or reused. Rules are retired instead
([[DOCS#rules-format]]).

### R-DOC-013: Architecture docs change with the code
<!-- @rule
status: active
rev: 1
since: 2026-10-13
enforced_by: [ci:xtask-docs]
applies_to: ["**"]
-->
A PR that makes a doc wrong fixes that doc in the same PR. "Docs follow-up" PRs are not
an acceptable substitute.

## Revision history

<!-- @generated begin history by="xtask docs sync" -->
| rev | date | by | sections | note |
|---|---|---|---|---|
| 1 | 2026-10-13 | agent:claude | all | Initial rules (unnumbered) |
| 2 | 2026-10-13 | agent:claude | all | Permanent rule IDs and lifecycle blocks; added Documentation rules R-DOC-001..013 |
<!-- @generated end history -->
