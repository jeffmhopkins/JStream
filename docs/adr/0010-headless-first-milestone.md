<!-- @doc
id = "ADR-0010"
title = "Build the headless engine and MCP before the GUI"
kind = "adr"
status = "accepted"
-->

# ADR-0010: Build the headless engine and MCP before the GUI

<a id="context"></a>
## Context
The GUI is the layer that changes most. Agents can exercise the engine through MCP from day
one, and snapshots let them check the visual output.

<a id="decision"></a>
## Decision
M1 delivers the engine, protocol, MCP server and CLI with no GUI, as a thin end-to-end
slice first (capture → encode → YouTube). The UI is M2, built on the proven API.
Technical risks are retired by spikes in M0, before M1 depends on them.

<a id="consequences"></a>
## Consequences
- The API is validated by real use before the UI depends on it.
- Early demos are driven by agents and the CLI rather than shown visually.
