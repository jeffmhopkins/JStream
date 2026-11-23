<!-- @doc
id = "ADR-0181"
title = "JStream names a JTunnel sender's modulation params"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0181: JStream names a JTunnel sender's modulation params

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
JTunnel 0.15 ([Jclap](https://github.com/jeffmhopkins/Jclap) PR #329) gives a sender 32
modulation params, Mod 1 to Mod 32: plain 0 to 1 values that a DAW can automate and
modulate. The sender streams them as its `params` state key, with `values` (32 numbers)
and `names`. A `data` motion modifier ([ADR-0171](0171-live-data-from-a-daw-drives-scenes.md))
or value binding ([ADR-0178](0178-value-bindings-drive-any-number.md)) reads Mod 5 as key
`params`, field `values.4`, so these params can drive cameras, items and settings with no
new engine code.

The maintainer wants the DAW to show what each one drives. When a camera's pan reads
Mod 5, the DAW should call that param "Camera 1 Pan". CLAP lets a plugin rename its params
while it runs: it changes their info and calls the host's `rescan` with
`CLAP_PARAM_RESCAN_INFO`, which takes effect at once. JTunnel 0.15 takes the names in a new
**names packet** (type 5) from a receiver subscribed to it: up to 40 bytes of UTF-8 for
each slot, as one whole list.

[ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) decision 3 says JStream only receives
from JTunnel. It sends mDNS queries and subscribe and bye packets, and nothing else.
Sending names is a fourth thing, and it carries words from the collection to the LAN.
That makes it a change of security and privacy posture, so this ADR is maintainer tier
([R-GOV-005](../RULES.md#R-GOV-005)). The maintainer signed off in the project thread, in
his words: "I give sign off for the reverse. Sending from jstream to jclap." His
`/accept` on this ADR records the decision.

<a id="decision"></a>
## Decision
1. **A fourth packet, to accepted senders only.** JStream's JTunnel receiver may also send
   a names packet. It goes only to a sender on the accepted list (ADR-0123 §3), at the
   address the person accepted it at, which ADR-0123 §2 keeps on the LAN. It goes only to
   a sender whose "Use this sender's state" is on (ADR-0171 decision 1), since the names
   are for the `params` key that switch lets JStream read. Off, nothing goes. JStream
   still never announces itself, never sends audio or MIDI, and never parses a names
   packet.
2. **Only the names of what is bound.** Each slot's name comes from what reads it in the
   collection, and only from that. A reader is an enabled `data` motion modifier (on an
   item or a scene's camera) or a value binding whose key is `params` and whose field is
   `values.<n>`, for n from 0 to 31. A reader names the sender in its `input`, or every
   such sender when it names none, and one that names the sender wins a slot over one
   that doesn't. The name is what the reader drives: the item's source name and the
   property or setting ("Webcam X"), the scene's name and the camera property ("Stage
   Pan"), or the binding's target ("Master Volume"). A slot read twice takes the first
   reader's name and a count of the others ("Webcam X +1"). Names are cut to JTunnel's
   40 bytes and have their control characters removed. Nothing else in the collection
   goes out.
3. **Beside the keepalive.** The receiver sends a sender's list with each subscribe, every
   half second, while the collection names any slot, so a lost packet is corrected half a
   second later. Once the last name goes, it sends an empty list four more times, and
   then nothing. A receiver that never named anything sends nothing, so names a DAW
   project saved stay as they were. Names are worked out again after every applied batch,
   undo, redo and collection load, and when the receiver starts.
4. **Rule change.** ADR-0123 decision 3's first point now reads: JStream sends mDNS PTR
   queries, subscribe and bye packets, and, per this ADR, names packets. The rest of
   ADR-0123 stands.

<a id="consequences"></a>
## Consequences
- **Easier:** binding a DAW's modulation to anything in JStream labels itself in the
  DAW, which saves a project with those names.
- **Harder:** a host on the LAN that spoofs an accepted sender's address learns nothing
  new from the reply. Names go only to the accepted address, not to the spoofer. A
  sender the person accepted learns the names of the sources, scenes and buses its
  params are bound to, and nothing more. The person turns that off per sender with its
  state switch.
- **Harder:** whether a DAW shows renamed params at once is the host's choice. CLAP says
  `CLAP_PARAM_RESCAN_INFO` takes effect immediately, and a host that doesn't follow it
  keeps "Mod N". JTunnel's `params` key carries the names either way.
- **Must do:** the maintainer's hardware run checks the names in Bitwig. CI checks the
  packet's layout against JTunnel's, and a fake sender on loopback receives the names.
