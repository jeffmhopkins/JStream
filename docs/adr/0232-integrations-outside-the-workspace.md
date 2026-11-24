<!-- @doc
id = "ADR-0232"
title = "Integrations for other apps live in integrations/, outside the Cargo workspace"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0232: Integrations for other apps live in integrations/, outside the Cargo workspace

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A control surface such as a Stream Deck was planned from the start: ADR-0104 lets one
pair with JStream across the network, and ADR-0112 names one as a way to drive actions.
A Stream Deck plugin can't be a crate. The Stream Deck app runs plugins on its own
Node.js runtime, through Elgato's SDK, and installs them from a `.streamDeckPlugin`
package. So it is TypeScript with npm dependencies, built and tested outside Cargo.

The repository has no place for such code. `crates/` and `apps/` are Cargo members, under
the crate layering, the workspace's dependency rules and `cargo deny`. The only npm
package so far is the web UI's end-to-end tests, which ship nothing.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes where code lives
and what builds it, not product scope, licensing, security posture, a compatibility
promise or spending. The plugin adds nothing to the protocol and uses pairing as ADR-0044
and ADR-0104 already allow.

<a id="decision"></a>
## Decision
1. **Where.** Code that runs inside another app and drives JStream lives in
   `integrations/<name>/`, one folder per integration. The first is
   `integrations/streamdeck/`.
2. **Clients only.** An integration speaks JStream's public protocol and nothing else,
   as any other client does ([R-ARCH-003](../RULES.md#R-ARCH-003)). A capability it needs
   is added to the protocol first ([R-ARCH-001](../RULES.md#R-ARCH-001)), so MCP agents,
   the CLI and the UI get it too. An integration never holds the control token: across
   the network it pairs as a device.
3. **Its own toolchain.** An integration is not a Cargo member. It builds with its host
   app's toolchain and pins every dependency exactly, with a lockfile. Its dependencies
   must have licences on `deny.toml`'s allow list, which the PR checks by hand
   ([R-DEP-001](../RULES.md#R-DEP-001), [R-DEP-004](../RULES.md#R-DEP-004)).
4. **Documented like a crate.** Each integration has a README that is a tracked doc, with
   sections whose `covers` bind its code. It says how to install, build and test it.
5. **Tested against the real thing.** An integration's tests may start a built
   `jstream-daemon` to run against. They skip when none is given, so the integration
   builds without Rust. CI doesn't run them yet. Adding a job is a separate change.
6. **Product agnostic.** An integration controls JStream's general features (scenes,
   sources, shows, outputs and so on). It carries nothing specific to one show or scene.

<a id="consequences"></a>
## Consequences
- **Easier:** a Stream Deck plugin, and later others such as Companion modules or Loupedeck
  plugins, have one obvious place, beside the protocol they speak, and change with it in
  the same PR.
- **Harder:** npm dependencies are outside `cargo deny` and Dependabot's Cargo group.
  Reviewers check licences and pins by hand, until a later change adds them to
  Dependabot and CI.
- **Must do:** an integration's README lists what its tests ran and what they didn't, and
  a protocol change that breaks a call an integration makes updates the integration in
  the same PR.
