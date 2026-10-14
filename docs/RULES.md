<!-- @doc
id = "RULES"
title = "Engineering rules"
kind = "normative"
status = "active"
review_every = "90d"
-->

# Engineering rules

These rules apply to every change, whether a human or an agent wrote it. Each rule has
a permanent anchor ID (e.g. [R-RT-001](#R-RT-001)) from the first merge to `main`.
The format and the `enforced_by` vocabulary are defined in
[DOCS › Rules format](DOCS.md#rules). Changing a rule's `status`, `enforced_by` or
`applies_to`, or retiring a rule, is a governance change ([R-GOV-001](#R-GOV-001)).

<a id="architecture"></a>
## Architecture
<!-- @section
-->

<a id="R-ARCH-001"></a>
### R-ARCH-001: Protocol first
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/**"]
-->
Every capability is first a command, query, event or stream in `jstream-protocol`.
UI, MCP and CLI are built on the protocol, never beside it. There are no UI-only features.

<a id="R-ARCH-002"></a>
### R-ARCH-002: Crate layering
<!-- @rule
status = "active"
enforced_by = ["ci:lint-deps"]
applies_to = ["crates/*/Cargo.toml", "apps/*/Cargo.toml"]
-->
A crate depends only on crates in strictly lower layers, per
[ARCH › Crate map](ARCHITECTURE.md#crate-map).

<a id="R-ARCH-003"></a>
### R-ARCH-003: Clients never depend on the engine
<!-- @rule
status = "active"
enforced_by = ["ci:lint-deps"]
applies_to = ["crates/jstream-ui/**", "crates/jstream-mcp/**", "crates/jstream-server/**", "apps/jstream-cli/**"]
-->
`jstream-ui`, `jstream-mcp`, `jstream-server` and `jstream-cli` depend on `jstream-control`
and `jstream-protocol`, never on `jstream-core`. Only the host apps wire the engine
to the interfaces.

<a id="R-ARCH-004"></a>
### R-ARCH-004: Structural decisions need an ADR
<!-- @rule
status = "active"
enforced_by = ["review", "human"]
applies_to = ["Cargo.toml", "crates/**", "apps/**", "REVIEW.md", "docs/DOCS.md", "docs/RULES.md", "docs/adr/**"]
-->
Changes to structure, dependency boundaries, protocol compatibility, platform support,
codec policy, the review process or the docs system are recorded as an ADR (template:
`docs/adr/TEMPLATE.md`), with a decision tier per [R-GOV-005](#R-GOV-005).
Contradicting an accepted ADR requires a new ADR that supersedes it.

<a id="R-ARCH-005"></a>
### R-ARCH-005: New crates are declared and justified
<!-- @rule
status = "active"
enforced_by = ["ci:lint-deps", "review"]
applies_to = ["crates/*/Cargo.toml"]
-->
A new crate declares `[package.metadata.jstream]` (layer, summary, `unsafe_allowed`),
which feeds the generated crate map. The PR says why it isn't a module in an existing
crate.

<a id="R-ARCH-006"></a>
### R-ARCH-006: Edits and actions are distinct
<!-- @rule
status = "active"
enforced_by = ["test:jstream_protocol::every_command_is_classified", "review"]
applies_to = ["crates/jstream-protocol/**", "crates/jstream-core/**"]
-->
Every command is classified as a **state edit** (undoable, batchable, atomic) or an
**action** (side effects such as starting an output: not undoable, not batchable), per
[ADR-0013](adr/0013-command-model.md).

<a id="rust-code"></a>
## Rust code
<!-- @section
-->

<a id="R-RS-001"></a>
### R-RS-001: The pinned toolchain is the MSRV
<!-- @rule
status = "active"
enforced_by = ["ci:test"]
applies_to = ["rust-toolchain.toml"]
-->
The toolchain is pinned to 1.97.0<!--fact:rust-toolchain--> in `rust-toolchain.toml`.
JStream is an application, so there is no separate MSRV promise. Toolchain bumps go in
their own PR and are a governance change.

<a id="R-RS-002"></a>
### R-RS-002: Format and lint clean
<!-- @rule
status = "active"
enforced_by = ["ci:fmt", "ci:clippy"]
applies_to = ["**/*.rs"]
-->
`cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass on
every required platform ([R-GIT-003](#R-GIT-003)).

<a id="R-RS-003"></a>
### R-RS-003: Error types
<!-- @rule
status = "active"
enforced_by = ["deny:bans", "review"]
applies_to = ["crates/**"]
-->
Libraries use `thiserror` enums that carry context. `anyhow` is allowed only in `apps/*` and
`xtask`, enforced by the `wrappers` entry in `deny.toml`. Errors crossing crate boundaries
are never just strings.

<a id="R-RS-004"></a>
### R-RS-004: No unwrap outside tests
<!-- @rule
status = "active"
enforced_by = ["lint:clippy::unwrap_used", "review"]
applies_to = ["crates/**", "apps/**"]
-->
No `unwrap()` in non-test code. Use `expect()` only as `expect("invariant: …")`, with the
message stating the invariant.

<a id="R-RS-005"></a>
### R-RS-005: Logging through tracing, never on the audio thread
<!-- @rule
status = "active"
enforced_by = ["lint:clippy::print_stdout", "lint:clippy::print_stderr", "review"]
applies_to = ["crates/**"]
-->
Libraries never print. Use `tracing`, with spans around commands, outputs and sessions.
The audio thread never logs. It reports through lock-free counters and an event ring that
the control plane drains ([R-RT-001](#R-RT-001)).

<a id="R-RS-006"></a>
### R-RS-006: Public API is documented
<!-- @rule
status = "active"
enforced_by = ["lint:missing_docs"]
applies_to = ["crates/**"]
-->
Public items in library crates have doc comments.

<a id="R-RS-007"></a>
### R-RS-007: Traits only at real seams
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/**"]
-->
Prefer plain data and functions. Add a trait only at a real seam: platform backend,
encoder/decoder, source kind, output kind, effect, or control transport.

<a id="R-RS-008"></a>
### R-RS-008: No global mutable state
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/**"]
-->
State is passed explicitly or owned by `Engine`. No `static mut`, and no global
`OnceLock<Mutex<…>>` holding application state.

<a id="R-RS-009"></a>
### R-RS-009: Lint suppressions carry a reason
<!-- @rule
status = "active"
enforced_by = ["lint:clippy::allow_attributes_without_reason", "ci:governance"]
applies_to = ["**/*.rs"]
-->
Every `#[allow]` or `#[expect]` has `reason = "…"`. New suppressions are governance changes.

<a id="R-RS-010"></a>
### R-RS-010: Cargo features are additive
<!-- @rule
status = "active"
enforced_by = ["ci:clippy"]
applies_to = ["crates/*/Cargo.toml"]
-->
Enabling a feature never removes behavior. CI checks feature combinations with `cargo hack`.

<a id="unsafe"></a>
## Unsafe and FFI
<!-- @section
-->

<a id="R-UNSAFE-001"></a>
### R-UNSAFE-001: Unsafe only in declared crates
<!-- @rule
status = "active"
enforced_by = ["ci:lint-deps"]
applies_to = ["crates/**"]
-->
Only crates whose metadata declares `unsafe_allowed = true` may contain `unsafe`:
`jstream-platform-windows`, `jstream-platform-macos`, `jstream-platform-linux`,
`jstream-vcam-windows` and `*-sys`. All other crates have `#![forbid(unsafe_code)]`,
and lint-deps checks that it's present.

<a id="R-UNSAFE-002"></a>
### R-UNSAFE-002: Every unsafe block is justified
<!-- @rule
status = "active"
enforced_by = ["lint:clippy::undocumented_unsafe_blocks"]
applies_to = ["crates/jstream-platform-*/**", "crates/jstream-vcam-windows/**"]
-->
Every `unsafe` block has a `// SAFETY:` comment explaining why it is sound.

<a id="R-UNSAFE-003"></a>
### R-UNSAFE-003: Safe wrappers, RAII resources
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-platform-*/**", "crates/jstream-vcam-windows/**"]
-->
FFI wrappers expose safe APIs. Raw handles don't leak past a crate's public API.
COM / Objective-C / C resources are wrapped in RAII types.

<a id="R-UNSAFE-004"></a>
### R-UNSAFE-004: Official bindings first
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-platform-*/**", "crates/jstream-vcam-windows/**"]
-->
Prefer maintained bindings (`windows`, `objc2-*`, `pipewire`) over hand-written `extern` blocks.

<a id="R-UNSAFE-005"></a>
### R-UNSAFE-005: No panic crosses an FFI boundary
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-platform-*/**", "crates/jstream-vcam-windows/**"]
-->
Every function called from foreign code (COM methods, WGC/WASAPI/MF callbacks, Core Audio
procs) wraps its body in `catch_unwind`. A caught panic becomes an error the engine
reports; it never unwinds into foreign frames. The release profile keeps `panic = "unwind"`.

<a id="real-time"></a>
## Real-time code
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#threading"]
verified = { hash = "b3:743b474a5ba032ef", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read the section and R-RT-001..006 against the edited ARCH#threading paragraph: the rules still bind audio, render and encode threads; the pipeline now catches render and mixer panics as R-RT-006 asks and counts skipped render ticks in stats.tick per R-RT-004. No rule text changed." }
-->

<a id="R-RT-001"></a>
### R-RT-001: Audio thread does not allocate, lock, block or log
<!-- @rule
status = "active"
enforced_by = ["test:jstream_audio::rt::callback_does_not_allocate", "review"]
applies_to = ["crates/jstream-audio/src/rt/**", "crates/jstream-platform-*/src/audio/**"]
-->
In steady state, the audio callback and mixer do not allocate, take locks, block, make
syscalls other than the device API, or log. Debug builds and tests install
`assert_no_alloc` as the global allocator (in each binary and test harness) to enforce this.

<a id="R-RT-002"></a>
### R-RT-002: Render and encode threads do bounded work
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-render/**", "crates/jstream-encode/**"]
-->
Render and encode threads never wait on the control plane, do bounded work per tick, and
do no file or network I/O. GPU, driver and encoder API calls are allowed, because they
allocate and block internally. Avoid per-item allocation in per-frame loops.

<a id="R-RT-003"></a>
### R-RT-003: Lock-free hand-off, deferred reclamation
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-core/**", "crates/jstream-audio/**", "crates/jstream-render/**"]
-->
Real-time threads exchange data with the control plane only through lock-free queues,
per-subsystem snapshots, or parameter channels. Shared objects whose last reference
would be dropped on a real-time thread go to a collector thread to be freed.

<a id="R-RT-004"></a>
### R-RT-004: Overload drops, never stalls
<!-- @rule
status = "active"
enforced_by = ["test:scenarios::overload_drops_and_counts", "review"]
applies_to = ["crates/jstream-render/**", "crates/jstream-encode/**", "crates/jstream-outputs/**"]
-->
Under overload, frames and packets are dropped and counted, never stalling the clock.
Drop counts are reported in `stats.tick`.

<a id="R-RT-005"></a>
### R-RT-005: Performance claims state where they were measured
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-audio/**", "crates/jstream-render/**", "crates/jstream-encode/**"]
-->
A PR that touches a hot loop states its cost as "measured on `<hardware, OS>`: …" or
"not measured". Never estimate GPU or encoder costs without measuring.

<a id="R-RT-006"></a>
### R-RT-006: Real-time panics degrade, never vanish
<!-- @rule
status = "active"
enforced_by = ["test:scenarios::rt_panic_is_reported", "review"]
applies_to = ["crates/jstream-core/**", "crates/jstream-audio/**", "crates/jstream-render/**"]
-->
A panic on a real-time thread is caught at the thread boundary. Audio mutes the affected
bus, and render repeats the last frame. An error event is raised, and outputs keep
running unless the error is fatal and reported.

<a id="dependencies"></a>
## Dependencies
<!-- @section
depends_on = ["docs/adr/0031-source-types-and-decoding-scope.md"]
verified = { hash = "b3:58068a9c133cbde8", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "depends_on and R-DEP-002's link moved to ADR-0031; R-DEP-002 gains ADR-0031's sentence that pure-Rust image and font crates are not banned codec libraries; deny.toml bans and lint-deps allowlist enforcement unchanged" }
-->

<a id="R-DEP-001"></a>
### R-DEP-001: Permissive licenses (plus MPL-2.0)
<!-- @rule
status = "active"
enforced_by = ["deny:licenses"]
applies_to = ["Cargo.toml", "deny.toml"]
-->
Dependencies may only use the licenses on the `deny.toml` allow list: permissive
licenses plus MPL-2.0 (file-level copyleft). No GPL, LGPL or AGPL. The one exception is the
prebuilt CEF runtime for browser sources, which contains LGPL-2.1 FFmpeg and ships with
its notices and source pointer ([ADR-0021](adr/0021-browser-source.md)).

<a id="R-DEP-002"></a>
### R-DEP-002: No FFmpeg, GStreamer or bundled C codec libraries
<!-- @rule
status = "active"
enforced_by = ["deny:bans", "ci:lint-deps"]
applies_to = ["Cargo.toml", "deny.toml"]
-->
Per [ADR-0031](adr/0031-source-types-and-decoding-scope.md). Known binding crates are banned in
`deny.toml`. lint-deps also checks every crate with a `links =` key or a build script
that compiles C against an allowlist. Non-media C dependencies (e.g. `ring`) are
allowed only through that allowlist. The one media exception is the prebuilt CEF runtime, whose
internal media stack decodes in-page audio and video only; no JStream crate calls its
codecs ([ADR-0021](adr/0021-browser-source.md)). Pure-Rust image and font crates are not
banned codec libraries.

<a id="R-DEP-003"></a>
### R-DEP-003: Workspace-managed versions
<!-- @rule
status = "active"
enforced_by = ["ci:lint-deps"]
applies_to = ["crates/*/Cargo.toml", "apps/*/Cargo.toml"]
-->
Dependency versions are declared in `[workspace.dependencies]`, and member crates use
`dep.workspace = true`.

<a id="R-DEP-004"></a>
### R-DEP-004: New dependencies are justified
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["Cargo.toml"]
-->
The PR says why the dependency is needed, its maintenance status and its transitive weight.
Prefer crates the workspace already uses, and disable default features where practical.

<a id="R-DEP-005"></a>
### R-DEP-005: No duplicate major versions
<!-- @rule
status = "active"
enforced_by = ["deny:bans"]
applies_to = ["Cargo.lock"]
-->
Duplicates need a `deny.toml` skip entry with a reason. Known duplicates (several `windows`
crate versions via Tauri, wgpu and windows-rs) are expected. `multiple-versions` is `warn`
until M1 and switches to `deny` there ([ROADMAP](../ROADMAP.md#m1)).

<a id="R-DEP-006"></a>
### R-DEP-006: TLS through rustls with the ring provider
<!-- @rule
status = "active"
enforced_by = ["deny:licenses", "review"]
applies_to = ["Cargo.toml"]
-->
Use `rustls` with the `ring` crypto provider. `aws-lc-rs` is not used, because its license
terms are not on the allow list.

<a id="R-DEP-007"></a>
### R-DEP-007: GitHub Actions are pinned to commit SHAs
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = [".github/workflows/**"]
-->
Every third-party action is referenced as `owner/action@<full commit SHA> # vX.Y.Z`. Mutable
tags (`@v4`) and branches are not allowed. Dependabot keeps the pins current
([ADR-0018](adr/0018-ci-supply-chain-policy.md)).

<a id="testing"></a>
## Testing
<!-- @section
-->

| Kind | Where | Required for |
|---|---|---|
| Unit | in-crate `#[cfg(test)]` | all logic |
| Protocol snapshot | `jstream-protocol` (`insta` over the JSON Schema) | any API change |
| Golden frame | `tests/golden/`, rendered on **one reference adapter** (Windows WARP) | compositor, transforms, crop, effects |
| Render smoke | Linux lavapipe, no image compare | catches crashes and validation errors |
| Audio offline | `jstream-audio` | mixer, filters, resampling: deterministic buffers |
| Mux conformance | `jstream-mux` | byte-exact fixtures; also validated with `ffprobe` in CI (a test tool only, never linked) |
| Engine scenario | `tests/scenarios/` | end-to-end command scripts on a headless `Engine` with fake devices and fake clock |
| Platform | `#[ignore]` + `--features hw-tests` | real devices and GPUs, run locally or on a self-hosted runner |

<a id="R-TEST-001"></a>
### R-TEST-001: Bug fixes ship a regression test
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/**"]
-->
Every bug fix includes a test that fails without the fix.

<a id="R-TEST-002"></a>
### R-TEST-002: API changes ship a schema snapshot
<!-- @rule
status = "active"
enforced_by = ["ci:test"]
applies_to = ["crates/jstream-protocol/**"]
-->
Any protocol change updates the `insta` schema snapshot. The snapshot diff is the API review.

<a id="R-TEST-003"></a>
### R-TEST-003: Golden images change only through tooling
<!-- @rule
status = "active"
enforced_by = ["ci:test", "review"]
applies_to = ["tests/golden/**"]
-->
Golden images change only through `cargo xtask golden --update` on the reference adapter.
The PR shows before and after.

<a id="R-TEST-004"></a>
### R-TEST-004: Tests are hermetic
<!-- @rule
status = "active"
enforced_by = ["ci:test"]
applies_to = ["crates/**", "tests/**"]
-->
Tests need no network, real devices or physical GPU, unless they are platform tests
behind `hw-tests`.

<a id="R-TEST-005"></a>
### R-TEST-005: Say what wasn't run
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/jstream-platform-*/**", "crates/jstream-vcam-windows/**"]
-->
Platform code must compile for its target. The PR lists every platform test that was not
run on real hardware. Cross-checking Windows code from Linux works only for crates with
no C build scripts; otherwise say "not checked on Windows".

<a id="compatibility"></a>
## Protocol and persistence compatibility
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/ARCHITECTURE.md#persistence"]
verified = { hash = "b3:755a5ebd3c2cb244", by = "agent:grok@stats-tick", note = "Re-read R-API-001 through R-API-004 against the protocol section. stats.tick is an additive event, a minor bump to 0.6, named noun.verb. expected_rev is unchanged. No collection migration. No rule text changed." }
-->

<a id="R-API-001"></a>
### R-API-001: Protocol semver
<!-- @rule
status = "active"
enforced_by = ["ci:test", "review"]
applies_to = ["crates/jstream-protocol/**"]
-->
`jstream-protocol` follows semver, independently of the app. Additions are minor. Removing
or renaming anything is major: it needs an ADR and one minor release of deprecation first.

<a id="R-API-002"></a>
### R-API-002: Naming
<!-- @rule
status = "active"
enforced_by = ["test:jstream_protocol::names_are_valid", "test:jstream_mcp::tool_names_are_valid"]
applies_to = ["crates/jstream-protocol/**", "crates/jstream-mcp/**"]
-->
Protocol commands are named `noun.verb` in snake_case (`scene.create`). MCP tool names must
match `^[a-zA-Z0-9_-]{1,128}$` ([Anthropic tool use](https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/implement-tool-use)),
which rules out the protocol's `.`. So the tools are generated from the protocol and grouped
by noun ([ARCH › MCP](ARCHITECTURE.md#mcp)): each command or query `noun.verb` is an `op` of
the tool named after its noun (`scene`), and reaches exactly one tool. Tool names are
unique. IDs are opaque strings.

<a id="R-API-003"></a>
### R-API-003: Versioned collections with migrations
<!-- @rule
status = "active"
enforced_by = ["ci:test"]
applies_to = ["crates/jstream-graph/**"]
-->
Collection files carry `schema_version`. Every bump ships a migration, plus a fixture test
that loads the previous version.

<a id="R-API-004"></a>
### R-API-004: Concurrency-safe writes
<!-- @rule
status = "active"
enforced_by = ["test:jstream_core::stale_expected_rev_is_rejected", "review"]
applies_to = ["crates/jstream-protocol/**", "crates/jstream-core/**"]
-->
State edits accept an optional `expected_rev` (rejected if stale) and an optional
idempotency key. Undo is per client ([ADR-0013](adr/0013-command-model.md)).

<a id="security"></a>
## Security and privacy
<!-- @section
-->

<a id="R-SEC-001"></a>
### R-SEC-001: Secrets stay in the OS credential store
<!-- @rule
status = "active"
enforced_by = ["test:jstream_core::secrets_are_redacted", "review"]
applies_to = ["crates/**"]
-->
Stream keys and OAuth tokens live only in the OS credential store. They never appear in
collection files, logs, events, snapshots or MCP responses (they are redacted as `"***"`).

<a id="R-SEC-002"></a>
### R-SEC-002: Every listener authenticates and checks Origin
<!-- @rule
status = "active"
enforced_by = ["test:jstream_server::rejects_missing_token", "test:jstream_server::rejects_foreign_origin"]
applies_to = ["crates/jstream-server/**", "crates/jstream-mcp/**"]
-->
Every HTTP, WebSocket and MCP listener requires the control token (even on localhost) and
validates the `Host` header, and the `Origin` header when one is present. Listeners bind
to `127.0.0.1` by default
([ADR-0006](adr/0006-mcp-full-access.md)).

<a id="R-SEC-003"></a>
### R-SEC-003: WASM plugins are sandboxed by default
<!-- @rule
status = "active"
enforced_by = ["test:jstream_plugin_wasm::no_ambient_capabilities"]
applies_to = ["crates/jstream-plugin-wasm/**"]
-->
WASM plugins get no filesystem or network access unless the manifest declares it and the
user grants it. CLAP plugins are not sandboxed: they are native code the user chose to
install, run in their own process, which isolates crashes, not capabilities
([ADR-0035](adr/0035-clap-plugins-out-of-process.md)).

<a id="R-SEC-004"></a>
### R-SEC-004: No telemetry; crash dumps stay local
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["crates/**", "apps/**"]
-->
JStream sends no telemetry. Crash minidumps and logs are written locally. Nothing is
uploaded without an explicit per-report user action.

<a id="git"></a>
## Git, PRs and CI
<!-- @section
-->

<a id="R-GIT-001"></a>
### R-GIT-001: Protected main, squash merges
<!-- @rule
status = "active"
enforced_by = ["human"]
applies_to = []
-->
`main` is protected. Changes land only through PRs whose [required checks](#R-GIT-003)
pass, and are squash-merged.

<a id="R-GIT-002"></a>
### R-GIT-002: Conventional Commit titles
<!-- @rule
status = "active"
enforced_by = ["ci:pr-title"]
applies_to = []
-->
PR titles follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/),
with the crate name minus `jstream-` as the scope, e.g. `feat(render): add perspective crop`.

<a id="R-GIT-003"></a>
### R-GIT-003: The CI check list
<!-- @rule
status = "active"
enforced_by = ["ci:governance"]
applies_to = [".github/workflows/**"]
-->
This table is the **only** list of check names that `enforced_by` may cite as `ci:<name>`.
`enforced_by = ["review"]` means the rule is judged by the reviewer agent ([REVIEW.md](../REVIEW.md)).

| Check | Runs | Required | From |
|---|---|---|---|
| `fmt` | `cargo fmt --check` | yes | M0 |
| `clippy` | clippy `-D warnings` on Windows and Linux; `cargo hack` feature check | yes | M0 |
| `test` | unit, snapshot, golden (Windows WARP), scenario tests | yes | M0 |
| `doc` | `cargo doc` with `-D warnings` | yes | M0 |
| `deny` | `cargo deny check` | yes | M0 |
| `lint-deps` | `cargo xtask lint-deps` (layering, unsafe, `links=` allowlist, workspace deps) | yes | M0 |
| `docs` | `cargo xtask docs check` | yes | M0 |
| `governance` | `cargo xtask governance` (label plus justification for governance changes) | yes | M0 |
| `pr-title` | Conventional Commit title | yes | M0 |
| `gate-test` | Node test of `review-gate`'s decision function, run against the workflow file | yes | now |
| `review-gate` | Commit status set from the reviewer agent's verdict for the head SHA ([R-GOV-003](#R-GOV-003)) | yes | now |
| `macos` | build and unit tests on macOS | no until M3 | M0 |
| `sweep` | weekly docs/external/facts sweep plus override audit list (scheduled) | n/a | M1 |

<a id="R-GIT-004"></a>
### R-GIT-004: One logical change per PR
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = []
-->
Keep mechanical refactors separate from behavior changes.

<a id="R-GIT-005"></a>
### R-GIT-005: PR description answers the template
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = []
-->
The description covers what changed, why, what was not run, and any new crate, new
dependency or performance claim (`.github/pull_request_template.md`).

<a id="documentation"></a>
## Documentation
<!-- @section
depends_on = ["docs/DOCS.md#gate"]
verified = { hash = "b3:fdb0c87dff90c573", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read R-DOC-001..012 against DOCS#gate after the rebase: R-DOC-009 names the adr-status exception citing ADR-0038 (renumbered from 0036); R-DOC-006 already covers generated blocks; DOCS#gate's ADR-0039 row-key wording leaves R-DOC-012 as is" }
-->

<a id="R-DOC-001"></a>
### R-DOC-001: Docs and sections are identified
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md"]
-->
Every tracked doc starts with a `@doc` metadata block, and every `##` section has an anchor and an
`@section` block ([DOCS › Sections](DOCS.md#sections)).

<a id="R-DOC-002"></a>
### R-DOC-002: References resolve
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md", "**/*.rs"]
-->
Links to files, anchors and rules must resolve. External URLs must be registered in
`docs/external.toml` ([DOCS › References](DOCS.md#references)).

<a id="R-DOC-003"></a>
### R-DOC-003: No stale verification
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**"]
-->
A section's stored `verified.hash` must match the computed hash
([DOCS › Hash-bound verification](DOCS.md#verification)).

<a id="R-DOC-004"></a>
### R-DOC-004: Fix docs in the same PR; verify only what you re-read
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["**"]
-->
A PR that makes any doc wrong fixes it in the same PR, whether or not a hash flagged it.
A section is verified only after it has been re-read against the change, and the note
says what was checked. A wrong section is edited, never verified.

<a id="R-DOC-005"></a>
### R-DOC-005: No history in files
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md"]
-->
Docs store no revision numbers, dates or history tables. These come from git
([DOCS › History](DOCS.md#history)).

<a id="R-DOC-006"></a>
### R-DOC-006: Generate what can be derived
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md"]
-->
Derivable content is generated, and generated blocks are never edited by hand.

<a id="R-DOC-007"></a>
### R-DOC-007: One source per fact
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md"]
-->
Repeated or external values are registered in `docs/facts.toml` and written with a fact
marker. ADRs are exempt.

<a id="R-DOC-008"></a>
### R-DOC-008: Code cites its governing sections
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.rs"]
-->
Crate roots carry a `Docs:` line that matches the sections covering them, in both directions.
Module-level `Docs:` lines are optional, but every reference on them must resolve.

<a id="R-DOC-009"></a>
### R-DOC-009: Accepted ADRs are immutable
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["docs/adr/**"]
-->
After acceptance, only `status` and `superseded_by` may change. A changed decision is
recorded in a new ADR. **Two exceptions:**
- The delimiter syntax of the `@doc` metadata block is a container, not content. It may
  change in every doc, accepted ADRs included, but only through a repo-wide format ADR, as
  a declared governance change, with each ADR's Context, Decision and Consequences left
  byte-identical ([ADR-0016](adr/0016-docs-as-checked-graph-v2.md)).
- The generated `adr-status` banner under the title is not content either. It may change
  in an accepted ADR, but only through `cargo xtask docs generate`, with everything else
  in the ADR left byte-identical
  ([ADR-0038](adr/0038-adr-status-banner-and-index.md),
  [DOCS › Generated content](DOCS.md#generated)).

<a id="R-DOC-010"></a>
### R-DOC-010: IDs are permanent
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md"]
-->
From the first merge to `main`, anchors and rule IDs are never deleted or reused.
Retired sections leave tombstones, and rules are retired, not deleted
([DOCS › Lifecycle](DOCS.md#lifecycle)).

<a id="R-DOC-011"></a>
### R-DOC-011: Time-based review never blocks
<!-- @rule
status = "active"
enforced_by = ["ci:sweep"]
applies_to = ["**/*.md", "docs/external.toml", "docs/facts.toml"]
-->
Docs and externals that are due for review are reported in the pinned issue. They never
fail a PR ([DOCS › Time-based review](DOCS.md#ttl)).

<a id="R-DOC-012"></a>
### R-DOC-012: Globs must match
<!-- @rule
status = "active"
enforced_by = ["ci:docs"]
applies_to = ["**/*.md"]
-->
A section's `covers` glob that matches no tracked file is an error in active docs (a
warning in drafts; retired docs are exempt): a section must not describe code that
doesn't exist. A rule's `applies_to` glob must be valid but may match nothing, so a rule
can govern a crate before it exists. Naming a crate that neither exists nor appears in the
crate map is reported as a likely typo. Empty `applies_to = []` is allowed for process
rules.

<a id="governance"></a>
## Governance
<!-- @section
depends_on = ["docs/adr/0019-review-gate-v2.md"]
verified = { hash = "b3:6a3bee027e59ef38", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read R-GOV-001..005 against ADR-0019: owner-only verdicts live in REVIEW/gate, labels+justification, tiers and proxy decisions unchanged" }
-->

<a id="R-GOV-001"></a>
### R-GOV-001: Governance changes are labeled and justified
<!-- @rule
status = "active"
enforced_by = ["ci:governance"]
applies_to = ["**"]
-->
Changes listed in [DOCS › Governance](DOCS.md#governance) fail the `governance` check
unless the PR carries the `governance-approved` label and has a non-empty "Governance
change" section in its description. Agents may apply the label.

<a id="R-GOV-002"></a>
### R-GOV-002: Overrides are declared, justified and audited
<!-- @rule
status = "active"
enforced_by = ["ci:governance", "ci:review-gate", "ci:sweep"]
applies_to = []
-->
Anyone, agents included, may apply `governance-approved` or `review-override`, but only
with the matching justification section in the PR description ("Governance change",
"Review override"). Never apply them just to get past a check you haven't examined.
Every PR merged with either label is listed in the weekly sweep issue for the maintainer
to audit. Agents never change branch protection or repository settings.

<a id="R-GOV-003"></a>
### R-GOV-003: An independent reviewer agent gates every merge
<!-- @rule
status = "active"
enforced_by = ["ci:review-gate"]
applies_to = []
-->
Every PR is reviewed by a reviewer agent following [REVIEW.md](../REVIEW.md), in a session
that authored none of the PR's commits. Its verdict comment, tied to the head SHA,
drives the `review-gate` status. PRs merge only through GitHub auto-merge once every
required check, `review-gate` included, is green. Builder agents never post verdict
markers and never merge by hand.

<a id="R-GOV-004"></a>
### R-GOV-004: Builders answer every blocking finding
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = []
-->
For each blocking finding (`B<n>`), the builder either pushes a fix and replies naming the
commit, or replies with a concrete rebuttal (a code path, test or doc that shows the
finding is wrong). Disputes that last three rounds go to the maintainer through
`needs-human` ([REVIEW › Re-review rounds](../REVIEW.md#rounds)).

<a id="R-GOV-005"></a>
### R-GOV-005: Tier-1 decisions need the maintainer
<!-- @rule
status = "active"
enforced_by = ["review"]
applies_to = ["docs/adr/**"]
-->
Every new ADR declares `tier = "maintainer"` or `tier = "agent"`, following
[ADR-0017](adr/0017-adr-decision-tiers.md). A Tier-1 ADR (product and platforms, process
and governance, or superseding any ADR) lands as `proposed` with `needs-human`. It becomes
`accepted` or `rejected` only on the maintainer's `/accept` or `/reject` decision on the
PR, either directly or relayed by an agent with a quoted instruction from the maintainer that
names the ADR ([ADR-0017](adr/0017-adr-decision-tiers.md)). Agents never post a decision the
maintainer did not give.
