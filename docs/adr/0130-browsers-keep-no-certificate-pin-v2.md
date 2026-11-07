<!-- @doc
id = "ADR-0130"
title = "A paired browser keeps no certificate pin, so a new warning on its host means checking the fingerprint again"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0130: A paired browser keeps no certificate pin, so a new warning on its host means checking the fingerprint again

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0104](0104-network-listener-over-tls.md) decision 5 has the person check the network
listener's certificate fingerprint once. A native client then pins it, and a browser
accepts its warning. [ADR-0114](0114-browsers-keep-no-certificate-pin.md) narrowed
ADR-0104's "after it, the pin holds" to native clients, because a browser keeps no pin:
it keeps an exception for that one certificate. If another certificate shows up for the
same host name later, the browser warns again. A person who clicks through then sends the
`__Host-jstream_device` cookie ([ADR-0047](0047-browser-pairing-page-and-device-cookie.md))
to whoever offered it.

ADR-0114 decision 2 had the host and the `/pair` page say that such a warning is expected
only right after a renewal on the host, and otherwise means "don't continue". The review
of that ADR (#1161, N1, carried in #1129) found the premise wrong: browsers also drop
certificate exceptions with no renewal at all. Clearing site data, a private window, a
browser update, or the exception simply expiring each bring the warning back for the same
certificate. A flat "stop" would then be wrong most of the times the person sees it, and
it would train them to ignore the advice.

<a id="decision"></a>
## Decision
This supersedes ADR-0114 and keeps its decisions 1 and 3. Decision 2 changes as follows.

1. **The claim stays narrowed.** After the first check, the pin holds for native clients.
   In a browser, what protects the person afterwards is the certificate warning and how
   they respond to it (ADR-0114 decision 1).
2. **A new warning on a paired host means: check the fingerprint again.** Where the host
   shows the fingerprint, and on the `/pair` page beside its code field, one sentence says
   so: a new certificate warning for this host means continue only if the fingerprint
   matches the one the host shows. The person compares the fingerprint the browser's
   warning offers with the host's, as ADR-0104 decision 5 has them do the first time.
3. **The threat model follows the build.** The PR that builds the network listener states
   this in the threat model's server section, beside ADR-0104's other controls
   (ADR-0114 decision 3).

<a id="consequences"></a>
## Consequences
- ADR-0114 is superseded; ADR-0104 keeps its text and status
  ([R-DOC-009](../RULES.md#R-DOC-009)). A reader of ADR-0104's Consequences should take the
  browser case from this ADR.
- The residual risk is ADR-0114's: a paired browser whose user continues past a warning
  for a certificate that isn't the host's hands that device's token to the impostor.
  Revoking the device (`device.revoke`) ends it, as for a lost device. The sentence now
  asks for the same check the person made when pairing, instead of a rule that is wrong
  whenever the browser drops its exception by itself.
- The host's fingerprint display and the `/pair` page carry the sentence. No protocol or
  rule change follows.
- Tier: agent, as ADR-0114 was. This corrects the wording of guidance to match the accepted
  design. The trust check is still ADR-0104 decision 5's fingerprint comparison, nothing
  a client may do changes, and no control is added or removed
  ([R-GOV-005](../RULES.md#R-GOV-005), [ADR-0043](0043-narrow-maintainer-tier.md)). Anything
  that changes the posture itself, such as revoking browser devices when the certificate is
  renewed, is outside it and would be the maintainer's.
