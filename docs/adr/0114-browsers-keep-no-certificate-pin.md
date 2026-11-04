<!-- @doc
id = "ADR-0114"
title = "A paired browser keeps no certificate pin, so a new warning on its host means stop"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0114: A paired browser keeps no certificate pin, so a new warning on its host means stop

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0104](0104-network-listener-over-tls.md) decision 5 has the person check the network
listener's certificate fingerprint once. A native client then pins it, and a browser
accepts its warning. Its Consequences then say, for every client: "A person who skips it on
first pairing can be fooled by an attacker already in the middle of that one connection;
after it, the pin holds."

That holds for native clients only. A browser keeps no pin. Accepting the warning stores an
exception for that one certificate, nothing more. When a later connection to the same host
name offers another certificate, the browser warns again, and a person who clicks through
sends the `__Host-jstream_device` cookie
([ADR-0047](0047-browser-pairing-page-and-device-cookie.md)) to whoever offered it. The
cookie is `Secure`, which binds it to HTTPS, not to one certificate. Whoever receives it
holds that device's token. #1090's review raised this (N4, #1129).

<a id="decision"></a>
## Decision
This clarifies ADR-0104's Consequences sentence above. It does not change what any client
is trusted to do, or how a certificate is made, shown, pinned or renewed.

1. **The claim is narrowed.** After the first check, the pin holds for native clients. A
   browser's protection afterwards is its certificate warning, and the person's response
   to it.
2. **A new warning on a paired host means stop.** Where the host shows the fingerprint, it
   also says so in one sentence: once a browser is paired, a certificate warning for this
   host is expected only right after the certificate was renewed on the host itself, and
   otherwise means "don't continue". The `/pair` page says the same next to its code field.
3. **The threat model follows the build.** The PR that builds the network listener
   states this in the threat model's server section, beside ADR-0104's other controls.

<a id="consequences"></a>
## Consequences
- ADR-0104 keeps its text and status ([R-DOC-009](../RULES.md#R-DOC-009)). A reader of its
  Consequences should take the browser case from this ADR.
- The residual risk is stated: a paired browser whose user clicks through a later warning
  hands that device's token to the impostor. Revoking the device (`device.revoke`) ends
  it, as for a lost device.
- The UI and the pairing page gain one sentence each when they are built. No protocol,
  rule or code change follows now.
- Tier: agent. This corrects a claim to match the accepted design and adds wording to
  pages not yet built, as [ADR-0110](0110-mcp-values-tool-naming.md) did for ADR-0102.
  Anything that changes the posture itself, such as revoking browser devices when the
  certificate is renewed, is outside it and would be the maintainer's.
