+++
id = "DOCS"
title = "Documentation system"
kind = "normative"
status = "active"
review_every = "90d"
+++

# Documentation system

How JStream keeps documentation accurate as the code and the outside world change.
The binding rules are `R-DOC-*` in [RULES](RULES.md#documentation). The rationale is in
[ADR-0011](adr/0011-docs-as-checked-graph.md).

**Idea:** docs are a graph of anchored sections. Each section is bound by a content hash
to the code it describes and to the sections it relies on. When anything in that set
changes, the hash no longer matches and CI asks for the section to be re-checked.
History, revisions and dates come from git and are never stored in files.

<a id="bootstrap"></a>
## Bootstrap status
<!-- @section
covers = ["xtask/src/docs/**"]
verified = { hash = "bootstrap", by = "agent:claude", note = "Spec written before the tool exists" }
-->

The checker (`cargo xtask docs`) is built in M0 ([ROADMAP](../ROADMAP.md#m0)). Until it
exists:

- Rules are enforced by review.
- Every `verified` hash is the placeholder `"bootstrap"`. The PR that lands the tool
  must replace every placeholder with a real hash, which forces a genuine re-read of
  every section.
- IDs, anchors and rule numbers become permanent at the **first merge to `main`**.
  Before that, they may be renamed freely.

<a id="identity"></a>
## Document identity
<!-- @section
-->

Every tracked Markdown file starts with TOML front matter:

```toml
+++
id = "ARCH"                  # permanent, unique, UPPER-KEBAB; never reused
title = "Architecture"
kind = "normative"           # normative | adr | guide | plan | reference | generated
status = "active"            # active | draft | retired  (ADRs: proposed | accepted | superseded | rejected)
review_every = "90d"         # omitted for ADRs and generated docs
reviewed = { by = "human:jeffmhopkins", note = "what was re-read and why it still holds" }
replaced_by = "docs/NEW.md"  # retired docs only
retired_sections = { "old-id" = ["new-id-a", "new-id-b"] }   # tombstones, see Lifecycle
+++
```

**Tracked:** everything under `docs/` and `crates/*/README.md`, plus `README.md`,
`CLAUDE.md` and `ROADMAP.md`.
**Not tracked:** `docs/adr/TEMPLATE.md` and `.github/**`.

**Why TOML:** one parser for everything in the repo, explicit string types (globs and
dates can't be mis-typed), and no archived YAML crates.

| kind | Meaning | `review_every` |
|---|---|---|
| `normative` | How things are or must be (ARCH, RULES, DOCS, CLAUDE, crate READMEs) | 90d |
| `plan` | ROADMAP | 30d |
| `guide` | How-tos | 180d |
| `reference` | Hand-written reference | 90d |
| `adr` | Decision record, immutable once accepted | none (see [TTL](#ttl)) |
| `generated` | Produced by `xtask`; never edited by hand | none (drift-checked) |

<a id="sections"></a>
## Sections
<!-- @section
-->

Every `##` heading in a tracked doc (except ADRs) is a **section**. It is written like this:

```markdown
<a id="gpu-pipeline"></a>
## GPU pipeline
<!-- @section
covers = ["crates/jstream-render/src/compositor/**"]
depends_on = ["docs/adr/0004-wgpu-with-native-interop.md#decision", "docs/RULES.md#R-RT-002"]
verified = { hash = "b3:4f1c9a0e7d2b8c61", by = "agent:claude@<session-url>", note = "…" }
-->
```

- The `<a id>` anchor is the section's permanent ID. It is independent of the heading
  text, so the heading can be renamed freely.
- `covers`: code globs, relative to the repo root, that this section describes. Optional.
- `depends_on`: the **sections** this one relies on, as `path#anchor` or
  `path` for a whole ADR. These are section-level edges only. Docs never depend on whole
  docs, because coarse edges create re-verification storms.
- `verified` is required when either `covers` or `depends_on` is set.
- A `###` belongs to its `##`. Rules are the exception: they are `###` headings with their
  own anchor and an `@rule` block (see [Rules](#rules)).
- ADRs use the fixed anchored headings `context`, `decision` and `consequences`, with
  no `@section` blocks. An ADR is a single immutable unit.
- The text before the first `##` belongs to the doc as a whole (covered by its [TTL](#ttl)).
- Metadata values must not contain `-->`.

<a id="references"></a>
## References
<!-- @section
-->

References are **standard Markdown links**. They are clickable on GitHub and checked by the tool.

| Target | Write it as | Checked against |
|---|---|---|
| Section | `[GPU pipeline](ARCHITECTURE.md#gpu-pipeline)` | the `<a id>` anchors in that file |
| Rule | `[R-RT-001](RULES.md#R-RT-001)` | rule anchors |
| ADR | `[ADR-0004](adr/0004-wgpu-with-native-interop.md)` | the file exists; superseded ADRs are flagged |
| File in the repo | `` [`compositor.rs`](../crates/jstream-render/src/compositor.rs) `` | the working tree |
| External page | `[YouTube Live API](https://developers.google.com/…)` | must be registered in [`external.toml`](external.toml) |
| Pinned fact | `build 22000<!--fact:min-windows-build-->` | [`facts.toml`](facts.toml): the value must appear right before the marker |

In code, refs are plain repo-root paths (no brackets), so rustdoc never treats them as
intra-doc links:

```rust
//! Wgpu compositor and effect runtime.
//!
//! Docs: docs/ARCHITECTURE.md#gpu-pipeline, docs/adr/0004-wgpu-with-native-interop.md
```

Checks:
- A link to a missing file or anchor fails.
- A link to a **retired anchor** fails, with a hint naming its replacements.
- A link from a normative doc or from code to a **superseded ADR** fails. ADRs may cite
  anything, since history is legitimate there.
- An external URL that isn't registered in `external.toml` fails.
- Examples inside code spans and fenced blocks are not checked.

<a id="back-references"></a>
## Code back-references
<!-- @section
-->

The crate root (`lib.rs` / `main.rs`) of every crate carries a `Docs:` line listing
every section whose `covers` matches files in that crate. The reverse is also checked:
each listed section must actually cover the crate. Module-level `Docs:` lines are
optional, and checked when present.

`cargo xtask docs for <path>` prints the sections, rules (via `applies_to`) and ADRs that
govern a file. Agents run it before editing unfamiliar code.

<a id="verification"></a>
## Hash-bound verification
<!-- @section
-->

`verified.hash` is a BLAKE3 digest (16 hex chars, `b3:` prefix) over:

1. the section's body text, normalized (metadata comment removed, LF line endings,
   trailing whitespace stripped);
2. for each file matched by `covers`, in sorted order: its path plus a hash of its
   contents;
3. for each `depends_on` target: its reference plus a hash of its normalized body.

So the verification is stale as soon as **any** of these change: the section, a covered
file (including a file being added, removed or renamed), or a section it depends on.
That means:
- **No merge-base logic is needed.** Staleness is a pure function of the tree, so squash
  merges, rebases and merges from `main` all behave the same.
- **An edit made after a verify is caught**, because the hash no longer matches.
- **Propagation is exactly one hop.** A dependent's hash includes its target's text,
  but not the target's covered files. A verify-only change edits only metadata, which
  is excluded from the hash, so it never cascades.
- **Parallel PRs only conflict when they re-verify the same section**, which is a real
  semantic conflict. After merging `main`, the hash is recomputed and the section may
  need re-verifying against the combined change. That is correct.

Commands:

```sh
cargo xtask docs check                                     # what is stale, and why
cargo xtask docs verify ARCH#gpu-pipeline --note "…"       # re-read, still holds: write new hash
cargo xtask docs verify ARCH#gpu-pipeline --edited --note "…"  # after editing the text
cargo xtask docs review ARCH --note "…"                    # doc-level TTL review
cargo xtask docs log ARCH#gpu-pipeline                     # history, derived from git
cargo xtask docs for crates/jstream-render/src/lib.rs      # what governs this file
```

<a id="history"></a>
## History, revisions and dates
<!-- @section
-->

Nothing about history is stored in the files. `xtask docs log` derives it from git:

- **Revision n** of a section is the n-th commit on `main` that changed its normalized
  body.
- **Date, author and PR** come from that squash-merge commit.
- **Verifications** are the commits that changed its `verified` line, with the note.

`xtask docs index` renders an index (every ID, link, revision count, last change, last
verification, TTL status). It is published as a CI artifact and is **not committed**,
so it never causes merge conflicts.

<a id="gate"></a>
## The PR gate
<!-- @section
-->

`xtask docs check` runs on every PR (required check `docs`) and fails on:

1. **Stale verifications:** any section whose computed hash differs from its stored one.
2. **Unmatched globs:** any `covers` or `applies_to` glob that matches nothing, unless
   the doc is `status = "draft"` (a warning then).
3. **Broken references:** see [References](#references).
4. **Back-reference mismatches:** see [Code back-references](#back-references).
5. **Generated drift:** regenerated content differs from what is committed.
6. **Fact mismatches** for file-sourced facts.
7. **Accepted-ADR edits:** content changes to an accepted ADR. Only `status` and
   `superseded_by` may change.
8. **Identity violations:** a rule or section anchor that existed on `main` is missing
   and has no tombstone. See [Lifecycle](#lifecycle).
9. **Rule metadata:** see [Rules](#rules).

It also **reports**, without failing: crates that no section covers. The `ai-review`
check ([governance](#governance)) reads this report.

CI posts one PR comment listing every verification in the PR (section, by, note)
so a reviewer reads all of them in one place.

<a id="notes"></a>
## Verification notes
<!-- @section
-->

A verification claims that the section was **re-read against the change and still
holds**. The note says what was checked and why it still holds, e.g.:
`"Added P010 output; pipeline order, device ownership and interop unchanged"`.

- If the section is wrong, edit it. Do not verify a section you know is wrong.
- The `ai-review` check judges each verification against the diff, and flags notes
  that don't engage with what changed. A stock-phrase list alone is too easy to game.
- A false verification is a bug, and gets fixed and noted like one.
- The sweep reports each section's verify-to-edit ratio from git. A section that gets
  re-verified far more often than it is edited has `covers` that are too broad, so
  narrow them.

<a id="ttl"></a>
## Time-based review
<!-- @section
-->

Docs also go stale when nothing in the repo changes. **Time-based checks never fail a PR.**

- **Docs:** a doc's TTL clock restarts at the last git commit that changed its content
  or its `reviewed` line. Once `review_every` has passed, it is due.
- **ADRs** have no TTL, because they are immutable. The external resources they cite do
  have TTLs. A decision that no longer holds gets a new superseding ADR.
- **External resources** ([`external.toml`](external.toml)): each entry records its URL,
  *what we rely on from it*, `review_every`, and `checked`/`status`.
  - `checked` is only changed in a PR by someone who re-read the page against `relied_on`.
  - A link check proves the page loads, not that `relied_on` is still true.
  - `status = "unverified"` means nobody has checked it yet.
- **Facts** ([`facts.toml`](facts.toml)), by source:
  - `file`: checked on every PR.
  - `ext`: the sweep fetches the page and checks the value still appears.
  - `manual`: listed for re-check when due.
- **Weekly sweep** (scheduled workflow `sweep`):
  - link-checks every external with lychee (cached, authenticated; 403 and 429 count
    as *unknown*, not broken);
  - lists due docs and externals, fact drift, verify-to-edit ratios, pending
    enforcement past its milestone, and every PR merged with `governance-approved`
    or `ai-review-override` (the override audit);
  - **updates one pinned issue, "Docs due for review"**, instead of opening new ones.
  - A scheduled agent may pick items from that issue and open PRs. Those PRs go
    through the normal gate.

<a id="lifecycle"></a>
## Lifecycle of IDs
<!-- @section
-->

- **Permanence:** IDs, anchors and rule numbers are permanent from the first merge to `main`.
- **Split or merge:** a section that is split, merged or removed leaves a tombstone in
  `retired_sections`, e.g. `{ "old-id" = ["new-id"] }` (or `[]` when it's simply
  gone). Its anchor is never reused.
- **Deleted docs:** a removed doc stays as a stub with `status = "retired"` and
  `replaced_by`.
- **Rules:** a rule is retired, never deleted (see [Rules](#rules)).
- **Renames:** a renamed file appears in the hash of every section whose `covers` match
  it, under both its old and new path, so the gate fires.

<a id="rules"></a>
## Rules format
<!-- @section
-->

```markdown
<a id="R-RT-001"></a>
### R-RT-001: Audio thread does not allocate or block
<!-- @rule
status = "active"                                  # active | retired
enforced_by = ["test:jstream_audio::rt::no_alloc", "ai-review"]
applies_to = ["crates/jstream-audio/src/rt/**"]
replaced_by = "R-RT-009"                           # retired rules only, with a reason in the text
-->
```

`enforced_by` vocabulary. The tool checks every entry:

| Entry | Meaning | Checked how |
|---|---|---|
| `ci:<check>` | A required CI check | Must be in [the check list](RULES.md#R-GIT-003). Once that check's milestone has passed, the workflow must contain it. |
| `lint:<lint>` | A rustc or clippy lint | Must be configured in `[workspace.lints]` |
| `deny:<section>` | A cargo-deny policy | Section exists in `deny.toml` |
| `test:<path>` | A named test | Must appear in `cargo test -- --list` once its crate exists. Until then it is *pending*. |
| `ai-review` | Judged by the AI review check | — |
| `human` | Needs the maintainer's judgment | — |

A rule enforced only by `ai-review`/`human` is allowed, but is listed in the index.

<a id="facts"></a>
## Facts
<!-- @section
-->

A value that appears in more than one place, or comes from outside the repo (minimum OS
build, toolchain version), is registered once in `facts.toml`. In prose, it is
written next to its marker (`build 22000<!--fact:min-windows-build-->`) so the tool can
check it. ADRs are exempt from this rule (they are immutable records). Their facts are
covered by the TTLs of their external resources.

<a id="generated"></a>
## Generated content
<!-- @section
-->

Anything derivable is generated, never hand-written: the crate map (from
`[package.metadata.jstream]`), the protocol reference, the MCP tool list, and the
index. Generated blocks in committed docs are fenced as
`<!-- @generated begin <name> -->` … `<!-- @generated end <name> -->` and are
drift-checked.

<a id="governance"></a>
## Governance: protecting the checks themselves
<!-- @section
depends_on = ["docs/adr/0015-review-and-governance.md"]
verified = { hash = "bootstrap", by = "agent:claude", note = "Spec written before the tool exists" }
-->

An agent could pass the gate by weakening it. These changes are **governance changes**,
and the `governance` check fails on them unless the PR carries the label
`governance-approved` **and** its description has a non-empty "Governance change"
section saying what is loosened or tightened, and why:

- edits to `covers`, `applies_to`, `enforced_by` or `status` in any metadata block;
- retiring a rule; deleting or retiring a doc or section;
- edits to `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml`,
  `[workspace.lints]`, `.github/**`, `xtask/src/docs/**`, or `docs/DOCS.md`;
- new `#[allow(…)]` / `#[expect(…)]` attributes (these must also carry `reason = "…"`).

Agents and the maintainer may both apply the label. The weekly sweep lists every PR
merged with it, so the maintainer can audit them
([ADR-0015](adr/0015-review-and-governance.md)).
