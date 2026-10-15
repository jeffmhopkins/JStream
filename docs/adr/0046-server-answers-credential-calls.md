<!-- @doc
id = "ADR-0046"
title = "The server answers the credential calls, and actions may answer with data"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0046: The server answers the credential calls, and actions may answer with data

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0044](0044-pairing-remote-browsers.md) adds `pairing.start`, `device.list` and
`device.revoke` to the protocol. They concern whom a listener admits, which is
`jstream-server`'s state (the control token, the paired devices, the open code), not the
engine's: the engine sits behind any number of listeners, or none (the desktop app's
in-process session, a test's mock), and never sees a token. Two questions follow that
ADR-0044 leaves open, both architecture, so agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)):

1. Who answers these calls, while keeping them reachable by every client, MCP agents
   included (ADR-0044: no permission tiers)?
2. `pairing.start` must hand its caller a code, and only its caller, so the code can't
   travel as an event. But an action's answer has so far said only that it was accepted
   ([jstream-protocol › Design](../../crates/jstream-protocol/README.md#design)).

<a id="decision"></a>
## Decision
- **The server answers them, by wrapping each session.** `jstream-server` hands every
  WebSocket connection and every MCP session the connector's `Control` session wrapped
  in its own `Control` implementation, which answers `pairing.start`, `device.list` and
  `device.revoke` from the server's device store and passes every other call through.
  The wrapper also renames a device's `hello` to `device:<name>`, so the engine's audit
  log names it as ADR-0044 asks without the engine knowing about devices. MCP agents get
  the calls as the generated `pairing` and `device` tools with no MCP change, and the
  stdio proxy (`jstream-cli mcp`) through the WebSocket.
- **Everyone else answers `unsupported`.** A `Control` with no server in front (the
  engine's in-process session, `MockControl`) answers these calls with a new error code,
  `unsupported` (`call`): valid, but not served here.
- **An action may answer with data.** `CallResult::Action` gains an optional `result`, an
  `ActionResult` tagged by the action's name, like `QueryResult`; `pairing.start`'s is
  the code and its lifetime. Every other action omits it, so their wire form is
  unchanged. Classification is unchanged too: `pairing.start` and `device.revoke` are
  actions (side effects, never undone or batched), `device.list` is a query. All of it
  ships as protocol 0.18, an additive minor ([R-API-001](../RULES.md#R-API-001)).

<a id="consequences"></a>
## Consequences
- The engine and the protocol crate know nothing of tokens; the device store, its file
  and the revocation of connections stay in `jstream-server`.
- Pairing works on every listener the same way, from the UI, the CLI (`jstream-cli
  pair`) and MCP, and a client talking to a bare engine learns why it doesn't.
- The server's own calls are not in the engine's audit log, which records what the
  engine executes; the server logs them (without the code or any token).
- A later action that needs to answer with data adds an `ActionResult` variant rather
  than an event or a query.
