<!-- @doc
id = "CRATE-jstream-midi"
title = "jstream-midi"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-midi

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-midi/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-midi"]
-->

Layer-0 MIDI model for controller input
([ADR-0120](../../docs/adr/0120-controller-input-midi-and-one-binding-system.md)).
Every MIDI input, a JTunnel stream or a local port, produces its types. No I/O, no
allocation, no locks and no dependencies, so an input thread can run it on every event.

Contents:
- `Message`: a MIDI 1.0 channel voice, system common or real-time message, parsed from
  exactly its bytes (`Message::parse`) and written back (`to_bytes`). A note-on with
  velocity 0 parses as a note-off. SysEx, running status and the undefined status bytes
  are refused with a `ParseError`: transports carry SysEx in their own framing and
  deliver whole messages. `message_len` gives a status byte's message length.
- `Channel`: 0 to 15 on the wire, shown 1 to 16.
- `Tempo`: MIDI clock (24 per quarter note), start, stop, continue and song position,
  each with its media-clock time in nanoseconds, become a `TempoNow`: BPM averaged over
  the last beat of clocks, whether the clock and the transport run, and the beat and bar
  (at the meter the tracker was made with), carried between clocks at the tempo but never
  past the next clock's position. `time_of_beat` and `next_bar_at` extrapolate when a
  beat or the next bar line lands. It follows the MIDI specification: the clock after
  start plays position 0, stop holds the position, song position is taken only while
  stopped, and the tempo is measured from clocks whether or not the transport plays.

`tests/no_alloc.rs` checks that parsing and the tracker allocate nothing.
