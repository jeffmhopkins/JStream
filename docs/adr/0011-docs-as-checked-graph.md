---
id: ADR-0011
title: "Documentation as a checked graph"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ext:adr-practice]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0011: Documentation as a checked graph

## Context
Most of JStream's code will be written by AI agents directed by one maintainer.
Agents rely heavily on docs, so a stale doc is worse than a missing one: it
gets followed confidently. Docs drift from three sources: code changes, changes
to other docs they rely on, and changes outside the repo (external APIs, OS
lifecycles, dependencies).

## Decision
Documentation is a graph of identified, versioned sections, checked by CI
(`cargo xtask docs`), as specified in [[DOCS]]:

1. Every doc has front matter with a permanent ID; `##` sections carry IDs, `covers`
   (code globs), `depends_on`, and a `verified` record.
2. All references use a single checked syntax (`[[ID#section]]`, `[[ext:…]]`,
   `[[fact:…]]`, `[[sym:…]]`, `[[cmd:…]]`) and must resolve. External URLs live only in
   `docs/external.toml`; repeated facts only in `docs/facts.toml`.
3. **Hard coverage gate:** a PR that changes covered code must edit or re-verify the
   covering sections. Content changes propagate one hop to dependents.
4. Revs, dates and histories are tool-written; staleness is computed from git history.
5. Verification is allowed by humans and agents, but needs a note saying what was
   checked; CI collects all attestations into one PR comment for review.
6. Tracking is per section, so a change only re-verifies the sections it touches.
7. Code carries checked module-level back-references (`Docs:` lines) to the sections and
   ADRs that govern it.
8. Rules have permanent IDs (`R-<AREA>-NNN`) with a lifecycle (active → retired), and
   each declares how it is enforced.
9. Time-based review: each doc and external has a `review_every`; CI warns at 1× and
   fails at 2×; a weekly sweep link-checks externals and re-checks facts.

## Consequences
- Every code PR has some doc overhead, kept small by per-section tracking and one-hop
  propagation.
- `xtask docs` must be built before any other code (first item of M0); until then the
  rules are enforced by review and metadata is maintained by hand.
- Rubber-stamp verification is still possible; the evidence note plus the PR summary
  table is the defense, and false verifications are treated as bugs.
- Docs are less pleasant to read raw (metadata comments, `[[…]]` refs); the generated
  `docs/INDEX.md` provides navigation.
