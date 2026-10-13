---
id: DOCS
title: Documentation system
kind: normative
status: active
rev: 1
review_every: 90d
depends_on: [ADR-0011, RULES]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
---

# Documentation system

How JStream keeps documentation accurate as the code changes. The binding rules are
`R-DOC-*` in [[RULES#documentation]]. This file explains the format and the
mechanics behind them. The rationale is in [[ADR-0011]].

The idea: **documentation is a graph of identified, versioned sections, linked to
the code they describe and to each other, and CI keeps the graph consistent.**

## Bootstrap status
<!-- @section
id: bootstrap
rev: 1
covers: [xtask/src/docs/**]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version; tool not yet built"}
-->

The checker (`cargo xtask docs`) is the first deliverable of M0 ([[ROADMAP#m0]]).
Until it exists, every rule here is enforced by review, and the agent or human
editing a doc performs the tool's steps by hand: bump revs, write `verified`, and
append history rows. Once the tool lands, the hand-written metadata is
re-validated by it and any drift is fixed in that PR.

## Document identity: front matter
<!-- @section
id: front-matter
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

Every tracked Markdown file starts with YAML front matter:

```yaml
---
id: ARCH                 # permanent, unique, UPPER-KEBAB. Never reused.
title: Architecture
kind: normative          # normative | adr | guide | plan | reference | generated
status: active           # active | draft | retired   (ADRs: proposed | accepted | superseded | rejected)
rev: 4                   # tool-managed, see "Revisions"
review_every: 90d        # TTL, see "Time-based review"
depends_on: [ADR-0001, RULES]       # whole-doc dependencies (optional)
superseded_by: ADR-0014             # ADRs only, when superseded
verified: {date: 2026-10-13, by: "agent:claude", rev: 4, note: "what was checked"}
---
```

Tracked files: everything under `docs/`, plus `README.md`, `CLAUDE.md`, `ROADMAP.md`,
and each crate's `README.md` (ID `CRATE-<crate-name>`).

| kind | Meaning | Default `review_every` |
|---|---|---|
| `normative` | Says how things are or must be (ARCH, RULES, DOCS, CLAUDE, crate READMEs) | 90d |
| `adr` | A decision record. Immutable once accepted (`R-DOC-011`) | 180d |
| `guide` | How-tos and tutorials | 180d |
| `plan` | ROADMAP and other forward-looking docs | 30d |
| `reference` | Hand-written reference material | 90d |
| `generated` | Produced by `xtask`; never edited by hand | none (drift-checked) |

## Sections: the unit of tracking
<!-- @section
id: sections
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

A `##` heading followed by a metadata comment is a **tracked section**. The comment
must come directly after the heading:

```markdown
## GPU pipeline
<!-- @section
id: gpu-pipeline                       # permanent within the doc; lowercase-kebab
rev: 3
covers: [crates/jstream-render/**]     # code this section describes (globs)
depends_on: [ADR-0004, RULES#R-RT-001] # docs/sections this section relies on
verified: {date: 2026-10-13, by: "agent:claude", rev: 3, note: "..."}
-->
```

- `covers` is optional. Sections without it are tracked for revisions and
  references, but not tied to code.
- `verified` is required on a section that has `covers`. Other sections rely on the
  doc-level `verified`.
- Headings are **not numbered**, and they can be renamed freely, because references
  use the `id`, never the heading text.
- A section ends at the next `##` heading. `###` subsections belong to their parent.
- Rules in [[RULES]] use `<!-- @rule ... -->` instead (see "Rules").

## References
<!-- @section
id: references
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

All cross-references use one checked syntax, in docs **and** in code comments:

| Form | Points at | Resolved against |
|---|---|---|
| `[[ARCH]]` | a document | front matter `id` |
| `[[ARCH#gpu-pipeline]]` | a section | section `id` |
| `[[RULES#R-RT-001]]` | a rule | rule ID |
| `[[ADR-0004]]` | an ADR | ADR front matter |
| `[[ext:yt-live-api]]` | an external resource | [`external.toml`](external.toml) |
| `[[fact:min-windows]]` | a pinned fact | [`facts.toml`](facts.toml) |
| `[[path:crates/jstream-render/src/lib.rs]]` | a file or directory | the working tree |
| `[[sym:jstream_render::Compositor]]` | a Rust item | rustdoc JSON of the workspace |
| `[[cmd:scene.create]]` | a protocol command/query/event | the generated protocol schema |

Rules:
- A reference that doesn't resolve fails CI.
- References inside inline code spans and fenced code blocks are examples and are not
  checked. TOML files are checked only in the fields that hold references
  (`used_by`, `source`).
- A reference to a **superseded ADR** or a **retired rule** fails CI, except inside
  ADRs and revision histories (historical context is allowed).
- External URLs live **only** in `external.toml`. Docs cite `[[ext:…]]`.
- Plain relative Markdown links (`[text](other.md)`) are allowed for convenience, and are
  also checked, but `[[…]]` is the canonical form.
- `docs/INDEX.md` (generated) lists every ID with a clickable link, since GitHub does
  not render `[[…]]` as links.

## Code back-references
<!-- @section
id: back-references
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

The graph runs both ways. The root module of each crate (`lib.rs` / `main.rs`) carries a
`Docs:` line in its module docs, naming every section whose `covers` matches files in
that crate, plus the ADRs and rules that govern it:

```rust
//! Wgpu compositor and effect runtime.
//!
//! Docs: [[ARCH#gpu-pipeline]] [[ADR-0004]] [[RULES#R-RT-001]]
```

- **Required** at crate roots: if section S covers files in crate C, C's root must cite S.
  The reverse is also checked: a crate root that cites S must be covered by S.
- **Optional** on significant modules (`mod.rs` or `foo.rs` module docs), and checked when present.
- Not used on individual lines; keep citations at module level.
- `cargo xtask docs for <path>` prints every section, rule (via `applies_to`) and ADR
  that governs a file. Agents run it before editing unfamiliar code.

## Revisions and dates
<!-- @section
id: revisions
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

**Nobody types a date or a revision number.** The tooling writes them, and git history
is the source of truth.

- **Section `rev`** increments when the section's content (anything other than its
  metadata block) changes. It increments **at most once per PR**, relative to the
  merge base.
- **Doc `rev`** increments once per PR in which any of its sections' content changed.
- **`verified`** records `{date, by, rev, note}`. Editing content also counts as
  verifying it, so the tool sets `verified` along with the rev bump.
- **`by`** is `human:<github-user>` or `agent:<name>`.
- **Revision history**: every doc ends with a generated `## Revision history` table,
  with one row per content change or verification:
  `| rev | date | by | sections | note |`. PR numbers are not written; `xtask docs log <ID>`
  resolves them from the squash-merge commits.
- The staleness check compares **git history** (the last commit touching a section
  vs. the last commit touching its covered paths), not the written dates. A wrong
  date can't hide staleness.

Commands:

```sh
cargo xtask docs sync   --note "what changed"     # after editing docs: bump revs, set verified, add history rows
cargo xtask docs verify ARCH#gpu-pipeline --note "what you checked and why it still holds"
```

## The coverage gate
<!-- @section
id: coverage-gate
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

In a PR, `xtask docs check` computes the changed files against the merge base and then:

1. **Coverage.** For each section whose `covers` matches a changed file, the PR must
   either change that section's content (rev bump) or re-verify it (`verified`
   updated in this PR). Otherwise CI fails and lists the sections.
2. **Propagation (one hop).** For each section or doc whose **content** changed, every
   section or doc that `depends_on` it must also be edited or re-verified in this PR.
   Verification alone does **not** propagate, which stops cascades.
3. **Rev integrity.** A content change without a rev bump fails, and so does a rev bump
   without a content change. Hand-edited generated blocks fail.
4. **Back-references** (see above) are consistent.
5. **ADR immutability.** Content changes to an `accepted` ADR fail. Only `status` and
   `superseded_by` may change.
6. **Rule IDs.** Every rule ID on the base branch still exists on the PR head (as active
   or retired).

CI then posts a **verification summary** comment on the PR, a table of every
section edited or verified, with rev, by and note, so the reviewer reads every
attestation in one place.

## Verification notes
<!-- @section
id: verification-notes
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

A verification is a claim that a section was **re-read against the change and still
holds**. The note must say what was checked:

- Good: `"P010 conversion added; pipeline order, device ownership and interop unchanged"`
- Rejected by CI: empty notes, notes under 20 characters, and stock phrases
  (`ok`, `lgtm`, `no change`, `still accurate`, `n/a`, `verified`).
- Agents must actually re-read the section. When a section is wrong, edit it, don't
  verify it. A verification that later turns out to be false is a bug like any other.

## Time-based review
<!-- @section
id: ttl
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

Docs also go stale when nothing in the repo changes.

- **TTL.** When `verified.date + review_every` has passed, CI **warns** on every PR.
  Past twice that, CI **fails** until the doc or section is re-verified or edited.
- **External resources** ([`external.toml`](external.toml)): each entry records the URL,
  *what we rely on* from it, when it was last checked, and its own `review_every`.
- **Pinned facts** ([`facts.toml`](facts.toml)): values that are easy to repeat and go
  stale (minimum OS, toolchain version, API versions). Each fact has exactly one
  source. Facts whose source is a file are checked automatically against it. Prose
  may restate a fact only next to its reference, e.g. `Windows 11 22H2 [[fact:min-windows]]`;
  the checker verifies that the value written on that line matches.
- **Weekly sweep** (scheduled CI, `xtask docs sweep`): link-checks every external
  resource, reports TTLs that are due within 14 days, re-checks facts, and opens one
  issue with the results. An agent can pick that issue up and open PRs with edits or
  verifications, each of which still goes through review.

## Rules
<!-- @section
id: rules-format
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

Each rule in [[RULES]] is a `###` heading with a permanent ID, followed by a
`@rule` block:

```markdown
### R-RT-001: No blocking work on real-time threads
<!-- @rule
status: active                 # active | retired
rev: 1
since: 2026-10-13
enforced_by: [test:assert_no_alloc, review]
applies_to: [crates/jstream-audio/**, crates/jstream-render/**]
replaced_by: R-RT-009          # retired rules only
-->
```

- IDs are `R-<AREA>-NNN` and are **never reused or renumbered**. A rule is retired by
  setting `status: retired`, with a reason and `replaced_by` when there is one. Its
  text stays.
- `enforced_by` lists how the rule is enforced: `lint:<name>`, `ci:<check>`,
  `test:<name>`, `deny:<section>`, or `review`. The checker verifies that every
  `lint:` is configured in the workspace lints and every `ci:` names a real required
  check. A rule enforced only by `review` is allowed, but it is listed in
  `docs/INDEX.md` so weakly enforced rules stay visible.
- `applies_to` drives `cargo xtask docs for <path>`.

## Generated content
<!-- @section
id: generated
rev: 1
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

Anything that can be derived is generated, never hand-written. That includes the
protocol reference, the MCP tool list, the crate map and layer diagram (from
`[package.metadata.jstream]` in each crate's `Cargo.toml`), `docs/INDEX.md`, and the
revision histories.

Generated regions are fenced:

```markdown
<!-- @generated begin crate-map by="xtask docs gen" -->
...
<!-- @generated end crate-map -->
```

CI regenerates them and fails on any difference.

## Revision history

<!-- @generated begin history by="xtask docs sync" -->
| rev | date | by | sections | note |
|---|---|---|---|---|
| 1 | 2026-10-13 | agent:claude | all | Initial version |
<!-- @generated end history -->
