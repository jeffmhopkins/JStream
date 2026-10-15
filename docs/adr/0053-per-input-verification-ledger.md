<!-- @doc
id = "ADR-0053"
title = "Per-input verification stamps in one ledger file"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0053: Per-input verification stamps in one ledger file

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Refines [ADR-0016](0016-docs-as-checked-graph-v2.md)'s point 2 (a section's verification is
a content hash over its text, covered files and dependencies) a third time, after
[ADR-0039](0039-hash-ignores-metadata-and-row-dependencies.md) and
[ADR-0042](0042-narrow-hot-architecture-sections.md). It supersedes none of them: what is
hashed, and that any change makes a section stale, stand. It changes how the hash is
stored, and with it which merges count as stale.

<a id="context"></a>
## Context
Each section stored one hash over all its inputs, in a `verified` line inside its
`@section` block. A crate README's Purpose and Design sections cover the whole crate
(`crates/<name>/src/**`), so every PR that touches a crate re-verifies them, and any two
open PRs on the same crate conflict on those lines, wherever in the crate they work. After
three merges on one day, 40 of the 53 open PRs (drafts included) did not merge cleanly with
`main`. In 12 of them every conflict was between stamps; across all 40, 450 of 616
conflict hunks were stamps, led by the Purpose and Design sections of `jstream-protocol`, `jstream-core`,
`jstream-graph` and the daemon.

The conflict was the only guard against a stale section reaching `main`: required checks
don't re-run when `main` moves, so without it a PR verified against an older `main` merges
as is. GitHub also ignores custom merge drivers when it computes mergeability (ADR-0042),
and `docs merge-main` only automates the resolution on the builder's side; every conflicted
PR still needs a merge push and another review round.

A single hash can't tell which input moved. But the inputs are separate: two PRs that
change different files in a crate each re-read the section against their own change, and
git merges such changes without anyone reading the combination either. A doc that held
for change A and for change B, in different files, holds for both as often as the code
does.

<a id="decision"></a>
## Decision
1. **One stamp entry per input.** A section's stamp is one entry per input, keyed `body`
   (its own text), `file:<path>` (each covered file; `file:<path>#jstream-meta` for a
   manifest hashed as its jstream view) and `dep:<reference>` (each `depends_on` target),
   each with the hash that input had when it was last verified. What each input's hash
   covers is unchanged ([DOCS › Hash-bound verification](../DOCS.md#verification)). A
   section is fresh when its entries name exactly its current inputs with their current
   hashes.
2. **Stamps live in `docs/verified.toml`, not in `@section` blocks.** Every entry is a
   `["DOC-ID#anchor"."<input>"]` table header, one `stamp` line (`hash`, and `by` and
   `note` when `docs verify` wrote it) and a blank line, sorted by section and input. The
   header doesn't change while the input exists, and the blank line follows every entry,
   the last included, so they anchor git's diff: changes to different existing entries
   merge cleanly, and adding or removing entries usually does, apart from the cases in
   item 4. Only
   `cargo xtask docs verify` writes it, in that canonical form, which `docs check` enforces.
   An inline `verified` line fails `docs check`. No section can cover the ledger.
3. **`docs verify` writes only what changed.** Entries whose input still has the recorded
   hash keep their `by` and `note`; changed and new inputs get this verification's; removed
   inputs are dropped. No other line is rewritten, so a verify that only drops entries, or
   finds nothing changed, can't conflict with a parallel change to another entry (which
   would then resolve to fresh, item 4); with nothing changed it writes nothing.
4. **Two PRs conflict on an input both changed.** Then neither re-read the combination,
   and its `stamp` line conflicts, as the whole stamp did before. Adding or removing
   entries at the same spot on both sides can conflict too: both adding inputs between
   the same two neighbours (two PRs adding modules to one crate often do), one adding an
   input next to one the other removes, or both removing neighbouring inputs. A change on
   one side next to an add or remove on the other merges. `docs merge-main` takes main's
   side of any ledger conflict and reports the section stale, as it did for inline stamps.
   Every line a verify rewrites is for an input it saw change, so a conflicting line taken
   from main no longer matches the merged input and the section is stale. The exception is
   both PRs making the same change to an input, where main's entry already verified that
   exact content. The fixture test checks each case on stamps `restamp` wrote.
5. **One-time migration.** `cargo xtask docs migrate` moves each inline stamp whose hash
   still matches into the ledger, with its `by` and `note` on `body`, and drops one that no
   longer matches, so that section is stale until someone re-reads it. It replaces
   `docs rehash`, whose ADR-0039 migration is done. `docs merge-main` runs it after merging
   a branch that still has inline stamps, and commits the result separately.

<a id="consequences"></a>
## Consequences
- PRs that change different existing files of a crate no longer conflict on its README,
  and the re-verification after merging `main` that a stamp conflict forced is gone for
  them. PRs that both add or remove files can still conflict, as item 4 says, and are
  re-verified as before. A fixture test merges ledger edits with git itself to hold the
  layout to both.
- A doc can now reach `main` verified against each of two changes but not re-read against
  both together, when they changed different inputs. Interactions between such changes are
  what review of the second PR and later edits catch, as for code that merges cleanly.
- A clean merge can, in a contrived case, leave a section stale on `main`, making the
  `docs` check red until someone re-verifies it: one PR adds a covered file whose
  neighbours survive while the other narrows `covers` so that file is excluded. It fails
  loudly; it is never falsely fresh.
- The note of a verification is repeated on every entry it wrote, and a second verify in
  the same PR replaces it on entries whose input changed again
  ([DOCS › Verification notes](../DOCS.md#notes)). The ledger is one large generated
  file (about 3,000 lines); a PR's diff of it is a few lines per section it
  re-verified, and the reviewer reads those entries as the PR's verifications.
- `docs log` reads verifications from the ledger's history as well as the old inline lines;
  the migration commit shows as a restamp.
- Branches opened before this change conflict once more, on the inline lines `main` drops.
  `docs merge-main` resolves those and moves any inline stamps they added.
