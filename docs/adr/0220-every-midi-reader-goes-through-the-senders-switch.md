<!-- @doc
id = "ADR-0220"
title = "Every MIDI reader goes through \"Use this sender's MIDI\""
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0220: Every MIDI reader goes through "Use this sender's MIDI"

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) §3, a maintainer decision, says a
JTunnel sender's MIDI drives bindings only once the person turns on "Use this sender's
MIDI" for it. [ADR-0207](0207-mute-actions-and-held-triggers.md) decision 4 built that
switch (`MidiInputs::bindable`) and put the engine's action-binding runner behind it.
It left value bindings ([ADR-0178](0178-value-bindings-drive-any-number.md)), channels
([ADR-0183](0183-virtual-control-channels.md)) and `midi` motion modifiers
([ADR-0168](0168-midi-controllers-drive-motion.md)) reading every sender's MIDI with no
check, and its consequences list putting them behind the switch as a must do (#1710).
So, while the switch was off, an accepted or spoofed sender's controls could still move
faders, mutes and settings bound to them.

<a id="decision"></a>
## Decision
This amends ADR-0207 decision 4: **the switch gates every MIDI reader, not only action
bindings.**

1. **A named input is read only while it is let in.** Value bindings, channels and
   `midi` modifiers all read MIDI through one function (`Control::read` in
   `jstream-core`). It reads an input whose state isn't marked bindable as a control at
   rest: every value at 0, a bend at its centre, no note held.
   `MidiInputs::set_bindable` marks each named input's state, one atomic, and an input
   first seen later takes the set current then.
2. **A reader naming no input reads only the inputs let in.** It reads a bound state
   that each input feeds only while it is let in, never the every-input state, which
   also holds the senders left out. When the switch drops an input, the bound state is
   rebuilt from what the inputs still let in hold, so a key or a controller the dropped
   input held is let go there as it is for a reader that names it.
3. **Switched on, an input is read from where its controls are.** Its own state goes on
   recording while it is off, so it can still be drawn.
4. **Drawing is unchanged.** The `midi_*` sources draw every sender, and an input's
   clock gives their beat ([ADR-0142](0142-feeding-the-midi-sources-and-their-beat.md)):
   drawing moves nothing in the collection or the mix.

<a id="consequences"></a>
## Consequences
- **Easier:** one gate for every reader. A new reader that reads through `Control::read`
  and the bound state is behind the switch.
- **Harder:** a fader, channel or modifier bound to a sender that is switched off reads
  its control at rest until the switch goes on, and then jumps to where the control is.
- **Harder:** a message an input still let in sends while the bound state is being
  rebuilt can be overwritten by what that input held just before, until its next
  message.
- **Must do:** a MIDI input other than a JTunnel sender, such as a local port, must be
  named to `set_bindable` to move anything.
