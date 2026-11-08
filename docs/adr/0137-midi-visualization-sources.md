<!-- @doc
id = "ADR-0137"
title = "MIDI visualization sources draw a lock-free per-input state on the CPU"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0137: MIDI visualization sources draw a lock-free per-input state on the CPU

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Issue #1224 asks for sources that show what a MIDI performer plays: a keyboard whose keys
light, a scrolling piano roll, drum pads, and controller meters. MIDI already reaches the
engine for bindings and tempo (`jstream-midi`, #1222), but no source can read it.

The forces:

- MIDI arrives on input threads, one per port or JTunnel sender. A source paints on the
  painter thread at its own frame rate. Neither may block the other, and the input path
  must not allocate (R-RT-001), so a burst of notes never stalls audio or video.
- A show can have several controllers. A source should be able to draw one of them or
  all of them together, and one channel or all of them.
- What a source draws is small: which keys are held and how hard, each controller's
  value, pitch bend, channel pressure, and for the piano roll the last few seconds of
  notes.
- The scope sources (`waveform`, `spectrum`) already solve the same shape of problem for
  audio: a lock-free feed written by one thread, and a painter that draws CPU frames at
  30 fps from a small pool, settling on one frame while nothing changes.

<a id="decision"></a>
## Decision
1. **State.** `jstream-sources` gains `MidiState`: per channel and key, the held velocity;
   per channel and controller, the value; per channel, pitch bend and channel pressure;
   and a log of the last 1024 note events. Every field is an atomic, and the log is a
   ring of one-word slots, each behind a sequence number. Any number of threads push and
   read without locks or allocation, and a reader skips a slot being written rather than
   see it torn. All-notes-off (CC 123) and all-sound-off (CC 120) release the channel's
   held keys.
2. **Inputs.** `MidiInputs` holds one `MidiState` per input name, made the first time a
   name is asked for, at most 64 of them, and one state that every input feeds. The host
   puts it in `Context::midi`. An input thread looks its state up once, then pushes to it
   and to the shared one itself. The name lookup takes a lock, so it is not done per
   message.
3. **Kinds.** Four kinds draw a state: `midi_keyboard`, `midi_piano_roll`, `midi_pads`
   and `midi_cc`. Each has an `input` setting (a name, or empty for every input) and a
   `channel` setting (1 to 16 as a device shows it, or 0 for every channel). Size and
   `input` restart the source; every other setting is live. Colours scale with velocity
   from 35% to full brightness, so a soft note still shows.
4. **Drawing.** Like the scope sources, a MIDI source paints CPU RGBA frames from a pool
   of four at 30 fps through its `Painter`, and only while something changes. A source
   whose input has had no new message, and whose drawing has nothing still moving (a pad
   fading, a roll scrolling), keeps its frame and is polled every 250 ms. Without a
   `Context::midi`, a source draws its first frame and never asks for another.
5. **Not yet.** This decision adds no protocol and no persisted data: the kinds reach
   clients through `kind.list`, as every kind does. Feeding `MidiInputs` from real ports
   and JTunnel, a beat indicator from the tempo, and exposing the state to browser pages
   are later steps. Whichever of them changes the protocol takes a slot in the protocol
   queue then.
6. **Feeding it** (amended with #1224's host wiring). The engine makes one `MidiInputs`
   and gives it its media clock (`set_clock`), and the JTunnel receiver pushes each
   accepted sender's MIDI events to it under the sender's name, at the media-clock time
   of their sample offset. The kinds measure a note's age on that clock (`now_ms`), not
   on the paint time a source is given, which counts from the source's start: the two
   differ, and comparing them hid every note. MIDI from JTunnel only feeds what these
   sources draw; it doesn't reach the binding system (ADR-0122).

<a id="consequences"></a>
## Consequences
- Drawing a performance costs nothing on the input path beyond a few atomic stores per
  message, and nothing on the painter thread while the performer rests.
- Every source of one input draws the same state, so a keyboard and a roll of one
  controller always agree.
- The piano roll shows at most the last 1024 note events, so a very dense performance
  over a long window loses its oldest notes. A key still held after its note-on left the
  log is drawn from the left edge.
- A name past the 64-input cap, or too long, gets an empty state: such a source draws
  nothing, though its messages still reach the shared every-input state.
- Sustain (CC 64) is stored like any controller but doesn't hold keys: a key lifts on
  its note-off even while the pedal is down. A keyboard shows the fingers, not the sound.
- The every-input state keeps one slot per channel and key, so when two inputs hold the
  same key on the same channel and one releases it, it shows as up there. A source that
  names its input doesn't share that slot.
- A settled source compares a count of every message pushed, not the clock or the note
  log, so a controller change in the same millisecond as the last one, or from an input
  whose clock lags, still redraws it.
- JTunnel feeds `MidiInputs`; local MIDI ports don't yet, so a controller plugged into
  this machine draws nothing until the host opens its ports.
