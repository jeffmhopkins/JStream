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
   sections, and opens a **sync PR** into the wave titled `chore(wave): merge main into
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
5. GitHub deletes the wave branch when it merges. Every PR into it is already merged or
   moved; one still open would be moved to `main` without a CI run.

<a id="protocol"></a>
## Protocol versions
<!-- @section
depends_on = ["docs/adr/0055-wave-branches.md#consequences"]
-->

Every PR that bumps `PROTOCOL_VERSION` goes into the earliest open wave, so bumps reach
`main` in the order they were assigned. Two PRs that set the same `minor` merge without a
conflict, so the wave owner hands out the numbers one at a time:

1. **A number goes to the first ready bump.** A bump is ready when it is out of draft, its
   review gate has passed, and everything it waits on (an ADR, a PR below it) has merged
   into the wave. The owner keeps the order in the queue, but a PR that isn't ready doesn't
   hold up the ones behind it. It gets the next free number once it is ready.
2. **One bump holds the next number at a time.** That PR sets `minor` to the next number,
   along with every doc and schema snapshot that names it, and turns on auto-merge. The
   next ready bump may set its number too, but it keeps auto-merge off until the one
   before it has merged. Setting the number is a push, so the review gate runs one more
   round on it.
3. **The holder comes first.** When another PR lands in the wave and the holder conflicts,
   the holder's thread merges the wave in before anything else it is doing. The owner
   watches the holder and tells its thread what conflicts, and how to resolve them, as soon
   as it is dirty. `docs merge-main` resolves stamp conflicts and conflicts inside
   `@generated` blocks such as the README's progress table, so these merges are usually
   automatic.

If the wave is cut first, the queue continues in the next wave.
