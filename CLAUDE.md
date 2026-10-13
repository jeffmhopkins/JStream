---
id: CLAUDE
title: Agent instructions
kind: normative
status: active
rev: 2
review_every: 90d
depends_on: [RULES, DOCS, ARCH]
verified: {date: 2026-10-13, by: "agent:claude", rev: 2, note: "Added documentation workflow and rule-ID citations"}
---

# CLAUDE.md

Instructions for AI agents working in this repo. Humans: this is a good summary too.
This file summarizes; when they disagree, [[RULES]] and [[DOCS]] win, and this file
gets fixed.

## Read first
<!-- @section
id: read-first
rev: 1
-->
- `docs/ARCHITECTURE.md` [[ARCH]]: crate map and layering. Know which layer you're in before you edit.
- `docs/RULES.md` [[RULES]]: every rule has a permanent ID. Cite IDs in PRs and reviews.
- `docs/DOCS.md` [[DOCS]]: how docs are tracked. **Read it before your first PR.**
- `docs/adr/`: check before proposing a design change. If you contradict an accepted ADR,
  write a new ADR that supersedes it ([[RULES#R-ARCH-004]]).
- Before editing an unfamiliar file: `cargo xtask docs for <path>` lists the sections,
  rules and ADRs that govern it. Read them.

## Non-negotiables
<!-- @section
id: non-negotiables
rev: 1
-->
1. New capability → protocol first ([[RULES#R-ARCH-001]]).
2. `unsafe` only in platform/sys crates, always with `// SAFETY:` ([[RULES#R-UNSAFE-001]], [[RULES#R-UNSAFE-002]]).
3. No allocation, locks, blocking or `.await` on real-time threads ([[RULES#R-RT-001]]).
4. No FFmpeg, no GPL/LGPL dependencies ([[RULES#R-DEP-001]], [[RULES#R-DEP-002]]).
5. `jstream-ui` never depends on `jstream-core` ([[RULES#R-ARCH-003]]).
6. Secrets never in logs, files, events or MCP responses ([[RULES#R-SEC-001]]).
7. No `unwrap()` in non-test code ([[RULES#R-RS-004]]).
8. Docs change in the same PR as the code that makes them wrong ([[RULES#R-DOC-013]]).

## Documentation workflow
<!-- @section
id: docs-workflow
rev: 1
depends_on: [DOCS#coverage-gate, DOCS#verification-notes]
-->
For every PR:
1. `cargo xtask docs check` tells you which sections cover the files you changed, and which
   sections depend on docs you edited.
2. For each one, **re-read it against your change**:
   - if it is now wrong or incomplete, edit it, then run `cargo xtask docs sync --note "<what changed>"`;
   - if it still holds, run `cargo xtask docs verify <ID#section> --note "<what you checked>"`.
3. Never type revs, dates or history rows by hand. Never edit `@generated` blocks.
4. Cite with `[[…]]` only. New external URL → add it to `docs/external.toml`.
   A value that is repeated elsewhere → add it to `docs/facts.toml`.
5. New crate or module root → add its `//! Docs:` line ([[RULES#R-DOC-009]]).
6. A false verification is a bug. If you didn't re-read it, don't verify it.

Until `xtask docs` exists ([[DOCS#bootstrap]]), do these steps by hand.

## Before you push
<!-- @section
id: before-push
rev: 1
-->
```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo xtask lint-deps
cargo xtask docs check
```
If you changed the protocol: `cargo insta review` and include the schema diff in the PR.
If you changed rendering: `cargo xtask golden` and describe any image changes.

## Working style
<!-- @section
id: working-style
rev: 1
-->
- Small PRs, one logical change, Conventional Commit title with the crate as scope ([[RULES#R-GIT-003]]).
- Don't add a crate when a module will do. Don't add a dependency without saying why.
- Platform work you can't run on this machine (e.g. Windows APIs from Linux): make sure it
  compiles for the target (`cargo check --target x86_64-pc-windows-msvc` where possible),
  put hardware tests behind `hw-tests`, and say clearly in the PR what was not run.

## Revision history

<!-- @generated begin history by="xtask docs sync" -->
| rev | date | by | sections | note |
|---|---|---|---|---|
| 1 | 2026-10-13 | agent:claude | all | Initial version |
| 2 | 2026-10-13 | agent:claude | all | Documentation workflow; rules cited by ID |
<!-- @generated end history -->
