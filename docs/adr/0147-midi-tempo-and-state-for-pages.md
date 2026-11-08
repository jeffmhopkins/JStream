<!-- @doc
id = "ADR-0147"
title = "The engine's tempo as shared values, and a page grant for one MIDI input"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0147: The engine's tempo as shared values, and a page grant for one MIDI input

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
#1224's last step gives pages and agents what MIDI does. Three accepted decisions
already shape it:

- [ADR-0122](0122-controller-input-midi-and-one-binding-system.md) §4 has the engine
  keep one tempo, from the first input that sends clock, and publish it as the shared
  values `tempo.bpm`, `tempo.beat`, `tempo.bar` and `tempo.playing`, so pages and
  agents read it like any other value.
- [ADR-0124](0124-page-api-read-review.md) §7 (maintainer tier) decides that tempo and
  MIDI each get their own checkbox and visible warning in the Page API group, off by
  default, before any page can read them. MIDI's warning is "A page given MIDI sees
  every note and control on its port." Its §6 lists what no page is ever given.
- [ADR-0142](0142-feeding-the-midi-sources-and-their-beat.md) has each input thread
  publish its tempo into its `MidiState`, and the every-input state follow one clock at
  a time.

The forces:

- A MIDI clock sends 24 messages a beat. Shared values change through the engine's
  lock and reach every client as `values.changed`, so they can't follow the clock
  message by message.
- A page that plays along with a performer needs notes as they happen, not once a
  beat. Pages are already sent updates at most once per page frame (ADR-0102 §3).
- MIDI arrives on input threads that take no engine lock.

<a id="decision"></a>
## Decision
1. **Tempo values.** When the JTunnel receiver starts, a `jstream-tempo` thread starts
   with it. Every 20 ms it reads the every-input tempo without a lock, and takes the
   engine's lock only when a published value would change. It sets:
   - `tempo.bpm`: the tempo to a tenth of a beat per minute. It moves only by at least
     a tenth, so a clock's jitter doesn't flicker it. It is removed while no clock runs.
   - `tempo.playing`: `true` while the transport plays and its clock runs.
   - `tempo.beat` and `tempo.bar`: whole quarter notes, and whole four-beat bars, since
     the start of the song.

   So the values change at most about once a beat while a song plays. Each change is a
   `values.changed` that names the client `engine`, with no request ID. The values are
   still ordinary shared values: a client may set these names, and the engine's next
   change overwrites them. `values.set` refuses nothing new, so ADR-0102's contract is
   unchanged.
2. **The MIDI grant (ADR-0124 §7).** The `browser` kind gains `page_api_midi`, a
   checkbox off by default, shown only while `page_api` is on, with ADR-0124's warning
   word for word. It also gains `page_api_midi_input`, the input's name (empty for every
   input), shown only while that checkbox is on. Without the checkbox, the name grants
   nothing. The Sources panel's badge names the grant, for example "MIDI (Keys)".
3. **What the page reads.** A granted page has `jstream.midi()`, which returns the
   latest reading as a fresh copy (`null` before the first), and a `midi` event with
   each new reading. A reading holds:
   - `input`, the input's name;
   - `tempo`: `null`, or `{bpm, playing, clock, beat, at}`, with `beat` carried to `at`;
   - the held `notes` and the non-zero `controllers`, as `[channel, key or controller,
     value]`, with channels 1 to 16;
   - non-zero `bend` and `pressure`, as `[channel, value]`;
   - `recent`: the last two seconds' note-ons, as `[at, channel, key, velocity]`.

   Times are seconds on the page's `jstream.clock`. A reading lists at most 128 notes,
   256 controllers and 32 recent notes. `recent` is a sliding window, not a list of
   events since the last update, so an update that replaces another one before it is
   sent loses no note.
4. **When it is sent.** A reading is made at a look where the input moved: a message
   since the last look, or a song still playing. It is sent only when it differs from
   the last one sent, at most once per page frame. While the input moves, the next
   page frame is always due. While it rests, the engine looks again every quarter
   second.
5. **Nothing else.** A reading carries no raw MIDI bytes, no SysEx, and nothing of any
   input but the one granted. It sends nothing back.

<a id="consequences"></a>
## Consequences
- An agent sees the tempo with `values.list {prefix: "tempo"}`. A page reads it as
  values (granted `tempo.*`) without the MIDI grant, which gives it whole beats only.
  With the MIDI grant it gets the exact beat, every page frame.
- A page sees a performer's notes within a page frame of the engine, and one that misses
  a frame still finds each note-on in `recent`.
- While a clock runs, a MIDI page is sent a reading every page frame, because the beat
  moves. While the input rests, it costs a look every quarter second and sends nothing.
- The tempo values exist only while JTunnel is on, since it is so far the only MIDI
  input. Local MIDI ports will feed the same state, and the same thread will serve them.
- `tempo.bar` counts four-beat bars. A page that needs another meter counts bars itself
  from `tempo.beat`, or from the beat it gets with the MIDI grant.
