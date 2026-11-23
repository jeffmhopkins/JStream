<!-- @doc
id = "ADR-0185"
title = "A channel names the JTunnel Mod it reads"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0185: A channel names the JTunnel Mod it reads

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0181](0181-jstream-names-a-jtunnel-senders-params.md) decision 2 names a JTunnel
sender's modulation params (Mod 1 to Mod 32) after what reads them. It lists the readers:
enabled `data` motion modifiers on items and scene cameras, and value bindings.
[ADR-0183](0183-virtual-control-channels.md) decision 3 added a fourth kind of reader: a
channel's `data` input. A DAW's automation goes into a channel this way, and from there it
drives several things at once. That is the main way a person feeds Mods into a show, but
ADR-0181 doesn't count it, so the DAW still shows those Mods as "Mod N".

This ADR is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It amends ADR-0181 from the
subject of channels. Nothing about who receives names, or when, changes. The names still
go only to accepted senders whose state is on, at most 40 bytes each, with control
characters removed. The only addition is one more kind of collection object name, the
channel's, under the rule that is already accepted: the name of what reads the slot.

<a id="decision"></a>
## Decision
1. **A channel's input is a reader.** ADR-0181 decision 2 also counts a channel's `data`
   input whose key is `params` and whose field is `values.<n>`, for n from 0 to 31. Like
   any other reader, it names the sender in its `input`, or any sender when it names none.
2. **Named after the channel.** Such a slot takes the channel's `name`. When the name is
   empty, it takes the channel's ID as words (`house_lights` reads "House Lights"). A
   channel's own `channel` inputs and `amount` references read no Mod, so they name
   nothing.
3. **Everything else stays as ADR-0181 says.** That covers ordering, `+N` for a slot that
   more than one reader reads, the 40-byte cut, sending beside the keepalive, clearing, and
   the state switch.

<a id="consequences"></a>
## Consequences
- **Easier:** a Mod that feeds a channel shows that channel's name in the DAW.
- **Harder:** an accepted sender whose state is on also learns the names of the channels
  its params feed, along with the source, scene and bus names it already learned. Its
  state switch still turns that off.
- **Must do:** `param_names` walks every channel's inputs. The engine test checks a named
  and an unnamed channel against a loopback sender, and that both names clear once the
  channels are deleted.
