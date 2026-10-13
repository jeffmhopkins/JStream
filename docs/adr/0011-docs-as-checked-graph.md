<!-- @doc
id = "ADR-0011"
title = "Documentation as a hash-checked graph"
kind = "adr"
status = "superseded"
superseded_by = "docs/adr/0016-docs-as-checked-graph-v2.md"
-->

# ADR-0011: Documentation as a hash-checked graph

<a id="context"></a>
## Context
Most code will be written by AI agents directed by one maintainer. Agents follow docs
confidently, so a stale doc is worse than a missing one. Docs drift from three sources:
code changes, changes to the docs they rely on, and the outside world. A first design
stored revisions and histories in the files. Independent review showed that it was
inconsistent from its first commit and would conflict on every pair of parallel PRs.

<a id="decision"></a>
## Decision
Documentation is a graph of anchored sections, checked by `cargo xtask docs`, as specified
in [DOCS](../DOCS.md):

1. TOML front matter. Every `##` section has a permanent anchor plus `covers` and
   section-level `depends_on`.
2. A section's verification is a **content hash** over its own text, its covered files and
   the sections it depends on. Any change makes it stale. Propagation is exactly one hop.
3. **No history in files.** Revisions, dates and authors come from git.
4. References are standard Markdown links, checked. External URLs are registered with
   what we rely on. Facts are registered once and marked in prose.
5. Time-based review never blocks PRs. A weekly sweep keeps one pinned issue up to date.
6. Rules have permanent IDs and declare their enforcement, using a fixed vocabulary
   that the tool checks.
7. Weakening the checks is a governance change ([ADR-0015](0015-review-and-governance.md)).

The [ADR practice](https://adr.github.io/) is followed for decisions.

<a id="consequences"></a>
## Consequences
- Every code PR carries some doc overhead. Narrow `covers`, section-level edges and
  verify-to-edit ratios keep it small.
- `xtask docs` must exist early. Its MVP is built in M0 alongside the technical spikes.
- Docs are slightly noisier to read raw (anchors, metadata comments).
- Tighter `LINT.IfChange`-style pairing between code regions and doc sections was
  considered, and deferred until the glob-based gate shows it's needed.
