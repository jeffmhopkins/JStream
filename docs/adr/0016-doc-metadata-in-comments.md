<!-- @doc
id = "ADR-0016"
title = "Doc metadata in HTML comments, not front matter"
kind = "adr"
status = "accepted"
-->

# ADR-0016: Doc metadata in HTML comments, not front matter

<a id="context"></a>
## Context
[ADR-0011](0011-docs-as-checked-graph.md) put each doc's metadata in TOML front matter
(`+++ … +++`), following independent review advice. GitHub only understands YAML front
matter (`---`). TOML front matter is rendered as a raw text paragraph at the top of every
doc and every README, which the maintainer noticed on the repo home page.

<a id="decision"></a>
## Decision
Doc-level metadata moves into a `<!-- @doc … -->` HTML comment at the top of the file. It
holds the same TOML keys and uses the same parser as the existing `@section` and `@rule`
blocks. This amends item 1 of ADR-0011 ("TOML front matter"). The rest of ADR-0011 stands.

The metadata container of accepted ADRs is converted too. Their Context, Decision and
Consequences text is unchanged, which keeps to the intent of
[R-DOC-009](../RULES.md#R-DOC-009).

<a id="consequences"></a>
## Consequences
- Docs render cleanly on GitHub. Metadata stays visible in the raw file and to agents.
- All three metadata kinds now share one syntax (`<!-- @kind` + TOML + `-->`), which keeps
  the `xtask docs` parser simpler.
- Metadata values must not contain `-->`, a constraint that already applied to section blocks.
