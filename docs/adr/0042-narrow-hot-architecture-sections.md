<!-- @doc
id = "ADR-0042"
title = "Narrow the hot architecture sections: manifest views, app entry points, threading model"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0042: Narrow the hot architecture sections: manifest views, app entry points, threading model

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Refines [ADR-0016](0016-docs-as-checked-graph-v2.md)'s point 2 (what a section's hash is
over) a second time, after [ADR-0039](0039-hash-ignores-metadata-and-row-dependencies.md).
It supersedes neither: every other point of both stands.

<a id="context"></a>
## Context
ADR-0039 stopped re-verification from cascading, but three sections of
[ARCHITECTURE.md](../ARCHITECTURE.md) still go stale on most PRs, because each `covers`
far more than its text describes:

- [ARCH › Shape](../ARCHITECTURE.md#shape) covers `apps/**`. Its text is a system diagram
  and a table saying what each host is. Every edit to an app's argument parser, logging,
  tests or README stales it.
- [ARCH › Crate map](../ARCHITECTURE.md#crate-map) covers every `crates/*/Cargo.toml` and
  `apps/*/Cargo.toml` whole. Its text is the layer, responsibility and `unsafe` of each
  crate, and the edges between `jstream-*` crates. Adding a third-party dependency, a
  feature or a comment to any manifest stales it.
- [ARCH › Threads, clock and sync](../ARCHITECTURE.md#threading) covers
  `jstream-core/src/runtime/**`, `jstream-audio/src/rt/**` and a render file that doesn't
  exist yet. It mixes the engine-wide model (the clock, the thread roster and its rules,
  how state reaches real-time threads, the live-value handover) with build detail of
  one subsystem (the output pipeline paragraph), which the crate READMEs also describe.

Over the last 40 commits on `main`, 55 `verified` restamps were in ARCHITECTURE.md, nearly
all in these three sections. Two PRs that each restamp one of them conflict on the
`verified` line, and [DOCS › Hash-bound verification](../DOCS.md#verification) is right
that this is a real conflict: the hash includes the covered files, so after both merge the
section really is stale. GitHub ignores custom merge drivers when it computes
mergeability, so the conflicts can't be scripted away on the server. They stall merges
and, as ADR-0039 already noted, train builders to restamp sections they had no reason to
re-read ([R-DOC-004](../RULES.md#R-DOC-004)). [DOCS › Verification notes](../DOCS.md#notes)
already says the remedy: a section re-verified far more often than it is edited has
`covers` that are too broad, so narrow them.

Measured on those 40 commits (by replaying the rules below on each commit's diff), the
commits that would have staled each section through its covered files:

| Section | Today | Under this ADR |
|---|---|---|
| Shape | 13 | 2 |
| Crate map | 9 | 5 |
| Threads, clock and sync | 7 | 0 (only edits to its own text or its ADRs) |

<a id="decision"></a>
## Decision
1. **Manifest views in `covers`.** A `covers` entry may end in `#jstream-meta`, as in
   `crates/*/Cargo.toml#jstream-meta`. The part before `#` is an ordinary glob and decides
   which files the section covers (for `docs for`, back-references and the "crates no
   section covers" report alike). Each matched file contributes its path plus a hash of
   its **jstream view** instead of a hash of its whole content:
   - the view is built from the parsed TOML: `package.name`; the whole
     `[package.metadata.jstream]` table; and, from `[dependencies]`,
     `[dev-dependencies]`, `[build-dependencies]` and each of their
     `[target.<cfg>.…]` variants, every entry whose key, or whose `package =` value,
     starts with `jstream-`, with its whole value (`workspace`, `path`, `features`,
     `optional`, `default-features`, …);
   - it is serialized canonically (tables and keys in sorted order, values as TOML), so
     comments, ordering and formatting don't count, and neither do third-party
     dependencies, features tables or lint tables;
   - a matched file that is not named `Cargo.toml`, or that does not parse as TOML, is
     hashed whole, so a view never hashes *less* than it can read;
   - `#` is reserved in `covers` globs, and any view name other than `jstream-meta` is a
     `docs check` error. Further views need their own ADR.

   ARCH › Crate map becomes `covers = ["crates/*/Cargo.toml#jstream-meta",
   "apps/*/Cargo.toml#jstream-meta"]`. That view holds exactly what the map states from
   the manifests (each crate's name, layer, summary and `unsafe_allowed`, and its
   `jstream-*` edges, dev-dependencies included, as the `apps/*` row mentions them).
   - Some rows also name a third-party dependency (`wgpu` types, `axum`, no `tokio` in
     WASM builds, pure-Rust image and font crates). The view no longer checks those
     against the manifest. For each crate that exists, the implementing PR makes sure the
     claim is stated in a section of that crate's README that covers its whole
     `Cargo.toml` (`jstream-control`, `-core`, `-mcp`, `-server` and both apps already
     have one; it adds the manifest to `covers` of `jstream-platform`, `-render` and `-mux`
     as needed, which tightens). A crate created later gets that section in the PR that
     creates it, under [R-ARCH-005](../RULES.md#R-ARCH-005).
   - *Why a view and not the generated crate map:* the map will be generated from
     `[package.metadata.jstream]` eventually ([DOCS › Generated content](../DOCS.md#generated)),
     but the `jstream-*` edges and the prose around the table still need a hash, and a
     view keeps working unchanged once the table is generated.
2. **ARCH › Shape covers each app's manifest view and entry point:** `covers =
   ["apps/*/Cargo.toml#jstream-meta", "apps/*/src/main.rs"]`. The host table then states
   only what an app's manifest and its `main.rs` crate documentation state (what the host
   is, what it links, which interfaces it serves, its top-level modes such as
   `jstream-cli mcp`). Flags, usage and design detail live in the app's README, whose
   `usage` and `design` sections already cover the rest of the app. Today's table already
   fits that, apart from naming `--collection`, which the daemon's `main.rs` crate doc
   names as well (as not yet built); the implementing PR re-reads it on that basis.
3. **ARCH › Threads, clock and sync keeps the engine-wide model and covers no code.**
   - It keeps: the media clock, the thread roster and each thread's rules, audio
     timestamps and latency compensation, how state reaches real-time threads, the
     live-value handover, the privacy flag, the planned CLAP plugin hosts and the S3
     hardware result. Its `depends_on` lists the ADRs those rest on (0014, 0027, 0028,
     0032 and 0035).
   - Each subsystem's runtime has its own small section in its crate README, which
     covers that code, carries the build detail and `depends_on` ARCH › Threads, clock and
     sync: `crates/jstream-audio/README.md#realtime` (exists, covers `src/rt/**`), a new
     `crates/jstream-core/README.md#runtime` covering `src/runtime/**` (the output
     pipeline paragraph moves there, and ARCH keeps one sentence linking it), and, when
     the render frame loop is written, a section in `jstream-render`'s README covering it
     (today's `frame_loop.rs` glob matches nothing).
   - A subsystem section must not contradict the model. A change that would makes the
     model edit in the same PR ([R-DOC-004](../RULES.md#R-DOC-004)), and editing the model
     stales every subsystem section for re-reading.
4. **One PR implements it**, after acceptance: the `#jstream-meta` view in
   `xtask/src/docs/` with fixture tests (formatting, comments and third-party edits leave
   the hash alone; layer, summary, `unsafe_allowed`, `jstream-*` edges, renames via
   `package =`, target-specific tables and an unparseable manifest change it; unknown view
   names fail `docs check`), the metadata edits above, the moved paragraph, and the edits
   to [DOCS › Sections](../DOCS.md#sections) and
   [DOCS › Hash-bound verification](../DOCS.md#verification). The three sections, and
   every section whose covers or text the PR changes, are re-read and verified with notes
   that say so.

<a id="consequences"></a>
## Consequences
- The three sections stop going stale on PRs that don't change what they say, so their
  `verified` lines conflict far less often, and a restamp of them again means a re-read.
- **What is no longer checked by these sections:** formatting, comments, third-party
  dependencies and features in manifests (crate map); app files other than the manifest
  and `main.rs` (shape); runtime code (threads). Each is still covered by a narrower
  section that describes it: the crate and app READMEs' sections that cover the whole
  manifest or the source, and the subsystem runtime sections. The cost is that a code
  change contradicting the threading model is caught by the subsystem section's re-read
  and by review, not by the model's own hash. The model states decisions (backed by its
  ADR dependencies), not build detail, to keep that gap small.
- Core's README gains a section that goes stale on `runtime/**` edits, which its `design`
  section already does. The total restamps per runtime PR still fall by one.
- `docs check` and `docs for` learn one `covers` suffix. Nothing else in the docs format
  changes, and existing `covers` entries keep their meaning.
- **Governance change:** the implementing PR narrows `covers` and edits
  `xtask/src/docs/**` and `docs/DOCS.md`, all listed in
  [DOCS › Governance](../DOCS.md#governance). It needs `governance-approved` and a
  "Governance change" section that lists, claim by claim, where each claim of the three
  sections is checked after the change.
