# 0001: Library-first engine with thin hosts

- Status: Accepted
- Date: 2026-10-13

## Context
JStream must run as a desktop app, headless (daemon), and be controlled by a web UI,
a CLI and AI agents over MCP.

## Decision
`jstream-core` is a library exposing an `Engine`. All control goes through the typed
`jstream-protocol` command/event API, accessed through the `Control` trait
(`jstream-client`) either in-process or over WebSocket. Hosts (`apps/*`) are thin.

## Consequences
- Headless operation and remote control come from the same code.
- MCP, UI and CLI cannot drift: they share one API.
- The protocol must be designed carefully and versioned (RULES §7).
