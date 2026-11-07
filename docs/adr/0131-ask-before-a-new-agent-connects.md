<!-- @doc
id = "ADR-0131"
title = "The desktop app asks the person before a new agent's first connection"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0131: The desktop app asks the person before a new agent's first connection

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
With `--mcp`, the desktop app reads its control token from a user-only file, and any
MCP client the person has set up reads the same file
([ADR-0006](0006-mcp-full-access.md)). The first time an agent connects, nothing on
screen says so. The maintainer asked for a prompt (#1229): "the first time an agent
connects, JStream should pop up and ask whether to allow it."

This ADR has to fit the posture that is already accepted, and not change it
([ADR-0043](0043-narrow-maintainer-tier.md)). The threat model leaves "another process
of the same user" out of scope, because it can read the token file anyway
([THREAT-MODEL › Who we defend against](../security/threat-model.md#attackers)). Any
gate between such a process and the engine is one that process can step around. It can
write the list of approved clients, or name itself as a client that was approved. So a
prompt can't be a security boundary here, and this ADR doesn't make it one. It is a
notice with a veto: the person sees each new agent once and can turn away one they
didn't expect, such as an MCP client someone else configured or one they forgot about.

<a id="decision"></a>
## Decision
1. **Who may connect is unchanged.** Every listener still requires the control token or
   a device token, and a client with one may still do everything
   ([ADR-0006](0006-mcp-full-access.md), [R-SEC-002](../RULES.md#R-SEC-002)). Approval
   is not a credential. The engine never treats an approved client differently from
   another token holder, and nothing in this ADR is a control in the threat model.
2. **Where it applies.** Only in the desktop app, only with `--mcp`, and only to
   connections that present the token from the token file. The app's own webview
   connects with a second token that it makes at launch and keeps only in memory, so the
   server tells the UI apart without trusting a name. Paired devices, which the person
   admitted by typing a code ([ADR-0044](0044-pairing-remote-browsers.md)), never
   prompt. The daemon and the desktop app without `--mcp` don't prompt either.
3. **The prompt.** When such a connection's `hello` names a client that isn't on the
   approved list, the server holds the connection: no `welcome`, no state, and no
   calls answered. It tells the open clients with a new event, and the desktop UI shows a
   dialog: "**<client name>** wants to control JStream", with how it connects (MCP over
   stdio through `jstream-cli mcp`, MCP over HTTP, or the WebSocket API). The buttons
   are **Allow once**, **Always allow** and **Deny**. Any admitted client may answer, as
   any client may revoke a device today. There are no tiers.
4. **The outcomes.** Allow once sends the `welcome`. Always allow also adds the client's
   name and transport to the approved list. Deny answers the `hello` with a new error,
   `not_allowed` ("the person declined this connection"), which an agent can report,
   and closes the connection. With no answer in 2 minutes, the connection gets the same
   error, saying it timed out. A client that reconnects after a denial prompts again.
5. **The approved list.** It holds a client name and a transport per row, in a user-only
   file beside the token file. It is named by what the client says in `hello`, which
   the client chooses. The Settings dialog's Devices page lists it, and each row can be
   removed (#1207). Removing a row doesn't close an open connection; the next one prompts.
6. **One setting.** "Ask before a new agent connects" is in the Settings dialog's General
   page, on by default, and kept per machine. Off, the app behaves as it does today.
7. **What stays out.** A headless daemon that refuses clients until one is approved from
   the CLI, a per-client credential issued on approval, and any limit on what an
   approved client may do would each change who may connect or what a client is trusted
   to do. Each needs its own maintainer-tier ADR.

<a id="consequences"></a>
## Consequences
- **Easier:** the person sees each new agent and can turn one away. The Agent activity
  panel already names the client behind each change.
- **Harder:** the server gains a held state between `hello` and `welcome`, which needs a
  test for each outcome: allow once, always allow, deny, timeout, a held connection that
  closes, and a UI that disconnects while a prompt is open. The protocol gains the event,
  the answer action, the approved-list query and removal, and `not_allowed`, in one minor
  version.
- **Must do:** the threat model's MCP section says, in the PR that builds this, that the
  prompt is a notice and not a control, so no one later relies on it as one. A client
  can pass by naming itself after an approved client, or by writing the list.
- Tier: agent. This adds a notice to the person and changes neither what any client is
  trusted to do nor who may connect. Section 7 lists the questions that would change
  them, split out as ADR-0043 asks.
