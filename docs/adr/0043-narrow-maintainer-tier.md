<!-- @doc
id = "ADR-0043"
title = "Decision tiers v2: the maintainer decides product-level calls only"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0043: Decision tiers v2: the maintainer decides product-level calls only

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0017](0017-adr-decision-tiers.md) sends every product, platform, process and
governance decision to the maintainer, and every ADR that supersedes another. In practice
that sent him decisions agents should have made: ADR-0040 (what happens to hotkey targets
and overrides on structural edits), ADR-0041 (YouTube single sign-in scope) and ADR-0042
(narrowing doc-section `covers`). Each waited on him, and none needed his judgment. The
maintainer wants to decide product-level calls and nothing else.

<a id="decision"></a>
## Decision
This ADR supersedes ADR-0017. Every ADR still declares a `tier` in its `@doc` block.

- **`tier = "maintainer"`**: a decision about
  - **product scope**: which features are in or out, and which platforms are supported;
  - **licensing**: the project's license, and taking a dependency or asset whose license
    the project doesn't already accept;
  - **security and privacy posture**: what data leaves the user's machine, what the app
    may access, and what a remote client or plugin is trusted to do;
  - **public compatibility promises**: what the project promises users and third parties
    about the protocol, MCP tools, file formats or plugin interfaces staying compatible;
  - **spending**: anything that costs money, such as paid services, certificates or
    hardware;
  - **this tier list and the decision-comment rules below**, so that agents can't widen
    their own authority.

  It opens as `status = "proposed"` and the PR gets the `needs-human` label. The
  maintainer decides with a decision comment, and the builder sets `status` to match.
- **`tier = "agent"`**: everything else. That includes architecture, internal design,
  crates, the review process, the docs system, CI and rule changes, and amending or
  superseding any ADR whose subject is not on the list above, whatever tier that ADR
  carried. It lands as `accepted` on merge, after the usual review. Changes that weaken a
  check still need the `governance-approved` label and its justification
  ([R-GOV-002](../RULES.md#R-GOV-002)).
- **Classification.** The tier follows what the ADR decides, not what it touches. The
  builder picks it and the reviewer checks it. Too low a tier is blocking; too high a
  tier is a non-blocking finding, and the builder relabels it. When an agent-tier ADR
  contains one maintainer-level question, split that question out rather than escalating
  the whole ADR.
- **Decision comments** keep ADR-0017's two forms unchanged:
  - **Direct:** the maintainer comments `/accept ADR-NNNN` or `/reject ADR-NNNN`.
  - **Proxy:** an agent relays the maintainer's explicit instruction, with this
    attribution on its own lines:
    ```
    /accept ADR-NNNN
    on-behalf-of: @<maintainer>
    instruction: "<the maintainer's words, quoted>"
    proxy-session: <the relaying agent's session id>
    ```
  The instruction must name the specific ADR. Standing or blanket instructions are not
  decisions. An agent never posts a decision the maintainer did not give.
- **Existing ADRs** keep their recorded `tier`; it is not edited
  ([R-DOC-009](../RULES.md#R-DOC-009)). It only records who decided them.

<a id="consequences"></a>
## Consequences
- The maintainer sees scope, license, security and privacy, compatibility promise and
  spending decisions before they bind, and nothing else.
- Agents can revise architecture and process on their own, including decisions the
  maintainer once made. The review gate is the check on those, and the weekly audit
  still lists every maintainer-tier decision.
- Fewer PRs wait on `needs-human`.
