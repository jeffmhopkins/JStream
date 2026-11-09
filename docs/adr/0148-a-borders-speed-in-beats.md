<!-- @doc
id = "ADR-0148"
title = "A border's speed in beats, from the engine's MIDI clock"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0148: A border's speed in beats, from the engine's MIDI clock

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The `border` kind (#1227) moves its dashes and its gradient round the edge at `speed`,
in px/s. #1294 asks for that speed in beats, so a border marches in time with the music.
It waited for a tempo source (#1222). That source is now built:
[ADR-0142](0142-feeding-the-midi-sources-and-their-beat.md) has each MIDI input publish
its clock's tempo, beat and transport into its `MidiState`, and the every-input state
follow one clock at a time. [ADR-0147](0147-midi-tempo-and-state-for-pages.md) shows
that tempo to pages and agents as the `tempo.*` shared values.

Those values change through the engine's lock, at most about once a beat. They are
too coarse to draw from: a border has to move smoothly between beats. The `midi_beat`
source already reads the every-input state on the render thread, without a lock, and
carries the beat on to the time it draws.

A border can't just turn its laps per beat into px/s from the tempo. The pattern would
drift off the beat each time the tempo changed, and it would keep moving when the song
stopped.

<a id="decision"></a>
## Decision

**1. The setting.** `border` gains `speed_unit`, a choice of `px_s` (the default, as
before) or `beats`. In `beats`, it reads `lap_beats` (−64 to 64, default 4) and not
`speed`: the dashes and the gradient go once round the edge every `lap_beats` beats.
Negative goes anticlockwise, and 0 holds still. The Inspector shows only the setting
of the chosen unit. A border saved before has no `speed_unit`, so it is still in px/s.

**2. From the song's position.** In beats, the distance the pattern has travelled is
the perimeter × beat ÷ `lap_beats`. The beat is the song's position: quarter notes
since its start, as the every-input MIDI clock gives it. That keeps the pattern on the
beat whatever the tempo does. It stops where the song stops, and it jumps with a song
position pointer. With no clock heard yet, the beat is 0 and the pattern stays at its
start. When a clock goes quiet while playing, the beat runs on by no more than one tick
past its last clock (ADR-0142), so the pattern holds.

**3. How the beat reaches a filter.** `FilterClock`, the media-clock reading every
filter draws with, gains `beat`. When the engine is given its MIDI inputs, the filter
assets plans read get them too. A plan then hands them to its shot.

Each frame, the render thread reads the every-input tempo once, at the frame's
media-clock time. It does that without a lock or an allocation, as the `midi_beat`
source does. It hands the same beat to every chain it runs that frame, including the
master and output stages. A shot planned without MIDI inputs (a test, a `video.snapshot`
of a collection with none) is always at beat 0.

**4. Compatibility.** No protocol or collection change. A kind's settings are free
JSON checked against its descriptor, and the descriptor is data. Only the built-in
kinds snapshot changes. A client that doesn't know `speed_unit` still shows `speed`.
Other kinds can follow the beat the same way later, through `FilterClock::beat`.

<a id="consequences"></a>
## Consequences
- **Easier:** a dashed or gradient border marches round an item in time with a DJ's or
  a DAW's MIDI clock, with one setting and no page.
- **Harder:** a frame's chains now depend on the MIDI state as well as the collection
  and the clock, so two frames at the same media time can differ if the song moved
  between them.
- **Limits that remain:** the tempo comes only from MIDI clock, the every-input state's.
  A border can't choose which input it follows, and there is no tap tempo. A motion
  modifier's speed in beats is a separate question, left for later.
