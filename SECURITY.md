<!-- @doc
id = "SECURITY"
title = "Security policy"
kind = "normative"
status = "active"
review_every = "90d"
-->

# Security policy

<a id="supported"></a>
## Supported versions
<!-- @section
-->

JStream has no release yet. Until the first release, only the `main` branch is
supported, and fixes land there. This section will say which releases get security
fixes once there are releases.

<a id="reporting"></a>
## Reporting a vulnerability
<!-- @section
-->

**Do not open a public issue.** Report privately through GitHub's private vulnerability
reporting: [open a draft security advisory](https://github.com/jeffmhopkins/JStream/security/advisories/new)
(the repository's **Security** tab, then **Report a vulnerability**).

Please include:

- what an attacker can do, and what they need first (network access, a local account,
  a malicious plugin or source);
- the steps to reproduce it, and the commit or version you tested;
- any logs, with stream keys, OAuth tokens and the control token removed.

JStream has one maintainer, so handling is best effort. The aim is to acknowledge a
report within a week, agree a fix and a disclosure date with the reporter, and publish
a GitHub security advisory once the fix is on `main`. Reporters are credited in the
advisory unless they ask not to be.

<a id="scope"></a>
## What JStream promises
<!-- @section
depends_on = ["docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-002", "docs/RULES.md#R-SEC-003", "docs/RULES.md#R-SEC-004"]
verified = { hash = "b3:ff6ba893ff824b19", by = "agent:claude@session_01WbL3WRbcjCtYjwjoicNdxV", note = "Merge of the updated pairing branch (main e1e2a94, pairing now protocol 0.19) into the ADR-0047 build: stamp conflicts only plus gate.rs/lib.rs using jstream_control's WS_PROTOCOL/WS_TOKEN_PROTOCOL_PREFIX; re-read this PR's pairing-page, cookie, pairing-switch and panel text beside the merged text; no version mention here; holds" }
-->

A report is in scope when it breaks one of these. The rules they cite are the authority.

- **Secrets:** stream keys and OAuth tokens stay in the OS credential store and are
  redacted everywhere else ([R-SEC-001](docs/RULES.md#R-SEC-001)).
- **The control API:** every connection needs the control token or a paired device's
  token (the pairing exchange aside, which needs an open one-time code, and the fixed
  pairing page on a TLS listener; a browser may hold a device token as a cookie, never the
  control token), a device token is accepted only over loopback or TLS, and the listener
  binds to localhost by default and checks `Host`, and `Origin` when one is present
  ([R-SEC-002](docs/RULES.md#R-SEC-002)).
- **WASM plugins** get no filesystem or network access unless the manifest declares it
  and the user grants it ([R-SEC-003](docs/RULES.md#R-SEC-003)).
- **No telemetry:** nothing leaves the machine without an explicit user action
  ([R-SEC-004](docs/RULES.md#R-SEC-004)).
- **Untrusted input:** media streams, files, network peers and chat text must not crash
  the engine, corrupt memory or run code.

Out of scope:

- **Anyone who already has the user's account.** The control token gives full control
  by design, so whoever can read it can do anything JStream can.
- **CLAP plugins.** They are native code the user chose to install. JStream isolates
  their crashes, not their behaviour (R-SEC-003).
- **Browser sources** running pages the user chose. The bounds JStream itself sets on
  them are in scope.

Dependencies are checked against the RustSec advisory database in CI (`cargo deny`), so
a known advisory in a dependency is usually fixed without a report.
