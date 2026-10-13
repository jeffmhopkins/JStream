+++
id = "CLAUDE"
title = "Agent instructions"
kind = "normative"
status = "active"
review_every = "90d"
+++

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
   *Until `xtask docs` exists (M0):* search [docs/RULES.md](docs/RULES.md) for rules whose
   `applies_to` matches your paths, and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for
   sections whose `covers` do.
3. If your change contradicts an accepted ADR, stop and propose a new ADR
   ([R-ARCH-004](docs/RULES.md#R-ARCH-004)).

<a id="docs-duty"></a>
## Your documentation duty
<!-- @section
depends_on = ["docs/DOCS.md#verification", "docs/DOCS.md#notes"]
verified = { hash = "bootstrap", by = "agent:claude", note = "v2: hash-bound verify workflow" }
-->

- `cargo xtask docs check` lists the sections your change made stale. Re-read each one
  against your diff. If it's wrong, **edit it**. If it still holds,
  `cargo xtask docs verify <ID#anchor> --note "<what you checked>"`.
- Never type dates, revisions or history into docs; git provides them. Never edit
  `@generated` blocks.
- New external URL → register it in `docs/external.toml`. Repeated value → `docs/facts.toml`.
- New crate root → add its `//! Docs:` line.
- *Before the tool exists:* set `hash = "bootstrap"` on any section you edit or re-read,
  and put your note in `note`.

<a id="limits"></a>
## Things you never do
<!-- @section
-->

- Apply `governance-approved` or `ai-review-override` without writing the matching
  justification section in the PR description. Apply them only after you have examined
  what the check flagged ([R-GOV-002](docs/RULES.md#R-GOV-002)). Every use goes to
  the maintainer's weekly audit.
- Change branch protection or repository settings.
- Weaken a check silently. `#[allow]` without a reason, narrowing `covers`, or editing lint
  or deny config is always a declared governance change.
- Verify a section you did not re-read.
- Claim a test ran when it didn't. List what wasn't run ([R-TEST-005](docs/RULES.md#R-TEST-005)).

<a id="commands"></a>
## Commands
<!-- @section
-->

Nothing is runnable yet: the Cargo workspace arrives in M0. From then on: `cargo fmt`, `cargo clippy`,
`cargo test`, `cargo doc`, `cargo deny check`, `cargo xtask docs check|verify|for|log`,
`cargo xtask lint-deps` and `cargo xtask governance`.
