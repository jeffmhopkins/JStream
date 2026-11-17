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
- **Product-level decisions belong to the maintainer**
  ([R-GOV-005](docs/RULES.md#R-GOV-005)): scope, licensing, security and privacy,
  public compatibility promises and spending. They are recorded as ADRs in
  [docs/adr/](docs/adr/), and an agent may only propose them. Agents decide and record
  everything else.
- **Docs are checked like code.** [docs/DOCS.md](docs/DOCS.md) explains the
  hash-verified sections and `cargo xtask docs check`.

<a id="working-on-the-code"></a>
## Working on the code
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#repo-layout", "docs/ARCHITECTURE.md#crate-map"]
-->

**Find your way around.** [ARCH › Repository layout](docs/ARCHITECTURE.md#repo-layout)
says what each top-level folder holds, and [ARCH › Crate map](docs/ARCHITECTURE.md#crate-map)
gives every crate's layer and job. Each crate and app has a README; its first section says
what the crate is for and which rules apply to it. To see everything that governs one
file (rules, doc sections and ADRs), run `cargo xtask docs for <path>`.

**Build and run.** [README › Build and run](README.md#build) has the commands. Everything
builds on every OS, and the tests run on fakes, so a Linux machine can work on Windows or
macOS code. On Linux the GPU tests need Vulkan; Mesa's software driver (lavapipe, in
`mesa-vulkan-drivers`) is what CI uses.

**Before you push**, run what CI runs ([`ci.yml`](.github/workflows/ci.yml)):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo xtask docs check          # stale or broken doc sections
cargo xtask lint-deps           # crate layers, unsafe and dependency rules
cargo deny check
```

CI also lints the macOS crate for `aarch64-apple-darwin` and the web UI for `wasm32`, and
builds the UI with `trunk`. Those jobs are listed in `ci.yml` with their exact commands.

**When a check says a doc is stale**, re-read the section it names against your change,
edit it if it's wrong, and record the re-read with `cargo xtask docs verify`
([CLAUDE › Your documentation duty](CLAUDE.md#docs-duty),
[DOCS › Hash-bound verification](docs/DOCS.md#verification)).

**Testing on real hardware.** The fakes don't cover devices, encoders or the GPU interop.
Each OS has a runbook for the checks only a real machine can run:
[Windows](docs/runbooks/windows-hardware-pass.md), [macOS](docs/runbooks/mac-testing.md)
and [Linux](docs/runbooks/linux-testing.md).

<a id="licensing"></a>
## Licensing
<!-- @section
-->

JStream is dual-licensed under Apache-2.0 or MIT, at your option (see
[README › License](README.md#license)). Anything submitted for inclusion is licensed the
same way, with no additional terms.
