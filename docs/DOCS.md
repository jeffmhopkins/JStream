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
-->

The checker (`cargo xtask docs`) is built in stages during M0 ([ROADMAP](../ROADMAP.md#m0)).

- **Available now:** `cargo xtask docs check` validates doc identity and metadata
  schema, sections and rules (anchors, blocks, and every `enforced_by` entry against
  [what it names](#rules): CI checks, configured lints, `deny.toml` sections, and tests
  read from the source),
  references (links, anchors, registered external URLs, superseded ADRs, `depends_on`), and
  facts. It also refuses git conflict markers left in a doc by a merge committed with its
  conflicts unresolved. It runs as the `docs` CI check.
- **Hash-bound verification** ([Hash-bound verification](#verification)): `docs check`
  fails on any section whose stamp no longer matches its text, covered files and
  dependencies, and `cargo xtask docs verify` records a re-read. Hashes skip metadata
  blocks and `depends_on` can name one table row
  ([ADR-0039](adr/0039-hash-ignores-metadata-and-row-dependencies.md)). A `covers` entry
  can hash manifests as their `#jstream-meta` view
  ([ADR-0042](adr/0042-narrow-hot-architecture-sections.md)). Stamps are one entry per
  input in `docs/verified.toml`, and `cargo xtask docs migrate` moved the inline ones there
  ([ADR-0053](adr/0053-per-input-verification-ledger.md)).
- **Unmatched globs** ([R-DOC-012](RULES.md#R-DOC-012)): `covers` globs that match no
  tracked file fail (warn in drafts; retired docs are exempt); `applies_to` globs are
  checked for validity only. `verify` refuses while untracked files match the section's
  `covers`.
- **Code back-references** ([Code back-references](#back-references)): crate roots are
  checked in both directions, and every `Docs:` reference must resolve (sections, or whole
  docs such as ADRs; superseded and retired docs fail).
- **`cargo xtask docs for <path>`** ([Code back-references](#back-references)): prints
  what governs a file.
- **Generated content** ([Generated content](#generated)): `cargo xtask docs generate`
  writes the ADR status banners, the ADR index, the README's progress block and the UI
  features summary, and `docs check` fails on their drift. `cargo xtask docs progress`
  lists progress entries the merged history contradicts.
  The crate map and the other generated references are not generated yet.
- **Reports (warnings):** crates no section covers, and `applies_to` globs naming an
  unknown crate ([PR gate](#gate)).
- **History and TTL review** ([History](#history), [Time-based review](#ttl)):
  `cargo xtask docs log` derives a doc's or section's history from git, and
  `cargo xtask docs review` writes a doc's `reviewed` stamp.
- **`cargo xtask docs merge-main`** ([Hash-bound verification](#verification)): merges
  `origin/main` (or, with `--from wave/<name>`, the wave a PR targets), resolves conflicts
  that are only between stamps or only inside `@generated` blocks, takes main's `by` and note
  for each ledger entry both sides recorded at the same content, moves inline stamps a branch from before ADR-0053 still
  has, and lists the sections left stale: each stale input with the note the branch had
  written on it and the merged commits that changed it, then the PRs those commits came
  from. `docs verify --merge` then keeps those notes and adds what the merge changed
  ([Verification notes](#notes)), and refuses an input whose file the branch changed after
  the merge. It and `--renote` list the merged PRs behind the entries they rewrite and
  name any the note leaves out. Every verify after the merge keeps the merged side's note
  on each entry that side re-verified, word for word ([Verification notes](#notes)).
- **Weekly sweep** ([Time-based review](#ttl)): the scheduled `sweep` workflow reports
  `cargo deny` findings, docs due for review, `cargo update --dry-run` and the override
  audit list in one issue. The parts that [Time-based review](#ttl) lists as not built
  yet are still missing.
- **Not yet:** `docs index`.
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

**Tracked:** every `.md` file in the repository, wherever it is, except the ones below.
Today that is `docs/`, `README.md`, `CLAUDE.md`, `REVIEW.md`, `ROADMAP.md` and the READMEs
under `crates/`, `apps/`, `spikes/` and `design/`, but a new Markdown file anywhere is
tracked too.
**Not tracked:** `docs/adr/TEMPLATE.md`, everything under `.github/`, and files whose
path starts with `LICENSE`.

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
-->
```

- The `<a id>` anchor is the section's permanent ID. It is independent of the heading
  text, so the heading can be renamed freely.
- `covers`: code globs, relative to the repo root, that this section describes. Optional.
  An entry may end in `#jstream-meta` (`crates/*/Cargo.toml#jstream-meta`): it covers
  the same files, but hashes each matched `Cargo.toml` as its jstream view only
  ([Hash-bound verification](#verification)). `#` is reserved in globs, and no other view
  exists ([ADR-0042](adr/0042-narrow-hot-architecture-sections.md)). Narrow `covers` to
  what the text states: a section re-verified on most PRs is too broad (see
  [Verification notes](#notes)).
- `depends_on`: the **sections** this one relies on, as `path#anchor` or
  `path` for a whole ADR. A `path#anchor` may also name a rule (`docs/RULES.md#R-…`),
  whose text up to the next rule or section is what is hashed
  ([Hash-bound verification](#verification)). These are section-level edges only. Docs never depend on whole
  docs, because coarse edges create re-verification storms.
  A section that relies on one row of a table can name just that row as
  `path#anchor/<key>`: the single table row in that section with a cell that is exactly
  `` `<key>` ``. Each crate README's Purpose depends on its own crate-map row this way
  (`docs/ARCHITECTURE.md#crate-map/jstream-core`, or `…#crate-map/apps/*` for the apps),
  so editing one row doesn't stale every crate. No matching row, or more than one, is a
  broken reference ([ADR-0039](adr/0039-hash-ignores-metadata-and-row-dependencies.md)).
- A section with `covers` or `depends_on` needs a stamp in `docs/verified.toml`, written by
  `cargo xtask docs verify` ([Hash-bound verification](#verification)). A `verified` key in
  the `@section` block is the old inline stamp and fails the check.
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
  anything, since history is legitimate there, and so may generated docs, since the ADR
  index lists every ADR ([Generated content](#generated)).
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

A section's **stamp** is one entry per input in [`docs/verified.toml`](verified.toml),
the ledger ([ADR-0053](adr/0053-per-input-verification-ledger.md)). Each entry holds a
BLAKE3 digest (16 hex chars, `b3:` prefix) of its input as it was when the section was last
verified against a change to it. The inputs, and their keys, are:

1. `body`: the section's body text, normalized (metadata blocks removed, LF line endings,
   trailing whitespace stripped);
2. `file:<path>`: each git-tracked file matched by `covers`, hashed by its contents. In
   Markdown files, metadata blocks are removed first. In workflow
   files (`.github/workflows/`), the commit SHA and version comment of `uses:` pins are
   ignored, so Dependabot pin bumps don't make the specifying doc stale
   ([ADR-0056](adr/0056-ci-supply-chain-policy-v2.md)). A file matched only through
   `#jstream-meta` entries contributes the hash of its **jstream view** instead, under the
   key `file:<path>#jstream-meta`: its
   `package.name`, its whole `[package.metadata.jstream]` table, and every entry of
   `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` and their
   `[target.<cfg>.…]` variants whose key or `package =` value starts with `jstream-`,
   rendered canonically (sorted keys), so comments, formatting and other tables don't
   count. A file that isn't a `Cargo.toml` or doesn't parse is hashed whole, and so is
   one that a plain entry also matches
   ([ADR-0042](adr/0042-narrow-hot-architecture-sections.md));
3. `dep:<reference>`: each `depends_on` target, hashed by its normalized body (metadata
   blocks removed), or by its one row for a `path#anchor/<key>` reference. A
   rule anchor (`RULES.md#R-…`) names the rule's text, up to the next rule or section. A
   whole-ADR target's body also leaves out its generated `adr-status` banner, so a status
   change alone doesn't make its dependents stale ([Generated content](#generated)).

**Metadata blocks** are exactly the blocks the parser reads as metadata: the `@doc` block
at the start of the file, an `@section` block right after an anchored `##` heading (not
in ADRs) and an `@rule` block right after an anchored `### R-…` heading, outside code
fences, whose opening line is exactly the marker and whose body parses as TOML. Anything
else is content and is hashed, including a look-alike comment elsewhere, one with text
after its marker, one in a code fence and one that isn't valid TOML, so text can't hide
from the hash by posing as metadata
([ADR-0039](adr/0039-hash-ignores-metadata-and-row-dependencies.md)).

A section is fresh when its stamp names exactly its current inputs, each with its current
hash. So it is stale as soon as **any** content among these changes: the section, a
covered file (including a file being added, removed or renamed), or a section (or row) it
depends on.
That means:
- **No merge-base logic is needed.** Staleness is a pure function of the tree, so squash
  merges, rebases and merges from `main` all behave the same.
- **An edit made after a verify is caught**, because an entry no longer matches.
- **Propagation is exactly one hop.** A dependent's `dep:` entry hashes its target's
  text, but not the target's covered files. A verify changes only the ledger, which no
  section can cover, so it never makes another section stale.
- **A merge never leaves a section falsely fresh; a conflict usually leaves it stale.**
  `docs verify` writes only the entries whose input changed or is new (they take its `by`
  and `note`; the others keep theirs) and drops removed ones; with nothing changed it
  writes nothing. Every entry is a `["DOC-ID#anchor"."<input>"]` header, which doesn't
  change while the input exists, then one `stamp` line and a blank line, the last entry's
  included. The headers and blank lines anchor git's diff, so parallel PRs that change
  different inputs of a section usually merge with it fresh. When the ledger conflicts,
  `docs merge-main` (below) takes main's side, which usually leaves the section stale for
  a re-read after merging `main`.
- **The ledger is generated.** Only `docs verify` (and `docs migrate`) writes it, in
  sorted, canonical form; `docs check` fails on any other form, and on an inline
  `verified` key in an `@section` block. A stamp for a section that no longer needs one
  is reported as a warning, and the next `docs verify` or `docs migrate` drops it.

Commands:

```sh
cargo xtask docs check                         # what is stale, and why
cargo xtask docs verify ARCH#gpu-pipeline \
    --by agent:claude --note "…"                # re-read (or edited) and it holds: restamp it
cargo xtask docs verify ARCH#gpu-pipeline \
    --by agent:claude --merge "…"               # the same after merge-main: keeps the branch's notes
cargo xtask docs verify ARCH#gpu-pipeline \
    --by agent:claude --renote "…"              # fresh section, better note: hashes unchanged
cargo xtask docs review ARCH \
    --by human:jeffmhopkins --note "…"          # doc-level TTL review: write `reviewed`
cargo xtask docs log ARCH#gpu-pipeline         # history, derived from git
cargo xtask docs for crates/jstream-render/src/lib.rs   # what governs a file
cargo xtask docs merge-main                    # merge origin/main; stamp and @generated conflicts take main's
cargo xtask docs merge-main --from wave/m1     # the same for a PR into a wave (ADR-0055)
cargo xtask docs migrate                       # move inline stamps to the ledger (below)
```

`docs merge-main` fetches and merges `origin/main` into a clean working tree. With
`--from wave/<name>` it merges that wave branch instead, and "main's side" below means the
wave's ([runbook](runbooks/waves.md#current)). Every
conflict in the ledger takes main's side: a merge never leaves a section falsely fresh,
and a conflict usually leaves it stale, for a re-read. The result must parse, and is
rewritten in canonical form. Once the ledger has no conflict left, each entry whose hash
equals main's entry for the same input takes main's entry, `by` and note included,
whether it conflicted or not. Both sides verified the same content of that input, so no
section's freshness changes; but the last note on it becomes the one main wrote for it,
and the branch's ledger diff keeps only the inputs it verified at other content. This goes
into the merge commit itself, and `merge-main` names each section it changed. A
fast-forward, or a merge with nothing to bring in, makes no merge commit and takes
nothing; neither does a merge whose ledger doesn't parse, which the steps after the commit
report. In a doc, a conflict whose two sides are only inline `verified` lines
inside `@section` blocks (either side may have none, as when `main` moved them to the
ledger) takes main's side too. A conflict whose two sides lie wholly inside `@generated`
blocks that `docs generate` writes, such as two PRs each regenerating the README's progress
table, takes main's side, and once every conflict is resolved every block is rewritten from
the merged sources, as `docs generate` would. If another conflict keeps the merge open, the
blocks still hold main's side until you run `docs generate` after resolving it. Any
other conflict, including one in a file that also has stamp conflicts, is left exactly as
git wrote it, and the merge stays open for you to finish by hand. When every conflict was
resolved this way it commits the merge, runs
`docs migrate` and commits what that changed separately, and prints the sections that are
now stale, for `docs verify` after a re-read. Under each one it prints each stale input,
the note this branch had written on it before the merge, and the merged commits that
changed the file the input lives in (for a merge commit, against its first parent). For a
`dep:` input, only those that changed the text it names count: the anchor's section or
row, or the whole doc's body.

`docs verify <DOC#anchor> --by <who> --merge "<text>"` re-verifies such a section after the
re-read. It finds the branch's latest merge (one that no `origin/main` or `origin/wave/*`
branch contains) and the entries the branch had written itself before it. An input whose
hash equals the merged side's entry gets that entry as it is, `by` and note included: the
branch changes nothing in it, so its diff against the merged side shows no rewrite there.
Each other entry a verify would rewrite, and each the branch had written whose input the
merge has since changed, gets the branch's note on it, then ` | merge: `, then the text.
An earlier merge's addendum is dropped first, because what it brought in is part of the
base by then. An entry the branch hadn't written gets `merge: ` and the text alone. Because
it rewrites the branch's entries whose input the merge changed even when they are fresh, it
also repairs a plain re-verify after the merge that replaced the branch's notes. It
refuses, naming each input, when the file an entry it would rewrite with the branch's note
lives in (the doc itself for `body`, the covered file, the target doc for `dep:`) differs,
committed or not, from what `merge-main` left: the merge, or its `docs migrate` commit.
The branch's earlier note doesn't cover such a later edit, so verify that section with
`--note` instead, covering both. An entry taken from the merged side is never refused,
since it carries no note of the branch's. It fails when the ledger on the branch before the
merge, at the merge base or on the merged side doesn't parse, rather than taking none or
every one of the entries for the branch's own; `merge-main` fails the same way when it
lists the branch's notes, after it has committed the merge.

`docs verify <DOC#anchor> --by <who> --renote "<note>"` replaces the `by` and `note` of a
section that is already fresh, as when a reviewer asks for a better note, without touching
any hash, and keeps the merged side's notes on those entries ([Verification
notes](#notes)). It rewrites only the entries this branch wrote: those that differ from the ledger
at the merge base of `HEAD` and `origin/main`, or of the wave named with
`--from wave/<name>`. A PR stacked on another PR's branch names that branch instead,
`--from <base branch>`, which must be an ancestor of `HEAD` (merge it first), so the base
PR's entries keep their notes. An entry equal to the one where `HEAD` left `origin/main`
or any `origin/wave/*` is kept too, so main's or a wave's notes survive when the stacked PR
has merged them ahead of its base. A bare `--renote` on a stacked PR compares against
`origin/main` and so rewrites the base PR's entries as well: name the base. It refuses a
stale section, which needs a re-read and `--note` or `--merge`, and a section this branch
never stamped. It fails when the ledger at that merge base doesn't parse, rather than
taking every entry for this branch's own.

`docs migrate` moves inline `verified` stamps, from before the ledger, into it. A stamp
whose hash is still what the inline rule (one hash over all the inputs above) computes
for the tree becomes a ledger stamp of the current inputs, with its `by` and `note` on
`body`. One that no longer matches is dropped and not moved, so the section stays stale
until someone re-reads and verifies it. It decides every section against the unchanged
tree before writing, drops stamps of sections that no longer need one, and rewrites the
ledger in canonical form. It replaced `docs rehash`, the one-time migration to ADR-0039's
rule.

<a id="history"></a>
## History, revisions and dates
<!-- @section
-->

Nothing about history is stored in the files. `xtask docs log` derives it from git:

- **Revision n** of a section is the n-th commit on `main` that changed its normalized
  body.
- **Date, author and PR** come from that squash-merge commit.
- **Verifications** are the commits that changed its entries in `docs/verified.toml`, with
  the `by` and note of the first changed entry that has them (or, before
  [ADR-0053](adr/0053-per-input-verification-ledger.md), its inline `verified` line). A
  commit that changed the stamp without a new `by` and note (`docs migrate`, or a merge
  that took main's entry) is shown as a restamp.
- **Reviews** of a whole doc are the commits that changed its `reviewed` line. The log of
  a doc also shows its TTL: when the clock last restarted and when the doc is due.

`docs log` takes a doc (`ARCH`), a section or a rule (`ARCH#gpu-pipeline`,
`RULES#R-DOC-011`), and `--rev <rev>` for another branch than `HEAD`. It follows the
first-parent line, so on `main` each entry is one squash-merged PR. A doc is followed by
its current path only, and a shallow clone shows only the history it has.

`xtask docs index` renders an index (every ID, link, revision count, last change, last
verification, TTL status). It is published as a CI artifact and is **not committed**,
so it never causes merge conflicts.

<a id="gate"></a>
## The PR gate
<!-- @section
-->

`xtask docs check` runs on every PR once it leaves draft: a draft skips it, and marking
the PR ready for review runs it on the head (required check `docs`,
[R-GIT-003](RULES.md#R-GIT-003)).
It fails on:

1. **Stale verifications:** any section whose stamp in `docs/verified.toml` doesn't name
   exactly its current inputs and hashes (the message lists the inputs that differ), any
   inline `verified` stamp, and a ledger that doesn't parse or isn't in canonical form.
2. **Unmatched globs:** any `covers` glob that matches no tracked file, unless the doc is
   `status = "draft"` (a warning then) or `status = "retired"` (exempt). `applies_to`
   globs only need to be valid: rules may name crates that don't exist yet
   ([R-DOC-012](RULES.md#R-DOC-012)).
3. **Broken references:** see [References](#references).
4. **Back-reference mismatches:** see [Code back-references](#back-references).
5. **Generated drift:** regenerated content differs from what is committed, or can't be
   generated (a `superseded_by` that names no tracked ADR, a `docs/progress.toml` that
   breaks its rules, a UI feature row with an unknown status). See
   [Generated content](#generated).
6. **Fact mismatches** for file-sourced facts.
7. **Accepted-ADR edits:** content changes to an accepted ADR. Only `status` and
   `superseded_by` may change, plus the generated `adr-status` banner and `@doc`
   delimiter changes made under a repo-wide format ADR ([R-DOC-009](RULES.md#R-DOC-009)).
8. **Identity violations:** a rule or section anchor that existed on `main` is missing
   and has no tombstone. See [Lifecycle](#lifecycle).
9. **Rule metadata:** see [Rules](#rules).

It also **reports**, without failing (as `warning:` lines), crates that no section covers,
and `applies_to` globs naming a crate directory (`crates/<name>/…`, `apps/<name>/…`) that
neither exists nor appears in [ARCHITECTURE › Crate map](ARCHITECTURE.md#crate-map) under
that directory, which is probably a typo. Crate-map names on its `apps/*` row are apps;
all others live under `crates/`, so `apps/jstream-types/…` is reported. The reviewer agent ([REVIEW.md](../REVIEW.md)) reads this report.

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
- A later `docs verify` of the same section in the same PR replaces the `by` and `note`
  of every entry whose input changed since the previous verify. An earlier note on such an
  entry survives only in git history (`docs log`), so the last note on each entry must
  cover the PR's whole change to that input: it is the one the reviewer reads.
- After `docs merge-main`, each stale section needs its own note: what its earlier note
  said about this PR's change, plus what the merge changed in that section's inputs.
  `docs verify --merge "<what the merge changed>"` keeps the earlier note on each entry
  this PR changes and adds the text. On an input whose hash equals the merged side's
  entry, it keeps that entry as it is, which `merge-main` has already given main's note.
- **The merged side's notes are kept** (#840). After the branch's own merge, every verify
  (`--note`, `--merge` or `--renote`) that rewrites an entry the merged side (main or the
  wave) re-verified since the branch left it ends the new note with that side's note, word
  for word, after ` | kept from the merged side: `. The notes the branch's earlier note
  kept that way are carried too, and none is added twice, even if the new text already
  holds it. Write only this PR's text; never copy the merged side's notes by hand. Before
  the branch's first merge, a verify still replaces main's note on an input the PR
  changes.
  `merge-main` prints the merged commits behind each
  stale input to write it from ([Hash-bound verification](#verification)), and under each
  stale section the PRs (`(#N)` in their subjects) those commits came from. A wave's
  landing squash (`chore(wave): land …`) counts as its own PR and the PRs the merge
  brought in through it. Its body is GitHub's list of every commit on the wave branch,
  with their bodies, so of its `* <subject> (#N)` lines only those naming a PR the
  branch's base doesn't already have count, and not an issue a subject cites as `(#A)`
  before its own `(#B)` or the body closes (`Closes #N`). Each counts whether or not that
  PR touched the input. The list can still hold an issue number that a wave-branch
  commit's subject ends with, since the history can't tell it from a PR's: don't credit
  a number that isn't a PR. `--merge` and
  `--renote` print the PRs behind the entries they rewrite (an entry kept as the merged
  side wrote it is left out) and name any the text leaves out: credit each PR whose change
  the note covers, by number. The text is
  still per section: one stock text across every stale section, such as "rechecked after merging
  main", fails review. A section this PR also changed after the merge takes `--note`, and
  `--merge` refuses it.
- A verify that only drops removed inputs, or finds no input changed, records no `by` or
  `note`: it rewrites no entry, so the ledger diff shows only the dropped entries. To
  improve the note on a fresh section, use `docs verify --renote`, never a hand edit. Say in
  the PR what you re-read after removing a covered file.
- The sweep is to report each section's verify-to-edit ratio from git (not built yet,
  see [Time-based review](#ttl)). A section that gets
  re-verified far more often than it is edited has `covers` that are too broad, so
  narrow them.

<a id="ttl"></a>
## Time-based review
<!-- @section
covers = [".github/workflows/sweep.yml"]
-->

Docs also go stale when nothing in the repo changes. **Time-based checks never fail a PR.**

- **Docs:** a doc's TTL clock restarts at the last git commit that changed its content
  or its `reviewed` line. Once `review_every` has passed, it is due.
  `cargo xtask docs review <DOC> --by <who> --note "…"` writes the `reviewed` line after a
  re-read. The note follows [Verification notes](#notes), and it must differ from the
  last one, since an unchanged line restarts nothing. ADRs and docs without
  `review_every` are refused.
- **ADRs** have no TTL, because they are immutable. The external resources they cite do
  have TTLs. A decision that no longer holds gets a new superseding ADR.
- **External resources** ([`external.toml`](external.toml)): each entry records its URL,
  *what we rely on from it*, `review_every`, and `checked`/`status`.
  - `checked` is only changed in a PR by someone who re-read the page against `relied_on`.
  - A link check proves the page loads, not that `relied_on` is still true.
  - `status = "unverified"` means nobody has checked it yet.
- **Facts** ([`facts.toml`](facts.toml)), by source:
  - `file`: checked on every PR.
  - `ext`: the sweep is to fetch the page and check the value still appears (not built
    yet, see the weekly sweep below).
  - `manual`: listed for re-check when due.
- **Weekly sweep** (scheduled workflow `sweep`, also run by hand from the Actions tab):
  - runs `cargo deny check` against the latest RustSec advisory DB;
  - lists docs due for review (from `cargo xtask docs log`, which prints `(due now)` once
    `review_every` has passed), and any `cargo xtask docs check` failure on `main`;
  - reports `cargo update --dry-run`: compatible updates, and dependencies held back by a
    version requirement or the toolchain;
  - lists the entries of `docs/progress.toml` that the merged history contradicts, and
    rows whose `needs` is missing or left over (`cargo xtask docs progress`,
    [Generated content](#generated));
  - lists every PR merged with `governance-approved` or `review-override` in the past
    eight days (the override audit, [R-GOV-002](RULES.md#R-GOV-002));
  - **rewrites one pinned issue, "Weekly sweep"**, instead of opening new ones, and
    reopens it if it was closed. Findings never fail the job; a tool that can't run does.
  - Not built yet: link-checking every external with lychee (cached, authenticated; 403
    and 429 count as *unknown*, not broken), due externals and `ext` facts, fact drift,
    verify-to-edit ratios, pending enforcement past its milestone, and every Tier-1 ADR
    decision on merged PRs, marked direct or proxy, with the quoted instruction (the
    decision audit).
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
| `lint:<lint>` | A rustc, clippy or other tool's lint | Must be set to a level other than `allow` in the root `Cargo.toml`'s `[workspace.lints]`. A `<tool>::<name>` entry (`clippy::`, `rustdoc::`) is looked up as `<name>` under that tool's own `[workspace.lints.<tool>]` table, and a name without a prefix under `[workspace.lints.rust]`. |
| `deny:<section>` | A cargo-deny policy | `deny.toml` has a top-level `[<section>]` table |
| `test:<path>` | A named test, or a module of tests | `<path>` is a crate (`-` written `_`) or an integration test target, then the test's path as `cargo test -- --list` lists it in one of that crate's targets, or in that target. The tool reads the path from the source without building, so tests a macro generates aren't seen. A module is accepted only as a prefix that ends at a `::` boundary with at least one test under it. A crate on the [crate map](ARCHITECTURE.md#crate-map) that doesn't exist yet makes the entry *pending*. |
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
`[package.metadata.jstream]`), the protocol reference, the MCP tool list, the index, and
the ADR status banners and ADR index, the README's progress table, and the UI features
summary. Generated blocks
in committed docs are fenced as `<!-- @generated begin <name> -->` …
`<!-- @generated end <name> -->` and are drift-checked.

`cargo xtask docs generate` writes them; running it again changes nothing. `docs check`
regenerates them in memory and fails, naming that command, when the committed text
differs. Two of them come from ADR `@doc` metadata
([ADR-0038](adr/0038-adr-status-banner-and-index.md)):

- **The `adr-status` banner** in every ADR: directly under the `# ADR-NNNN: …` line, after
  one blank line, the begin marker, one blockquote line and the end marker, then a blank
  line. A superseded ADR reads `> **Superseded** by [ADR-NNNN: Title](NNNN-….md). This
  decision no longer applies.`, with the successor named by `superseded_by` (a repo path);
  any other reads `> **Status:** <status>`. `docs/adr/TEMPLATE.md` carries the block with
  `proposed`. The banner is not ADR content: it may change in an accepted ADR, through the
  generator only ([R-DOC-009](RULES.md#R-DOC-009)).
- **The ADR index** `docs/adr/README.md`: a tracked doc with `kind = "generated"`, written
  whole, whose `adr-index` block is a table of every ADR under `docs/adr/` in number
  order: number, title, status, tier, and the successor of a superseded one.

It also generates one block from data rather than from docs:

- **The `progress` block** in `README.md`: a table of the milestones with how many of
  their items are merged, in review or planned, then each milestone's items, all from
  `docs/progress.toml`. That file lists the milestones and each roadmap item's
  milestone, title, `status` (`done`, `hardware` for merged work still waiting on a run
  on Windows hardware, named by a `note`, `review`, or `planned`) and the PRs that
  deliver it. An item not yet `done` lists in `needs` the steps it still needs, in order
  (`build`, `wave`, `hardware` or `maintainer`; a `hardware` item without it needs its
  run), and the block's **What's left** table shows each such item with them, linking
  the order they land in ([ROADMAP › Endgame](../ROADMAP.md#endgame)). `docs check`
  fails when the file doesn't parse or breaks those rules. A PR that delivers or advances
  an item updates its entry, so the README changes with the work.
  `cargo xtask docs progress` compares the file with the merged history (the `(#N)` of
  each squash commit, the `(#N)` of each `* …` line in a wave's landing squash, which
  lists the wave's PRs among every commit on the wave branch, and item IDs that PR titles name in parentheses) and
  prints, as Markdown, the entries it contradicts, the `planned` and `review` items with
  no `needs` and the `done` items still listing it; the weekly `sweep` reports them. It
  never fails a PR, because a PR's own merge commit doesn't exist yet.

And one from the tables of a hand-written doc:

- **The `ui-summary` block** in `docs/UI-FEATURES.md`: a table with one row per area,
  linking its section by heading and anchor, then the number of its feature rows and
  how many are in each status (✅ 🟡 ⬜ 🔒 ⏸), then a **Total** row. An area is any
  other `##` section of that doc with a Markdown table that has a `Status` column,
  in document order; each row of such a table is counted by its `Status` cell.
  `docs check` fails on a status that isn't one of the five, and on a block that finds
  no area. A doc without the block is left alone, so the block is opt-in.

<a id="governance"></a>
## Governance: protecting the checks themselves
<!-- @section
covers = ["xtask/src/governance.rs", ".github/workflows/governance.yml"]
depends_on = ["docs/adr/0019-review-gate-v2.md"]
-->

An agent could pass the gate by weakening it. These changes are **governance changes**,
and the `governance` check fails on them unless the PR carries the label
`governance-approved` **and** its description has a non-empty "Governance change"
section saying what is loosened or tightened, and why:

- edits to `covers`, `applies_to`, `enforced_by` or `status` in any metadata block;
- any edit to an accepted ADR, including a metadata-container change under R-DOC-009's
  exception, but not a change confined to its generated `adr-status` banner;
- retiring a rule; deleting or retiring a doc or section;
- edits to `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml`,
  `[workspace.lints]` or the root manifest's `[lints]`, `.github/**`, `xtask/src/docs/**`,
  `xtask/src/main.rs`, `xtask/src/governance.rs`, `xtask/src/lint_deps.rs` (which holds
  the `unsafe` and native-code allowlists), `docs/DOCS.md`, or `REVIEW.md`;
- adding or editing a package manifest (any `Cargo.toml` below the root) whose `[lints]`
  is not exactly `workspace = true`, including a new crate with no `[lints]` table;
- new `#[allow(…)]` / `#[expect(…)]` attributes, also inside `#[cfg_attr(…)]` (these must
  also carry `reason = "…"`).

**Pre-approved:** a Dependabot PR that only changes pinned `uses:` references is justified by
[ADR-0056](adr/0056-ci-supply-chain-policy-v2.md) and needs no label.

Agents and the maintainer may both apply the label. The weekly sweep lists every PR
merged with it, so the maintainer can audit them
([ADR-0019](adr/0019-review-gate-v2.md)).

**How the check works.** `cargo xtask governance --base <rev>` compares `HEAD` with its
merge base with `<rev>` (`git diff <rev>...HEAD`; committed changes only, and a rename
counts as a delete plus an add) and lists the governance changes. Run locally, it only
lists them. The `governance` workflow runs it on every PR event, label and description
edits included, against `origin/<base branch>`, and passes it the PR's labels,
description and author from the event payload. Then:

- **Justification:** the first `##` heading that starts with "Governance change" (any
  case) opens the section, which runs to the next `##` heading. It must still contain
  text once HTML comments are removed, as in `review-gate`'s `section()`.
- **Metadata** blocks are compared per doc, section anchor and rule ID. A section or rule
  that is gone counts as deleted or retired.
- **Suppressions** are found by a text scan of changed `.rs` files (`//` lines skipped),
  looking into `cfg_attr` attribute lists (nested too), and compared with the base as a
  multiset, so an unchanged attribute that moves doesn't count.
- **Package manifests** are judged by their content at `HEAD`: any added or edited one
  must inherit the workspace lints, whatever the edit was.
- **Pre-approval** needs the author `dependabot[bot]` and a diff limited to modified
  `.github/workflows/` files whose only differing lines re-pin the same action to a full
  commit SHA.
- **Container exception** ([R-DOC-009](RULES.md#R-DOC-009)): when an accepted ADR's
  metadata container changes and either its delimiters differ or its metadata block
  doesn't parse as TOML before or after the edit (so a block that stops parsing counts),
  that is a container change: its "Governance change" section must also cite, as
  `ADR-NNNN`, an ADR that is accepted at `HEAD`. A content change is reported separately.
  Whether the cited ADR really is a repo-wide format ADR is judged in review.
- **Status banner** ([R-DOC-009](RULES.md#R-DOC-009)): before comparing an accepted ADR,
  each `adr-status` block whose inside is exactly one `> ` line is removed from both
  sides, so a change confined to it is not listed. A block of any other shape stays, and
  counts as content. The banner's text is left to `docs check`, which fails unless it is
  exactly the generator's output for the ADR's metadata, and an edit to that metadata
  (`status` when superseding) is itself listed.

**What the workflow guarantees.** It builds the checker from the PR's base commit, with
the base's toolchain file and lockfile and no build cache, then checks out the PR head
and runs that binary on it. The checker reads the head only through `git`. So edits a PR
makes to `xtask`, `rust-toolchain.toml` or `Cargo.lock` never change how its own check
runs. The workflow file itself is the exception: GitHub runs a `pull_request` workflow as
the PR's own version of it defines it, so a PR that edits
`.github/workflows/governance.yml` can change its own check. Such an edit is still a
listed `.github/**` change that needs the label and justification, but if the edit
disables the check, only review and the weekly audit stand behind it.

<a id="dependency-lint"></a>
## Dependency lint
<!-- @section
covers = ["xtask/src/lint_deps.rs"]
depends_on = ["docs/RULES.md#R-ARCH-002", "docs/RULES.md#R-ARCH-003", "docs/RULES.md#R-ARCH-005", "docs/RULES.md#R-UNSAFE-001", "docs/RULES.md#R-DEP-002", "docs/RULES.md#R-DEP-003"]
-->

`cargo xtask lint-deps` (CI check `lint-deps`) checks the rules whose `enforced_by` names
`ci:lint-deps`. It reads `cargo metadata --locked` for the whole workspace, including the
resolved dependency graph, reports each problem as `<where>: <rule>: <what>`, and fails if
there is any. `xtask/src/lint_deps.rs` holds its allowlists, so editing it is a
[governance change](#governance).

- **Declared metadata** ([R-ARCH-005](RULES.md#R-ARCH-005)): every workspace member has
  `[package.metadata.jstream]` with `layer`, a non-empty `summary` and a boolean
  `unsafe_allowed`. `layer` must be a non-negative TOML integer: `"2"` or `2.0` fails.
- **Layering** ([R-ARCH-002](RULES.md#R-ARCH-002)): every dependency on another member is
  in a strictly lower layer. Every dependency kind counts, dev-dependencies included. A
  member without a valid `layer` is reported once, under R-ARCH-005, and skipped here.
- **Clients** ([R-ARCH-003](RULES.md#R-ARCH-003)): `jstream-ui`, `jstream-mcp`,
  `jstream-server` and `jstream-cli` never reach `jstream-core`, directly or through other
  members. The finding shows the path.
- **`unsafe`** ([R-UNSAFE-001](RULES.md#R-UNSAFE-001)): only the platform, vcam, CLAP
  wrapper, browser and browser-helper crates on the `unsafe` allowlist and `*-sys` crates
  may declare `unsafe_allowed = true`. No member but `jstream-plugin-host` reaches
  `jstream-plugin-clap`, directly or through other members
  ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)); the finding shows the path. For a
  member that declares `false`, every library and binary crate root must carry
  `#![forbid(unsafe_code)]` (alone or among other lints, possibly wrapped over several
  lines) among its leading inner attributes: only whitespace, comments and other inner
  attributes may come before it. One inside a comment or later in the file doesn't count.
- **Native code** ([R-DEP-002](RULES.md#R-DEP-002),
  [ADR-0061](adr/0061-source-types-and-decoding-scope-v2.md)): every resolved package, members or
  not, that has a `links =` key or whose build script compiles C must be on the
  native-code allowlist, with any feature that entry requires enabled. "Compiles C" is a
  heuristic: a build-dependency on `cc`, `cmake`, `autotools` or `nasm-rs`, or on a build
  helper that has one of them as a normal dependency, one level down (`cxx-build` → `cc`).
  Deeper chains, and build scripts that run a compiler without those crates, are not
  detected; the `deny.toml` bans and review cover them. The graph is the one default
  features resolve, so a package that only an off-by-default feature reaches (CEF's
  `cef-dll-sys`, behind `jstream-browser`'s `helper` feature) isn't checked: its allowlist
  entry records the decision, and `cargo deny check --all-features` still sees it.
- **Workspace dependencies** ([R-DEP-003](RULES.md#R-DEP-003)): every entry in a member's
  `[dependencies]`, `[dev-dependencies]` and `[build-dependencies]` tables, including
  `[target.<cfg>.…]` ones, is `<dep>.workspace = true`, with its version in the root
  `[workspace.dependencies]`.
