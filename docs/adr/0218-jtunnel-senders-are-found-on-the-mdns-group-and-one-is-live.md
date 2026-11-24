<!-- @doc
id = "ADR-0218"
title = "JTunnel senders are found on the mDNS group, one accepted sender is live, and it is also the input JTunnel"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0218: JTunnel senders are found on the mDNS group, one accepted sender is live, and it is also the input JTunnel

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) §3 has the JTunnel receiver find senders
over mDNS and list them for the person to accept. The receiver asked with a legacy
unicast query (RFC 6762 §6.7) from its own socket, expecting each sender to answer that
socket. JTunnel doesn't: its responder answers every query, and announces every 10 s, to
mDNS's group only. So no sender was ever listed, and the person had to type the sender's
tunnel name and its address and port, which change when the DAW picks another port.

The maintainer then asked for three things beside working discovery. Several senders may
be accepted, but only one is received at a time. Accepting a discovered sender should be
one click after the confirmation ADR-0123 requires. And because only one sender is live,
what a collection binds to should not depend on the sender's tunnel name (a show built
against a sender called `Bitwig` broke against one called anything else).

<a id="decision"></a>
## Decision
1. **Discovery listens on the group.** While the receiver runs with discovery on, a
   second socket, the mDNS listener, binds port 5353 on every IPv4 interface with
   address (and, where it exists, port) reuse, so it shares the port with the system's
   own responder as every mDNS program does, and joins 224.0.0.251. It only reads: nothing
   is ever sent from it, so ADR-0123 §3's "JStream only receives" and its three kinds of
   packet sent hold. What it takes is exactly what the receive socket took from an
   answer: an mDNS message from a LAN address's port 5353, read by the same bounded
   parser into the same bounded list, which trusts a listed sender with nothing.
   Anything else on the group (queries, traffic from outside the LAN ranges, from another
   port) is dropped uncounted, since it is other programs' ordinary traffic; at most 16
   datagrams are read per turn of the thread, so a busy LAN can't hold up playout. If the
   listener can't be opened, the receiver runs without it, as before. The query is still
   sent, which also prompts every sender to announce at once.
2. **One live sender (desktop app).** The app's setting keeps any number of accepted
   senders and names one `live`, by default the first. The engine is started with the
   live sender alone; the others stand by. Making another live, adding one, or accepting
   one heard on the network makes it live in place of the one before, which stays
   accepted. A sender heard by the name of an accepted one at another address (its DAW
   restarted on another port) is offered the same way, and accepting moves it. Every
   first use is still confirmed by name and address (ADR-0123 §3). The daemon's
   `--jtunnel-accept` list is unchanged: it receives from each.
3. **The live sender is also the input `JTunnel`.** The engine gives the first accepted
   sender (in the app, the live one) a second name, `JTunnel`, unless an accepted sender
   is itself called that. Its MIDI feeds both names' states, its state messages are put
   in the live data under both, a `network_audio` source may name it either way (and
   names `JTunnel` by default), and param names read under either name reach it
   ([ADR-0181](0181-jstream-names-a-jtunnel-senders-params.md)). A binding, source or
   grant that names `JTunnel` follows whichever sender is live; one that names a sender
   keeps working. The app's Map dialog and Learn show the live sender once, as
   `JTunnel`, so what they bind follows the live sender. No protocol type changes:
   `JTunnel` is an input name like any other.

<a id="consequences"></a>
## Consequences
- **Easier:** a sender on the LAN shows up and is accepted with one confirmation; a DAW
  that moved port is followed with one; a collection works against whichever sender is
  live.
- **Harder:** the receiver holds a second socket on a well-known port while it runs. Its
  input is the same parser and bounds as before, and the threat model's LAN inputs
  section says so. The live sender's state is held twice in the live data (each copy
  within its per-sender bound).
- **Must do:** the threat model, the core, desktop and UI READMEs, and a regression test
  that an announcement on the group is listed.
