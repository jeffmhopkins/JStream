+++
id = "ADR-0006"
title = "MCP agents get full, authenticated access"
kind = "adr"
status = "accepted"
+++

# ADR-0006: MCP agents get full, authenticated access

<a id="context"></a>
## Context
Agents should be able to do anything a user can, including going live. But "localhost
only" is not a security boundary: any web page can open `ws://127.0.0.1`, and DNS
rebinding defeats Host checks unless they are validated explicitly. The
[MCP transport spec](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
requires servers to validate `Origin`.

<a id="decision"></a>
## Decision
- Every protocol capability is available over MCP with **no permission tiers**.
- **Every listener** (WebSocket API, MCP over HTTP, web UI) **requires the control token**,
  even on localhost, and validates `Origin` and `Host`. Listeners bind to `127.0.0.1` by
  default.
- The token lives in a file only the user can read. The stdio MCP proxy and the desktop
  UI read it from there.
- Every agent action goes to the audit log, and undo is per client
  ([ADR-0013](0013-command-model.md)).

<a id="consequences"></a>
## Consequences
- Agents keep full power, and a web page can't take over JStream behind the user's back.
- Clients need the token, which is one extra setup step for remote browsers (copy the
  token or scan a QR code).
- If multi-user or untrusted agents become a need, revisit with a tiered model.
