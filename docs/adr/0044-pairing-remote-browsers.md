<!-- @doc
id = "ADR-0044"
title = "Pairing remote browsers with a one-time code"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0044: Pairing remote browsers with a one-time code

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0006](0006-mcp-full-access.md) makes every listener require the control token, and
the web UI on the same machine gets in with it: the browser asks for it as a password
([jstream-server › Design](../../crates/jstream-server/README.md#design)). A browser on
another device (a tablet beside the stream, a laptop on the LAN) has no easy way to get
the token: it lives in a user-only file on the host, and copying 64 hex characters by
hand is error-prone, while pasting it through a chat or a note spreads it. The roadmap
plans "token pairing for remote browsers" for M2
([ROADMAP › M2](../../ROADMAP.md#m2)).

Pairing changes what a remote client is trusted to do: something other than the token
file admits a client, and one endpoint answers without the token. That is security
posture, so it is the maintainer's call
([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
- **Who starts it.** Pairing is off until an authenticated client starts it with a new
  protocol action, `pairing.start` (from the host's own UI, or `jstream-cli pair`). Its
  answer, to that client only, is a **pairing code**: 8 characters from Crockford's
  base32 alphabet (40 bits, shown as `XXXX-XXXX`). One code is open at a time; a new
  `pairing.start` replaces it. Like every action, it is open to any client with the
  token, MCP agents included, since a token holder could hand out the token anyway
  (ADR-0006: no permission tiers).
- **The exchange.** The remote browser opens the UI, which asks for the code and posts
  it with a device name to `POST /api/v1/pair`. This is the one request a listener
  answers without a token, and it still passes the `Host` and `Origin` checks. A code
  is single-use, expires 2 minutes after it was made, and is void after 5 wrong
  attempts on it. A right code is answered with a new **device token**.
- **Device tokens, not the control token.** Each paired device gets its own 256-bit
  token, which the host keeps only as a BLAKE3 digest, in a user-only file beside the
  token file. A device token grants exactly what the control token grants (no tiers), is
  named in the audit log as `device:<name>`, and can be listed and revoked by any
  authenticated client (`device.list`, `device.revoke`); revoking one closes its open
  connections. One unused for 30 days is dropped. The control token is never sent to a
  device, so revoking a lost tablet doesn't break every other client.
- **Never logged.** Pairing codes and device tokens never appear in logs, events, the
  audit log, snapshots or MCP results other than `pairing.start`'s own answer
  ([R-SEC-001](../RULES.md#R-SEC-001)). The host shows the code on its own screen.
- **Only over TLS beyond loopback.** A device token travels only over loopback or TLS
  (`https://`, `wss://`), so pairing is offered on a non-loopback address only once the
  server has a TLS listener (its open question 1). Listening beyond loopback stays the
  user's explicit choice ([R-SEC-002](../RULES.md#R-SEC-002)).
- **Rule change.** On acceptance, R-SEC-002 reads: every listener requires the control
  token or a device token, except `POST /api/v1/pair` while a pairing code is open, and
  this ADR amends ADR-0006's "every listener requires the control token" the same way.
  Nothing is built before then.

<a id="consequences"></a>
## Consequences
- A remote browser pairs by typing a short code shown on the host, instead of copying
  the token file.
- One endpoint answers without the token. A 40-bit code that lives 2 minutes and allows
  5 guesses gives an attacker on the network about a 1 in 200 billion chance per code.
- The host keeps a second secret file (the device digests), and the protocol gains
  `pairing.start`, `device.list` and `device.revoke` in a minor version.
- Remote use waits on TLS in `jstream-server`, with a certificate the browser will warn
  about unless the user installs it; how that certificate is made and trusted is a
  separate decision.
