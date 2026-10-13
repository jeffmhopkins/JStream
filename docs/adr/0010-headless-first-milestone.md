# 0010: Build headless engine and MCP before the GUI

- Status: Accepted
- Date: 2026-10-13

## Context
The GUI is the most change-prone layer. Agents can exercise the engine through MCP
from day one, and snapshots let them verify visual output.

## Decision
Milestone 1 delivers the engine, protocol, MCP server and CLI, including virtual camera
and YouTube streaming, with no GUI. The UI is M2 and is built on the proven API.

## Consequences
- The API is validated by real use before the UI depends on it.
- Early demos are agent/CLI-driven rather than visual.
