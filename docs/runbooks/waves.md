<!-- @doc
id = "RUNBOOK-WAVES"
title = "Wave branches runbook"
kind = "guide"
status = "active"
review_every = "90d"
-->

# Wave branches runbook

How builders and wave owners use wave branches day to day. Why waves exist and what CI runs
where is in [ADR-0055](../adr/0055-wave-branches.md); this page is who does what, in what
order.

<a id="which-wave"></a>
## Which branch a PR targets
<!-- @section
depends_on = ["docs/adr/0055-wave-branches.md#decision"]
-->

There is one open wave per milestone group: `wave/m1` (M1a to M1d), `wave/m2` and
`wave/m3`. After a wave lands in `main`, the next one for that group is numbered:
`wave/m1-2`, `wave/m1-3`, and so on.

| The PR | Targets |
|---|---|
| Delivers a roadmap item | The open wave for that item's `milestone` in `docs/progress.toml` |
| Bumps `PROTOCOL_VERSION` | The earliest open wave, whatever its milestone ([Protocol versions](#protocol)) |
| A fix, docs or tests with no roadmap item | The earliest open wave |
| Exists to change CI, the review gate, the `xtask` commands, governance or the rules themselves (its own subject is `.github/`, `xtask/`, `REVIEW.md`, `CLAUDE.md` or `docs/RULES.md`) | `main` |
| Opened by Dependabot | `main` |
| An urgent fix the maintainer asks to land now | `main` |

A feature PR that also adds its own rule to `docs/RULES.md` or its own CI step still goes to
its wave: `governance` checks it there, and again on the wave's PR into `main`.

A stacked PR keeps the PR below it as its base. Only the bottom of a stack targets a wave.

To move an open PR, change its base with the GitHub MCP tool `update_pull_request`
(`base: "wave/m1"`), then bring that wave in ([Keeping current](#current)) and push. Changing
the base doesn't re-run CI, so a PR moved without a push keeps the checks it ran against its
old base. Never close and reopen a PR to move it.

<a id="current"></a>
## Keeping a PR current
<!-- @section
depends_on = ["docs/DOCS.md#verification"]
-->

A PR into a wave merges its wave, not `main`:
`cargo xtask docs merge-main --from wave/m1`. It resolves stamp conflicts and `@generated`
block conflicts the same way and lists the sections to re-read and verify. Merge, never rebase.
Verify each one with `docs verify --merge`, which keeps the note this PR already wrote on
an entry and adds what the merge changed. A plain `--note` replaces that note, so the
ledger no longer covers the PR's own change ([DOCS › notes](../DOCS.md#notes)). Every
verify keeps the wave's own note on an entry the wave re-verified, after the new text, so
write only this PR's text. When the section's inputs need different notes, give each its
own with `--per-input --merge-file <notes.toml> --from wave/<name>`
([DOCS › verification](../DOCS.md#verification)). To correct
a note on a section that is already fresh, use `--renote` with `--from wave/<name>`; a
stacked PR names the branch it is stacked on, `--from <base branch>`.

Everything else in [CLAUDE.md](../../CLAUDE.md#pr-loop) holds: auto-merge (squash) on once the
PR leaves draft, every blocking finding answered, and `docs/progress.toml` updated with the
PR number. The PR number is the PR into the wave. It reaches `main` with the wave.

<a id="owner"></a>
## The wave owner
<!-- @section
depends_on = ["docs/adr/0055-wave-branches.md#decision"]
-->

The coordinator names one thread as the owner of each wave. The owner:

1. Creates the wave branch from `main`, with the GitHub MCP tool `create_branch`.
2. Keeps the wave current with `main`. A `wave/*` branch only takes PRs, so the owner
   branches off the wave, runs `cargo xtask docs merge-main`, verifies the stale
   sections as [Keeping a PR current](#current) says, and opens a **sync PR** into the wave titled `chore(wave): merge main into
   wave/m1`. Turn on auto-merge with `mergeMethod: MERGE`, never squash: a squashed sync
   leaves `main` out of the wave's history, and the same conflicts come back at the next
   sync. Do this whenever another wave lands, before cutting.
3. Cuts the wave ([Landing a wave](#landing)).
4. Fixes what the wave's full CI run finds, with fix PRs into the wave. The thread that
   wrote the failing change helps if it is still running.

<a id="landing"></a>
## Landing a wave
<!-- @section
depends_on = ["docs/adr/0055-wave-branches.md#decision", "REVIEW.md#gather"]
-->

Waves land in milestone order: `wave/m1` before `wave/m2` before `wave/m3`. A later wave
can be cut early only if it doesn't conflict with `main` once the earlier one lands.

Cut a wave when every PR still targeting it is merged, or draft, or blocked on the
maintainer, and at least one PR has merged into it. Then:

1. Create the next wave (`wave/m1-2`) from `main`, and move every open PR that targets the
   wave onto it, drafts included. Nothing may merge into a wave after it is cut. Until the
   cut wave lands, a moved PR's diff also shows that wave's changes; once it lands, the
   owner syncs the new wave and each moved PR merges it again.
2. Sync the wave with `main` if it conflicts.
3. Open the PR from the wave into `main`, titled `chore(wave): land wave/m1`. List each PR
   it carries, with its number and title. Copy in the "Governance change" section of every
   carried PR that had one, and apply `governance-approved` if any did: `governance` diffs
   the whole wave against `main`. Turn on auto-merge (squash).
4. The reviewer reviews the sync merges ([REVIEW.md](../../REVIEW.md#gather)); the PRs into
   the wave were reviewed already. Every check runs, then the after-review Windows, macOS
   and end-to-end jobs.
5. Once it merges, open a PR on `main` that moves the wave's rows in `docs/progress.toml`
   from `review` to `done` (or `hardware`, naming the pending run in `note`), and drops or
   updates each row's `needs`, since the cut
   wave takes no more commits. A row keeps `review` while one of its PRs is still open.
6. GitHub deletes the wave branch when it merges. Every PR into it is already merged or
   moved; one still open would be moved to `main` without a CI run.

<a id="protocol"></a>
## Protocol versions
<!-- @section
depends_on = ["docs/adr/0055-wave-branches.md#consequences"]
-->

Every PR that bumps `PROTOCOL_VERSION` goes into the earliest open wave, so bumps reach
`main` in the order they land. Nobody hands out numbers: git serializes them.

1. **A bump takes the next number itself**: one more than its wave's when it is built. It
   sets `minor`, every doc and schema snapshot that names it, and the opening line of
   the protocol README's [version paragraph](../../crates/jstream-protocol/README.md#versions),
   which reads `` `PROTOCOL_VERSION` is 0.N because `` followed by this bump's reason. The
   test `the_readme_opens_on_the_current_version` holds that line to the number. It goes
   out of draft and turns on auto-merge like any other PR, without waiting on another
   bump.
2. **The first to land keeps the number.** Two bumps that took the same number each
   rewrote that one line with a different reason, so once one lands the other conflicts
   and GitHub won't merge it. Its thread merges the wave in
   (`cargo xtask docs merge-main --from wave/<name>`), keeps the landed bump's reason as
   the next clause of the paragraph, and takes the next number everywhere its own change
   named the old one, all in one push. That push starts a review round
   ([Without churn](#churn)).
3. **A conflicting bump comes first.** Its thread merges the wave in before anything else
   it is doing. The owner watches the wave's open bumps and tells a thread as soon as its
   bump conflicts. `docs merge-main` resolves stamp conflicts and conflicts inside
   `@generated` blocks such as the README's progress table, so the rest of these merges
   is usually automatic.

If the wave is cut first, an open bump moves to the next wave and takes the number after
the cut wave's.

<a id="churn"></a>
## Merging main without churn
<!-- @section
depends_on = ["docs/runbooks/waves.md#protocol"]
-->

Every push to a PR runs CI again, and every push but a merge of the base branch into a
passed head starts a new review round, so a merge of `main` costs at least a full CI run.
`main` doesn't require a PR to be up to date before it merges, so most of those merges buy
nothing. The same holds for a PR into a wave.

- **Merge `main` only when it is needed** (a PR into a wave merges its wave): the PR
  conflicts (GitHub then runs no CI on it), including a protocol bump whose number another
  bump took first ([Protocol versions](#protocol)), or it needs code that has just
  landed. Not after every change to `main`, and not to make a PR look fresh while
  it waits for review.
- **One push per round.** Answer every finding, merge `main` if it is needed, take the
  next protocol number if another bump took this one, and verify every stale section, then push once.
  Don't push again while a review round or CI is running on the head unless that round has
  failed.
- **Batch the ledger.** Run `docs verify` for every stale section before the push, never
  one push per section. A conflict that is only in `docs/verified.toml` or a `@generated`
  block is one `docs merge-main` and one push.
- **A merge keeps a passed review.** When a passed PR merges `main` (with
  `docs merge-main` and its `docs verify --merge` restamps, in a commit of their own) and
  its own diff doesn't change, `review-gate` carries the pass to the new head and there is
  no review round ([ADR-0109](../adr/0109-review-pass-carries-across-base-merges.md)). A
  conflict resolution that changes the PR's own lines, any other commit in the push, or a
  PR that hadn't passed gets a normal round.
- **Draft until it is ready.** CI skips drafts, so a PR still being built stays a draft.
