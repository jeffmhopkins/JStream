<!-- @doc
id = "ADR-0235"
title = "Pair a device by approving it on the host"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0235: Pair a device by approving it on the host

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A device on the network pairs with a one-time code (ADR-0044). The person starts pairing
on the host, reads the 8-character code there, compares the certificate fingerprint the
device shows with the one the host shows (ADR-0104), and types the code on the device.
For a Stream Deck (ADR-0232), whose settings panel sits on another computer, that is two
screens and two long strings for one key press. The maintainer asked for less: "I don't
like the pairing process ... we should just go in there and say allow pairing in jstream
and then at that point it should auto connect". Offered a window in which any device
pairs on its own, or a prompt on the host for each device that asks, they chose the
prompt ("Approve on JStream's screen").

This changes who can get in, so it is maintainer tier
([R-GOV-005](../RULES.md#R-GOV-005)).

<a id="decision"></a>
## Decision
1. **A device can ask instead of typing a code.** A native client posts its name to
   `POST /api/v1/pair/ask` (`PAIR_ASK_PATH`), answered without a token like
   `POST /api/v1/pair`, after the same `Host` and `Origin` checks, and only while the
   server pairs devices (ADR-0234). The request is held until the person answers, for at
   most 2 minutes. At most 4 asks are open at once, and one per address: another is
   turned away at once (`429`).
2. **The host asks the person.** Every admitted session hears `pairing.asked`, with the
   ask's ID, the device's name (its own text, never an instruction) and the address it
   asked from. The host's UI shows "<name> at <address> wants to connect", with Allow and
   Deny. Any admitted session may answer, with `pairing.answer` (as any may start
   pairing, ADR-0044). Every session hears `pairing.ask_ended` when the ask is
   answered, times out or the device goes away, and closes its prompt.
3. **Allowed, it is paired as with a code.** The held request is answered `200` with a
   new device token (`{"device_id", "token"}`), which the host keeps as a digest, lists
   under Paired devices and can revoke, as ADR-0044 says. Denied is `403`, timed out
   `408`.
4. **Trust on first use.** With no code, the device doesn't compare fingerprints: it
   keeps the certificate it saw when it asked. Someone who can intercept the device's
   traffic on the local network at that moment could relay the ask and be the one the
   device trusts. The prompt's address lets the person tell their own device's ask
   apart, and the person who doesn't want the risk pairs with the code, which stays.
5. **Nothing else changes.** Browsers still pair with the code (ADR-0047); the code
   path, the tokens, revocation and the network listener's rules (ADR-0104) are as
   they are. The ask, its name and its address are never logged beyond `info`, and no
   token is ever logged (R-SEC-001).

<a id="consequences"></a>
## Consequences
- Pairing a Stream Deck is: press Connect on the deck, press Allow on JStream.
- The protocol gains `pairing.asked`, `pairing.ask_ended` and `pairing.answer`.
- A person who presses Allow on an ask they didn't expect pairs a stranger's device; the
  prompt says so plainly, and Paired devices lists it for revoking.
- The threat model names the ask, its limits and trust on first use.

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. Not a tracked doc. -->
