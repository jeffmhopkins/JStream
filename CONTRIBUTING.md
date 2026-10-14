<!-- @doc
id = "CONTRIBUTING"
title = "Contributing"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Contributing to JStream

<a id="status"></a>
## Who works on JStream
<!-- @section
-->

Until its first release, JStream has **one developer**, the maintainer
([@jeffmhopkins](https://github.com/jeffmhopkins)), who works with AI builder and reviewer
agents. It takes **no outside code contributions** before then: a pull request from
anyone else is closed without review, however good it is. The design is still moving
quickly, and every change goes through a decision and review process (below) that an
outside contributor can't drive.

What is welcome:

- **Bug reports and ideas**, as issues. Use the issue forms, so a report has what's
  needed to act on it.
- **Security problems**, reported privately as [SECURITY.md](SECURITY.md) describes.
  Never in a public issue.

This page will be revised before the first release, when the project decides whether
and how to take outside contributions.

<a id="how-changes-land"></a>
## How changes land
<!-- @section
depends_on = ["docs/RULES.md#R-GIT-001", "docs/RULES.md#R-GIT-002", "docs/RULES.md#R-GOV-003", "docs/RULES.md#R-GOV-005"]
verified = { hash = "b3:6ae1ffd1144e50b4", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "#100: first hash of its rule dependencies' text. Re-read against R-GIT-001 (protected main, squash), R-GIT-002 (Conventional Commit titles), R-GOV-003 (reviewer gate, auto-merge) and R-GOV-005 (Tier-1 is the maintainer's); every bullet matches" }
-->

This is how the maintainer and the agents work. It also tells an issue reporter what
happens to a report.

- **Every change is a pull request** to a protected `main`, squash-merged
  ([R-GIT-001](docs/RULES.md#R-GIT-001)). The title is a Conventional Commit
  ([R-GIT-002](docs/RULES.md#R-GIT-002)), and the description follows the
  [pull request template](.github/pull_request_template.md).
- **A reviewer agent gates every merge** ([R-GOV-003](docs/RULES.md#R-GOV-003)). It
  follows [REVIEW.md](REVIEW.md) and posts findings and a verdict. The PR auto-merges
  once every required check, including the review gate, is green.
- **Builder agents** start at [CLAUDE.md](CLAUDE.md). It points to the rules, docs and
  ADRs that govern each file, and to the PR loop they follow.
- **Product, platform and process decisions belong to the maintainer**
  ([R-GOV-005](docs/RULES.md#R-GOV-005)). They are recorded as ADRs in
  [docs/adr/](docs/adr/), and an agent may only propose them.
- **Docs are checked like code.** [docs/DOCS.md](docs/DOCS.md) explains the
  hash-verified sections and `cargo xtask docs check`.

<a id="licensing"></a>
## Licensing
<!-- @section
-->

JStream is dual-licensed under Apache-2.0 or MIT, at your option (see
[README › License](README.md#license)). Anything submitted for inclusion is licensed the
same way, with no additional terms.
