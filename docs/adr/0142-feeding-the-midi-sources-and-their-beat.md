<!-- @doc
id = "ADR-0142"
title = "The engine feeds the MIDI sources on the media clock, with each input's tempo"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0142: The engine feeds the MIDI sources on the media clock, with each input's tempo

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0137](0137-midi-visualization-sources.md) gave the MIDI sources a lock-free state per
input in `MidiInputs`, and left feeding it from real inputs, and a beat indicator from the
tempo, for later steps of #1224. This decides both. It builds on ADR-0137 and replaces
none of it.

The forces:

- The JTunnel receiver (ADR-0123) is the first input that has MIDI. A sender may send
  MIDI with no audio, and no source may be recording it.
- Messages are stamped on the media clock (ADR-0014). A source is painted at a time
  that counts from its own start. The kinds compared the two, so on a real host every
  note looked like it was in the future, or long past.
- ADR-0122 §4 has `jstream-midi`'s tracker turn MIDI clock and transport into a tempo,
  a beat and a bar. The tracker is plain state, not atomics, so only one thread can run
  it.
- A show may have two inputs that both send clock.

<a id="decision"></a>
## Decision
1. **The host makes one `MidiInputs`** and puts it in the sources' context. It gives
   it the media clock once (`set_clock`). The kinds measure a note's age on that clock,
   which is the clock the note was stamped on. Without a clock, as in tests, they fall
   back to the paint time.
2. **An input thread feeds it through a `MidiFeed`** (`MidiInputs::feed(name)`), one per
   input. The feed looks up the input's own state and the every-input state once, so it
   takes no lock per message. The JTunnel receiver keeps one feed per accepted sender.
   It pushes each frame's MIDI events as the frame falls due, at the media-clock time
   of the event's sample, whether or not a source records the sender. MIDI from
   JTunnel feeds only what the MIDI sources draw. It doesn't reach the binding system.
3. **Each feed runs the input's tempo.** Its clock, start, stop, continue and song
   position run its own `jstream-midi` tracker on the input thread. The feed publishes
   what the tracker reads (time, tempo, beat, playing) into the input's state and the
   every-input state. The reading sits in atomics behind a sequence number, so neither
   side locks, and a reader retries rather than see it torn. The every-input state
   follows one input's clock at a time. Another input takes it over only after the
   first has been silent for half a second, so two clocks never fight over it.
4. **A fifth kind, `midi_beat`**, draws a row of a bar's beats. The beat the clock is on
   is lit: it flashes as the beat starts and dims as it passes, and the bar's first
   beat has its own colour. Stopped, it shows where the song is, dimmed. It has an
   `input` setting but no `channel`, since a clock belongs to the whole input. Its meter
   is a setting, `beats_per_bar`, because MIDI clock carries none.
5. **Not yet.** The engine-wide tempo and the shared values `tempo.*` that ADR-0122 §4
   gives pages and agents, and MIDI state for browser pages, are not this decision.
   They change the protocol, and come with the page step and its slot in the protocol
   queue. Neither are local MIDI ports. This adds no protocol and no persisted data.

<a id="consequences"></a>
## Consequences
- A JTunnel sender's notes, controllers and beat draw as soon as it is accepted, even
  for a MIDI-only sender.
- On the input path, each message costs a few atomic stores more than ADR-0137's, and a
  clock message also costs one step of the tracker. Nothing allocates.
- The every-input beat shows the first clock heard. To show a particular input's beat,
  a `midi_beat` source names that input.
- A controller plugged into this machine draws nothing until the host opens local
  ports. That needs its own feed per port.
- A host on the LAN that spoofs an accepted sender (ADR-0123 §6) can move what that
  sender's MIDI sources draw, its beat included. It still reaches nothing else.
