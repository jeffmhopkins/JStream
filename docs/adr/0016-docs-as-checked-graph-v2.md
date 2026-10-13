<!-- @doc
id = "ADR-0016"
title = "Documentation as a hash-checked graph (v2: metadata in comments)"
kind = "adr"
status = "accepted"
-->

# ADR-0016: Documentation as a hash-checked graph (v2: metadata in comments)

Supersedes [ADR-0011](0011-docs-as-checked-graph.md).

<a id="context"></a>
## Context
[ADR-0011](0011-docs-as-checked-graph.md) established documentation as a hash-checked graph,
with each doc's metadata in TOML front matter (`+++ … +++`). GitHub only renders YAML front
matter (`---`). TOML front matter appears as a raw text paragraph at the top of every doc
and README, which the maintainer noticed on the repo home page. ADRs can't be partially
amended ([R-DOC-009](../RULES.md#R-DOC-009)), so this ADR restates ADR-0011's whole
decision with the one change, and supersedes it.

<a id="decision"></a>
## Decision
Documentation is a graph of anchored sections, checked by `cargo xtask docs`, as specified
in [DOCS](../DOCS.md):

1. **Doc metadata lives in a `<!-- @doc … -->` HTML comment** at the very start of the
   file. It is TOML, in the same form as the `@section` and `@rule` blocks, and GitHub
   renders nothing for it. Every `##` section has a permanent anchor plus `covers` and
   section-level `depends_on`. *(This is the only change from ADR-0011, which used `+++`
   front matter.)*
2. A section's verification is a **content hash** over its own text, its covered files and
   the sections it depends on. Any change makes it stale. Propagation is exactly one hop.
3. **No history in files.** Revisions, dates and authors come from git.
4. References are standard Markdown links, checked. External URLs are registered with
   what we rely on. Facts are registered once and marked in prose.
5. Time-based review never blocks PRs. A weekly sweep keeps one pinned issue up to date.
6. Rules have permanent IDs and declare their enforcement, using a fixed vocabulary
   that the tool checks.
7. Weakening the checks is a governance change ([ADR-0015](0015-review-and-governance.md)).
8. **The metadata container is not ADR content.** The delimiter syntax of `@doc` blocks
   may change in every doc, accepted ADRs included, but only through a repo-wide format
   ADR (like this one), as a declared governance change, leaving each ADR's Context,
   Decision and Consequences byte-identical ([R-DOC-009](../RULES.md#R-DOC-009)).

The [ADR practice](https://adr.github.io/) is followed for decisions.

<a id="consequences"></a>
## Consequences
- Docs render cleanly on GitHub. Metadata stays visible in the raw file and to agents.
- All three metadata kinds share one syntax (`<!-- @kind` + TOML + `-->`), which keeps the
  `xtask docs` parser simpler. Metadata values must not contain `-->`.
- Every code PR carries some doc overhead. Narrow `covers`, section-level edges and
  verify-to-edit ratios keep it small.
- `xtask docs` must exist early. Its MVP is built in M0 alongside the technical spikes.
- Tighter `LINT.IfChange`-style pairing between code regions and doc sections was
  considered, and deferred until the glob-based gate shows it's needed.
