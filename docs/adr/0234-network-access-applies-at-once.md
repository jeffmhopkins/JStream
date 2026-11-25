<!-- @doc
id = "ADR-0234"
title = "Network access applies at once"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0234: Network access applies at once

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The desktop app's "Allow devices on my network" (ADR-0104 decision 1) and "Let devices
find JStream on my network" (ADR-0233 decision 3) were saved and then applied at the
app's next start, because the server bound the network listener once, before serving.
The maintainer found that a restart to turn it on or off was wrong: "\"allow devices on
my network\" requires a reset... this should be dynamic".

Neither setting changes who can get in. Opening the listener at once opens it to the
same rules as at start (TLS only, device tokens only, pairing with the code and the
fingerprint). Closing it at once is stricter than before, when a listener the person
turned off stayed open until the app restarted. So this is a change to when the
settings apply, not to the security posture, and is agent tier
([R-GOV-005](../RULES.md#R-GOV-005)).

<a id="decision"></a>
## Decision
1. **The server opens and closes the network listener while it runs.** A
   `NetworkSwitch` from the server opens it with a `NetworkConfig` and closes it.
   `ServerConfig::network` still opens one at start, through the same switch. The
   gate's rules for the network side, and what `pairing.start` says about it, are read
   per request, so they follow the switch.
2. **Closing stops what it opened.** Closing stops accepting connections, says goodbye
   on mDNS (ADR-0233), and ends every WebSocket session admitted through the listener,
   with 1001 as at shutdown. Devices stay paired, and are admitted again once it opens.
3. **The server pairs while the listener is open.** It pairs devices when
   `ServerConfig::pairing` is on or the listener is open. The desktop app turns
   `pairing` off and keeps its paired-devices file always, so it pairs exactly while
   network access is on.
4. **The desktop settings apply at once.** "Allow devices on my network" saves the
   setting, then opens or closes the listener. When it can't open (the port is taken),
   the page says why, and the setting stays saved for the next start. "Let devices
   find JStream on my network" starts or stops the announcement at once. This amends
   ADR-0233 decision 3's "it applies at the next start". The daemon's `--lan` is
   unchanged: a flag applies when the daemon starts.

<a id="consequences"></a>
## Consequences
- Turning network access off ends the devices' sessions at once. A Stream Deck shows
  itself as disconnected and reconnects when it is turned on again.
- At start, a listener that can't open no longer fails the server's start: the desktop
  app starts without it and says why, as it did before by retrying without network
  access.
- The desktop, server and UI docs, the threat model, and the Stream Deck plugin's hint
  drop "applies after a restart".

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. Not a tracked doc. -->
