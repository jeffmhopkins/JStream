<!-- @doc
id = "DOCS"
title = "Documentation system"
kind = "normative"
status = "active"
review_every = "90d"
-->

# Documentation system

How JStream keeps documentation accurate as the code and the outside world change.
The binding rules are `R-DOC-*` in [RULES](RULES.md#documentation). The rationale is in
[ADR-0016](adr/0016-docs-as-checked-graph-v2.md).

**Idea:** docs are a graph of anchored sections. Each section is bound by a content hash
to the code it describes and to the sections it relies on. When anything in that set
changes, the hash no longer matches and CI asks for the section to be re-checked.
History, revisions and dates come from git and are never stored in files.

<a id="bootstrap"></a>
## Bootstrap status
<!-- @section
covers = ["xtask/src/docs/**"]
verified = { hash = "b3:7f5a40bab35400ab", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read against xtask/src/docs: uncovered-crate and applies_to-typo warnings now listed as reports; retired/superseded Docs refs fail; docs for path handling; not-yet is review and log" }
-->

The checker (`cargo xtask docs`) is built in stages during M0 ([ROADMAP](../ROADMAP.md#m0)).

- **Available now:** `cargo xtask docs check` validates doc identity and metadata
  schema, sections and rules (anchors, blocks, `enforced_by` vocabulary and check names),
  references (links, anchors, registered external URLs, superseded ADRs, `depends_on`), and
  facts. It runs as the `docs` CI check.
- **Hash-bound verification** ([Hash-bound verification](#verification)): `docs check`
  fails on any section whose stored hash no longer matches its text, covered files and
  dependencies, and `cargo xtask docs verify` records a re-read. Every `bootstrap`
  placeholder was replaced, each after a re-read, in the PR that added hashing.
- **Unmatched globs** ([R-DOC-012](RULES.md#R-DOC-012)): `covers` globs that match no
  tracked file fail (warn in drafts; retired docs are exempt); `applies_to` globs are
  checked for validity only. `verify` refuses while untracked files match the section's
  `covers`.
- **Code back-references** ([Code back-references](#back-references)): crate roots are
  checked in both directions, and every `Docs:` reference must resolve (sections, or whole
  docs such as ADRs; superseded and retired docs fail).
- **`cargo xtask docs for <path>`** ([Code back-references](#back-references)): prints
  what governs a file.
- **Reports (warnings):** crates no section covers, and `applies_to` globs naming an
  unknown crate ([PR gate](#gate)).
- **Not yet:** the `review` and `log` commands.
  Until they land, those rules are enforced by review.
- IDs, anchors and rule numbers became permanent at the first merge to `main` (#1).

<a id="identity"></a>
## Document identity
<!-- @section
-->

Every tracked Markdown file starts with a `@doc` metadata block: TOML inside an HTML
comment, so GitHub renders nothing for it. `<!-- @doc` must be the very first bytes of the
file: no byte-order mark, and no blank lines or other text before it.
ADRs also carry `tier = "maintainer"` or `tier = "agent"`
([R-GOV-005](RULES.md#R-GOV-005)); other docs omit it.

```markdown
<!-- @doc
id = "ARCH"                  # permanent, unique, UPPER-KEBAB; never reused
title = "Architecture"
kind = "normative"           # normative | adr | guide | plan | reference | generated
status = "active"            # active | draft | retired  (ADRs: proposed | accepted | superseded | rejected)
review_every = "90d"         # omitted for ADRs and generated docs
tier = "maintainer"          # ADRs only: maintainer | agent (R-GOV-005)
reviewed = { by = "human:jeffmhopkins", note = "what was re-read and why it still holds" }
replaced_by = "docs/NEW.md"  # retired docs only
retired_sections = { "old-id" = ["new-id-a", "new-id-b"] }   # tombstones, see Lifecycle
-->
```

**Tracked:** everything under `docs/` and `crates/*/README.md`, plus `README.md`,
`CLAUDE.md`, `REVIEW.md` and `ROADMAP.md`.
**Not tracked:** `docs/adr/TEMPLATE.md` and `.github/**`.

**Why TOML in a comment:** one parser for everything in the repo (`@doc`, `@section` and
`@rule` blocks are all TOML), explicit string types so globs and dates can't be mistyped,
and nothing visible on GitHub. GitHub only renders YAML (`---`) front matter; TOML (`+++`)
front matter would show up as raw text ([ADR-0016](adr/0016-docs-as-checked-graph-v2.md)).

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

The crate root (`src/lib.rs`, else `src/main.rs`) of every crate carries a `Docs:` line
listing every section whose `covers` matches files in that crate. The reverse is also
checked: each listed section must actually cover the crate.

- **Crates:** a crate is any directory with a tracked `Cargo.toml` below the workspace
  root. A file belongs to the deepest crate containing it, so a nested crate's files don't
  count toward the outer one. A crate without `src/lib.rs` or `src/main.rs` (custom
  `[lib] path`, binaries only) is not checked yet.
- **The line:** a single `//! Docs:` line in the file's leading `//!` comment. Blank lines
  and single-line inner attributes (`#![…]`) may come before it. References are
  comma-separated, written as in [References](#references): `path#anchor` for a section,
  or a bare `path` for a whole doc such as an ADR.
- **What counts:** only section references take part in the two-way check. Whole-doc
  references add context and only have to resolve. A reference to a superseded or retired
  doc fails. Sections in retired docs create no
  obligations ([R-DOC-012](RULES.md#R-DOC-012)). A crate that no section covers needs no
  line.
- **Module-level lines** are optional. When present, every reference must resolve.

`cargo xtask docs for <path>` prints what governs a file. Agents run it before editing
unfamiliar code. The path is relative to the repository root, wherever you run the
command, and doesn't have to exist yet. Absolute paths inside the repository also work.
It lists:

- **Sections** whose `covers` match the path (not in retired docs).
- **Rules** whose `applies_to` match it (not retired).
- **ADRs**, other than superseded ones, cited by a matched section's `depends_on`, by a
  link inside a matched section or rule, or by the `Docs:` line of the file or its crate
  root.

<a id="verification"></a>
## Hash-bound verification
<!-- @section
-->

`verified.hash` is a BLAKE3 digest (16 hex chars, `b3:` prefix) over:

1. the section's body text, normalized (metadata comment removed, LF line endings,
   trailing whitespace stripped);
2. for each git-tracked file matched by `covers`, in sorted order: its path plus a hash
   of its contents. In workflow files (`.github/workflows/`), the commit SHA and version
   comment of `uses:` pins are ignored, so Dependabot pin bumps don't make the
   specifying doc stale ([ADR-0018](adr/0018-ci-supply-chain-policy.md));
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
cargo xtask docs check                         # what is stale, and why
cargo xtask docs verify ARCH#gpu-pipeline \
    --by agent:claude --note "…"                # re-read (or edited) and it holds: write new hash
cargo xtask docs review ARCH --note "…"        # doc-level TTL review          (planned)
cargo xtask docs log ARCH#gpu-pipeline         # history, derived from git     (planned)
cargo xtask docs for crates/jstream-render/src/lib.rs   # what governs a file
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
2. **Unmatched globs:** any `covers` glob that matches no tracked file, unless the doc is
   `status = "draft"` (a warning then) or `status = "retired"` (exempt). `applies_to`
   globs only need to be valid: rules may name crates that don't exist yet
   ([R-DOC-012](RULES.md#R-DOC-012)).
3. **Broken references:** see [References](#references).
4. **Back-reference mismatches:** see [Code back-references](#back-references).
5. **Generated drift:** regenerated content differs from what is committed.
6. **Fact mismatches** for file-sourced facts.
7. **Accepted-ADR edits:** content changes to an accepted ADR. Only `status` and
   `superseded_by` may change, plus `@doc` delimiter changes made under a repo-wide
   format ADR ([R-DOC-009](RULES.md#R-DOC-009)).
8. **Identity violations:** a rule or section anchor that existed on `main` is missing
   and has no tombstone. See [Lifecycle](#lifecycle).
9. **Rule metadata:** see [Rules](#rules).

It also **reports**, without failing (as `warning:` lines), crates that no section covers,
and `applies_to` globs naming a crate (`crates/<name>/…`, `apps/<name>/…`) that neither
exists nor appears in [ARCHITECTURE › Crate map](ARCHITECTURE.md#crate-map), which is
probably a typo. The reviewer agent ([REVIEW.md](../REVIEW.md)) reads this report.

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
- The reviewer agent ([REVIEW.md](../REVIEW.md#checks)) re-reads each verified section
  against the diff. A note that doesn't engage with what changed is a blocking finding.
  A stock-phrase list alone is too easy to game.
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
    or `review-override`, and every Tier-1 ADR decision on merged PRs, marked direct or
    proxy, with the quoted instruction (the override and decision audit);
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
enforced_by = ["test:jstream_audio::rt::no_alloc", "review"]
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
| `review` | Judged by the reviewer agent ([REVIEW.md](../REVIEW.md)) | — |
| `human` | Needs the maintainer's judgment | — |

A rule enforced only by `review`/`human` is allowed, but is listed in the index.

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
depends_on = ["docs/adr/0019-review-gate-v2.md"]
verified = { hash = "b3:2ce92e8a86bea3ab", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read against ADR-0019 and ADR-0018: governance change list, label + justification, Dependabot pin-only pre-approval all match the current gate and rules" }
-->

An agent could pass the gate by weakening it. These changes are **governance changes**,
and the `governance` check fails on them unless the PR carries the label
`governance-approved` **and** its description has a non-empty "Governance change"
section saying what is loosened or tightened, and why:

- edits to `covers`, `applies_to`, `enforced_by` or `status` in any metadata block;
- any edit to an accepted ADR, including a metadata-container change under R-DOC-009's exception;
- retiring a rule; deleting or retiring a doc or section;
- edits to `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml`,
  `[workspace.lints]`, `.github/**`, `xtask/src/docs/**`, `docs/DOCS.md`, or `REVIEW.md`;
- new `#[allow(…)]` / `#[expect(…)]` attributes (these must also carry `reason = "…"`).

**Pre-approved:** a Dependabot PR that only changes pinned `uses:` references is justified by
[ADR-0018](adr/0018-ci-supply-chain-policy.md) and needs no label.

Agents and the maintainer may both apply the label. The weekly sweep lists every PR
merged with it, so the maintainer can audit them
([ADR-0019](adr/0019-review-gate-v2.md)).
