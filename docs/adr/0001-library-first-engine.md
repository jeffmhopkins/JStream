<!-- @doc
id = "ADR-0001"
title = "Library-first engine with thin hosts"
kind = "adr"
status = "accepted"
-->

# ADR-0001: Library-first engine with thin hosts

<a id="context"></a>
## Context
JStream must run as a desktop app and headless, and be controlled by a web UI, a CLI and
AI agents over MCP.

<a id="decision"></a>
## Decision
`jstream-core` is a library exposing an `Engine`. All control goes through the typed
`jstream-protocol` API via the `Control` trait (`jstream-control`), either in-process or over
an authenticated WebSocket. The interfaces (server, MCP, CLI, UI) depend only on
`jstream-control`. The host apps wire them to the engine.

<a id="consequences"></a>
## Consequences
- Headless operation and remote control come from the same code.
- MCP, UI and CLI can't drift apart, because they share one API.
- Remote-only clients (CLI, stdio MCP proxy) don't compile the engine.
- The protocol must be designed and versioned carefully ([R-API-001](../RULES.md#R-API-001)).
