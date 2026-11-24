<!-- @doc
id = "ADR-0233"
title = "Announce the network listener on mDNS, behind its own setting"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0233: Announce the network listener on mDNS, behind its own setting

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A device that pairs with JStream across the network, such as the Stream Deck plugin
(ADR-0232), has to be told JStream's address. ADR-0104 made that a typed name or IP
address: the network listener prints its addresses, the person copies one. On a home or
studio network that address changes with DHCP, and the computer's name may not resolve
from the other machine.

Local networks already solve this with multicast DNS and DNS-SD: a service announces
itself under a type such as `_jstream._tcp.local`, and anything on the same link can list
it. The Stream Deck plugin browses for it.

Announcing changes JStream's privacy posture, which is why this is maintainer tier
([R-GOV-005](../RULES.md#R-GOV-005)). With network access on, the listener's port is
already open to the link, so anyone there can find it by trying ports. Announcing it tells
everyone on the link, unasked, that this computer runs JStream, its name, and the port.
It admits no one: pairing still takes the one-time code and the certificate fingerprint
the host shows (ADR-0044, ADR-0104). It is not telemetry
([R-SEC-004](../RULES.md#R-SEC-004)): it goes only to the local link, only after the
person turned network access on, and says only what a device needs to connect.

The maintainer asked for discovery, chose "announce while network is on", and asked for
it to be a setting: "Have Auto announced just have a setting that we can turn on and off
in settings of the GUI of jstream".

<a id="decision"></a>
## Decision
1. **What is announced.** While the network listener is open, the host announces one
   DNS-SD instance, `<host name>._jstream._tcp.local`: a PTR record for the service, an
   SRV record with the listener's port and the host's `.local` name, and a TXT record
   `v=1`. Nothing else: no fingerprint, version, project or device list. No address
   records; a device connects to the address the answer came from.
2. **Only with network access.** It is never announced while "Allow devices on my
   network" (the daemon's `--lan`) is off, so it never says more than the open port
   already would.
3. **Its own setting, on by default.** The desktop app's **Settings › Devices** has "Let
   devices find JStream on my network" under "Allow devices on my network", on unless the
   person turns it off; like that one, it applies at the next start. It is saved in the
   same `network.json` (`"announce": false` turns it off). The daemon announces with
   `--lan` unless started with `--lan-quiet`.
4. **How.** A small responder in `jstream-server` (`announce.rs`) on UDP 5353, shared with
   the system's own responder, joined to `224.0.0.251`. It announces three times at start
   and once a minute (TTL 120 s), answers queries for the service or the instance, and
   says goodbye (TTL 0) when the server stops. It reads a query with bounds on its size,
   questions and labels. If the port can't be opened, it logs and the listener works
   without it, by its address. IPv4 only for now.
5. **Finding is not trusting.** A device that finds JStream still compares the
   fingerprint and pairs with the code. A spoofed answer can at worst point a device at
   another address, where the fingerprint check refuses it.

<a id="consequences"></a>
## Consequences
- A Stream Deck (or any later device) finds JStream with **Find JStream**, without the
  address. Typing it still works, for networks that drop multicast.
- Anyone on the link can see that this computer runs JStream while network access is on.
  The person can turn that off without turning network access off.
- The threat model and the desktop and daemon docs name the announcement and its setting.
- A later IPv6 announcement, or more TXT keys, is a change to this decision.

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. Not a tracked doc. -->
