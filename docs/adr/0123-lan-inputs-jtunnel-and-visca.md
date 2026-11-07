<!-- @doc
id = "ADR-0123"
title = "LAN inputs without TLS: JTunnel senders and VISCA, opt-in and accepted one peer at a time"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0123: LAN inputs without TLS: JTunnel senders and VISCA, opt-in and accepted one peer at a time

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0122](0122-controller-input-midi-and-one-binding-system.md) builds JTunnel in as a
receiver, so a DAW's audio and MIDI reach JStream over the LAN (#1222, #1223), and VISCA
lets JStream drive PTZ heads (#1182) and lets a PTZ controller drive JStream's virtual
cameras (#1181). It split the network question out to this ADR, because what a remote
peer is trusted to do is security posture
([ADR-0043](0043-narrow-maintainer-tier.md)).

None of these protocols can meet JStream's listener rule
([R-SEC-002](../RULES.md#R-SEC-002), [ADR-0104](0104-network-listener-over-tls.md)):
opt-in, TLS only, device tokens only. JTunnel is plain UDP with discovery over mDNS.
VISCA-over-IP is plain UDP or TCP. Neither has authentication or encryption, and a
host on the LAN can spoof either one. The other end is fixed: JTunnel is the DAW's
plugin, and the cameras and controllers are hardware. So JStream can't add TLS.

The maintainer set the shape in #1223: discovery and the receiver listen on the LAN only,
there's a clear on or off setting, and accepting a sender is explicit the first time.

<a id="decision"></a>
## Decision
1. **Off by default, one switch each.** Three host settings, each off until the person
   turns it on: **Receive from JTunnel senders**, **Control PTZ cameras** and **Accept
   a PTZ controller**. They live in the desktop app's settings, and the daemon has a
   flag for each. Each setting is per machine, not in the collection, so opening a
   collection never turns one on. Off means no socket is open for that protocol.
2. **LAN only.** Every socket refuses datagrams and connections from addresses outside
   loopback, the private IPv4 ranges (10/8, 172.16/12, 192.168/16) and link-local
   (169.254/16). It never sends to them either. mDNS uses its link-local multicast
   group (224.0.0.251:5353).
3. **JTunnel: JStream only receives.**
   - It sends three things: mDNS PTR queries for `_jtunnel._udp.local`, and subscribe
     and bye packets to senders. It never announces itself and never sends audio or
     MIDI.
   - Senders it finds are listed in Add source and the Mixer. Listing a sender trusts
     it with nothing.
   - **The first use of a sender is explicit.** The first time the person picks a
     sender by name, they confirm it, and its name and address go on the machine's
     accepted-senders list, where they can remove it. JStream subscribes only to
     accepted senders. Its receive socket drops (and counts) every datagram whose
     source address isn't one it subscribed to.
   - **What a sender can do is bounded.** Its audio feeds the source made from it. Its
     MIDI drives bindings only if the person turns on "Use this sender's MIDI" for it.
     Then it can do what those bindings do, and nothing else: it never reaches the
     protocol directly.
4. **VISCA out (JStream drives PTZ heads).** JStream sends only to camera addresses the
   person adds (IP and port, no discovery in the first cut). It reads replies only from
   that address and port. A reply can only update the pose JStream shows and the
   command's status. The camera list is per machine.
5. **VISCA in (a controller drives virtual cameras).** The listener binds to one
   interface the person picks, never to all of them. It answers only controller IP
   addresses on an allowlist that the person fills. Anything else is dropped and
   counted. Each endpoint drives only the camera actions of the scene camera it's bound
   to (#1181's runtime drive and aim actions, preset recall), never another action.
6. **The risk that stays.** On the LAN, a host that spoofs an accepted sender's or
   controller's address can inject audio, MIDI or camera moves within these bounds. That
   is how JTunnel and VISCA work everywhere. Off by default, LAN only, per-peer
   acceptance and bounded actions keep it to people on the same network who already
   target this machine. The control listener, and what a device token may do, are
   unchanged.
7. **Rule change.** On acceptance, R-SEC-002 also says: the JTunnel and VISCA sockets
   are not control listeners. They follow ADR-0123: each is opt-in, LAN only and limited
   to accepted peers, and runs only bindings or camera actions, never the protocol.
   Nothing that opens these sockets is built before then.

<a id="consequences"></a>
## Consequences
- **Easier:** a DAW over JTunnel and standard PTZ gear work without a bridge, with
  nothing to pair.
- **Harder:** JStream carries two network paths outside the TLS listener's model. Each
  one needs its source-address filter, LAN-range check and counters tested, and listed
  in `profile.get`.
- **Must do:** the settings and the accepted-senders, camera and controller lists, each
  in the PR that opens its socket, and the R-SEC-002 text once this is accepted.
