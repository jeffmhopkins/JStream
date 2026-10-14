<!-- @doc
id = "ADR-0039"
title = "Doc hashes ignore metadata blocks; row-level crate-map dependencies"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0039: Doc hashes ignore metadata blocks; row-level crate-map dependencies

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Refines [ADR-0016](0016-docs-as-checked-graph-v2.md)'s point 2 (what a section's hash is
over). It does not supersede it: every other point of ADR-0016 stands.

<a id="context"></a>
## Context
[DOCS › Hash-bound verification](../DOCS.md#verification) says that a verify-only change
edits only metadata, which is excluded from the hash, so it never cascades. That holds for
a section's own body, but not for the two other inputs of the hash:

- **Covered files are hashed whole.** A section that covers a Markdown file (for example
  ARCH › Control protocol, which covers `crates/jstream-protocol/**`, `…-control/**` and
  `…-server/**`, READMEs included) hashes that file's `@doc`, `@section` and `@rule`
  blocks too, `verified = { hash, by, note }` lines included. Re-verifying any section in
  one of those READMEs makes ARCH › Control protocol stale; re-verifying that makes
  ARCH › Shape and ARCH › MCP (which depend on it) stale, and so on.
- **`depends_on` targets are hashed as their body.** A whole-doc dependency (a bare ADR
  path) or a section that contains rules includes the metadata blocks inside it.
- **Every crate README's Purpose depends on the whole crate map**
  ([ARCH › Crate map](../ARCHITECTURE.md#crate-map)). Editing one row makes about 16
  Purpose sections stale, although each of them relies on its own row only.

Together these make parallel PRs conflict on `verified` lines all the time, and they
train builders to re-verify sections they had no reason to re-read, which weakens what a
verification means ([R-DOC-004](../RULES.md#R-DOC-004)).

<a id="decision"></a>
## Decision
1. **Metadata blocks are not hashed.** Wherever a hash reads Markdown (a section's own
   body, a covered `.md` file, a `depends_on` target's body), it first removes the
   well-formed metadata blocks: exactly the blocks the `xtask docs` parser recognises as
   metadata. That is the `@doc` block at the very start of the file, an `@section` block
   on the line right after an anchored `##` heading (not in ADRs), and an `@rule` block
   right after an anchored `### R-…` heading; in each case outside code fences, with an
   opening line that is exactly `<!-- @doc`, `<!-- @section` or `<!-- @rule`, a closing
   `-->` line, and a body that parses as TOML. Anything else, including a look-alike
   comment in the wrong place, one with text after its opening marker, one inside a code
   fence or one whose TOML does not parse, is still content and is hashed. So hidden text
   cannot escape the hash by dressing up as metadata.
   - Re-verifying a section therefore never makes another section stale.
   - Every content edit stales exactly what it staled before: the set of hashed content
     only loses the metadata blocks. Metadata edits that matter (`covers`, `depends_on`
     of the section itself, `applies_to`, `enforced_by`, `status`) are already either in
     the section's own hash inputs or [governance changes](../DOCS.md#governance).
2. **Row-level dependencies.** A `depends_on` entry may name one row of a Markdown table
   inside a section as `path#anchor/<key>`, for example
   `docs/ARCHITECTURE.md#crate-map/jstream-core` or
   `docs/ARCHITECTURE.md#crate-map/apps/*`. It resolves to the single table row in that
   section that has a cell consisting exactly of `` `<key>` ``; zero or several matching
   rows is a broken reference. Its hash input is that row's text, so the dependent goes
   stale only when its own row changes. Each crate README's Purpose depends on its own
   row instead of the whole crate map.
   - *Why this and not the crate's `Cargo.toml` metadata:* what the Purpose restates is
     the row (layer, responsibility, `unsafe`), which is prose in ARCH; the manifest holds
     only part of it. The row reference also keeps working unchanged when the crate map
     becomes a generated block ([DOCS › Generated content](../DOCS.md#generated)). It is
     generic (any anchored section with a table) but costs one resolver, and anchors
     never contain `/`, so the form is unambiguous.
3. **One-time migration.** `cargo xtask docs rehash` restamps hashes under the new rule.
   It writes a section's hash only when the stored hash equals what the old rule computes
   for the current tree (or what the new rule computes with each row dependency widened
   to its whole section, which covers sections that were fresh before being narrowed);
   it replaces only the `hash` value and keeps `by` and `note`. Every decision is made
   against the unmodified tree before anything is written, so the order of sections
   cannot matter. A stale section is never written: it stays stale for a person or
   agent to re-read and verify. It is run once in the PR that implements this ADR.

<a id="consequences"></a>
## Consequences
- Parallel PRs conflict on a `verified` line only when they re-verify the same section,
  which is what [DOCS › Hash-bound verification](../DOCS.md#verification) already
  promises; the claim becomes true for covered files and dependencies too.
- A crate-map edit stales only the Purpose sections of the crates whose rows changed (and
  sections that still depend on the whole table).
- Nothing is loosened for content. What is no longer hashed is text that GitHub does not
  render and that the checker reads as metadata; changes to it remain visible in the diff,
  and its load-bearing fields are governance-checked.
- The migration PR restamps many `verified` lines without new notes; its description must
  say so, so the PR's verification list is read as a migration, not as re-reads.
- Changes [DOCS › Sections](../DOCS.md#sections) (the `depends_on` row form) and
  [DOCS › Hash-bound verification](../DOCS.md#verification). R-DOC-003 and R-DOC-004 refer
  to that section and need no edit.
- **Governance change:** it edits `xtask/src/docs/**` and `docs/DOCS.md`, both protected
  ([DOCS › Governance](../DOCS.md#governance)). The PR needs `governance-approved` and a
  "Governance change" section.
