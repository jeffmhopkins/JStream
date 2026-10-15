<!-- @doc
id = "CLAUDE"
title = "Agent instructions"
kind = "normative"
status = "active"
review_every = "90d"
-->

# CLAUDE.md

The entry point for AI agents. It deliberately does **not** restate rules. It tells you
where they are and how to find the ones that apply to your change.

<a id="orient"></a>
## Before you change anything
<!-- @section
-->

1. Read [docs/DOCS.md](docs/DOCS.md) once. It explains how docs, rules and verification work here.
2. For every file you will touch, run `cargo xtask docs for <path>`. It lists the rules,
   doc sections and ADRs that govern that file. Read them.
3. If your change contradicts an accepted ADR, stop and propose a new ADR
   ([R-ARCH-004](docs/RULES.md#R-ARCH-004)).

<a id="docs-duty"></a>
## Your documentation duty
<!-- @section
depends_on = ["docs/DOCS.md#verification", "docs/DOCS.md#notes"]
verified = { hash = "b3:d5323183193d9c30", by = "agent:claude@session_01BLMb1wRFFGKk8EK5MwkMUt", note = "Re-read against DOCS#notes, whose verify-to-edit sentence now says the sweep doesn't report it yet: the check/verify workflow, no-dates rule and hash bullets here don't rely on it and still hold" }
-->

- `cargo xtask docs check` lists the sections your change made stale. Re-read each one
  against your diff. If it's wrong, **edit it**. Either way, once it holds, record it:
  `cargo xtask docs verify <ID#anchor> --by agent:<name> --note "<what you checked>"`.
- Never type dates, revisions or history into docs; git provides them. Never edit
  `@generated` blocks; run `cargo xtask docs generate` (e.g. after changing an ADR's status).
- New external URL → register it in `docs/external.toml`. Repeated value → `docs/facts.toml`.
- New crate root → add its `//! Docs:` line.
- Never type a `hash` by hand. Only `docs verify` writes it (apart from the one-time
  `docs rehash` migration of ADR-0039, which never restamps a stale section).

<a id="pr-loop"></a>
## The PR loop (you are a builder)
<!-- @section
depends_on = ["REVIEW.md#output", "REVIEW.md#rounds"]
verified = { hash = "b3:a74acf3f822f102f", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: auto-merge commands, verdict markers never posted by builders, Dependabot step 6 (by author), ADR step 7 with proxy decisions all match REVIEW#output/#rounds" }
-->

1. Open the PR with **auto-merge (squash) enabled**. It merges by itself once every required
   check, including `review-gate`, is green. To enable it, use the GitHub MCP tool
   `enable_pr_auto_merge` with `mergeMethod: SQUASH`, or `gh pr merge <n> --auto --squash`.
   If enabling fails, say so in the PR. Never merge by hand instead.
2. A reviewer agent will comment findings `B<n>` (blocking) and `N<n>` (non-blocking), and
   end with a verdict line. **Never post `jstream-review` markers yourself, and never merge
   by hand.**
3. Answer every blocking finding: push a fix and reply naming the commit, or reply with a
   concrete rebuttal ([R-GOV-004](docs/RULES.md#R-GOV-004)). Each push resets the gate and
   triggers a new review round.
4. Non-blocking findings: fix them if cheap, or say they're deferred. Pre-existing problems
   become issues.
5. A `needs-human` label means the maintainer decides. Stop arguing that point.
6. **Dependabot PRs** (author `dependabot[bot]`): enable auto-merge (squash) on them,
   and handle CI failures as if they were your own PRs. Action-pin-only bumps are
   pre-approved governance ([ADR-0018](docs/adr/0018-ci-supply-chain-policy.md)).
7. **Writing an ADR:** set its `tier` per [R-GOV-005](docs/RULES.md#R-GOV-005). For
   `maintainer` (product and platforms, process and governance, superseding any ADR):
   open it as `proposed`, add `needs-human`, and wait for the maintainer's `/accept` or
   `/reject` decision, then set `status` to match. You may post a decision **only** to relay
   an explicit instruction from the maintainer that names the ADR, using ADR-0017's
   proxy format with their words quoted. **Never post a decision they didn't give.**

<a id="limits"></a>
## Things you never do
<!-- @section
-->

- Apply `governance-approved` or `review-override` without writing the matching
  justification section in the PR description. Apply them only after you have examined
  what the check flagged ([R-GOV-002](docs/RULES.md#R-GOV-002)). Every use goes to
  the maintainer's weekly audit.
- Change branch protection or repository settings, merge by hand, or post a review verdict.
- Weaken a check silently. `#[allow]` without a reason, narrowing `covers`, or editing lint
  or deny config is always a declared governance change.
- Verify a section you did not re-read.
- Claim a test ran when it didn't. List what wasn't run ([R-TEST-005](docs/RULES.md#R-TEST-005)).

<a id="commands"></a>
## Commands
<!-- @section
-->

Available now: `cargo fmt`, `cargo clippy`, `cargo test`, `cargo doc`, `cargo deny check`,
`cargo xtask docs check`, `cargo xtask docs verify`, `cargo xtask docs for`,
`cargo xtask docs generate` (rewrites `@generated` blocks, e.g. ADR status banners),
`cargo xtask docs log <DOC[#anchor]>` (history from git), `cargo xtask docs review <DOC>`
(doc-level TTL review),
`cargo xtask lint-deps` and `cargo xtask governance --base origin/main` (lists your
governance changes).
