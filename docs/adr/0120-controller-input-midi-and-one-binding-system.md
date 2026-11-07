<!-- @doc
id = "ADR-0120"
title = "Controller input: MIDI and JTunnel, one binding system for every trigger"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0120: Controller input: MIDI and JTunnel, one binding system for every trigger

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The maintainer wants a DAW to drive JStream over MIDI, so that scene changes and effects
land on the beat (#1222), the DAW's audio to arrive as a source (#1223), and MIDI to be
drawn by native sources (#1224). The first transport is JTunnel, a CLAP plugin in the
maintainer's [Jclap](https://github.com/jeffmhopkins/Jclap) repository, which JStream
speaks natively with no plugin of its own. Local MIDI ports must work through the same
path. A VISCA PTZ controller (#1181) and JStream controlling PTZ heads (#1182) bind to
the same actions, so all of it is one controller-input system.

What JTunnel does, from its source (`plugins/jtunnel/src/protocol.h`, `discovery.h` and
its README):

- **Wire.** Plain UDP datagrams, little-endian, wire version 2 (JTunnel 0.11 and later),
  never over 1472 bytes. A receiver subscribes to a sender by name every half second,
  and the sender unicasts audio packets back to it. Each audio packet carries one or two
  frames (the second is the previous frame again, with redundancy on). A frame is 5 ms
  of Opus at 48 kHz, up to 2 ms of 24-bit PCM at the sender's rate, or 1 ms of nothing
  (MIDI only), plus the MIDI that falls in it, each event with its sample offset into
  the frame: MIDI 1.0 messages, SysEx in numbered pieces, and CLAP note expressions.
- **Clock.** MIDI is sample-locked to the audio, so an event's time is the sender's
  sample position. The receiver estimates the sender's clock against its own from packet
  arrivals and holds a configured lead (its buffer) over late packets.
- **Discovery.** Senders announce `<name>._jtunnel._udp.local.` over mDNS, with an SRV
  record for the port and TXT records for who claims the name; a receiver finds them
  with a PTR query, and takes a sender's address from where its announcement came from.

Hotkeys are already "a trigger bound to a protocol action, kept in the collection"
([ADR-0024 §5](0024-scene-structure.md), [ADR-0112](0112-assignable-camera-zoom-bindings.md)),
and every control path ends as a protocol action
([R-ARCH-001](../RULES.md#R-ARCH-001)). Shared values
([ADR-0102 decision 7](0102-page-api-for-browser-sources.md)) give pages and agents
named values to read.

<a id="decision"></a>
## Decision
1. **One binding system.** A binding is a trigger and what it does, kept in the
   collection, listed and edited in one place (the Hotkeys list grows into Bindings) and
   set over the protocol and MCP like hotkeys are.
   - **Triggers** are a key chord (ADR-0024 and ADR-0112's `Chord`, unchanged), a MIDI
     note, control change or program change (on one channel or any, from one input or
     any), and later a VISCA controller button. Nothing is MIDI-only: every trigger
     binds to every action.
   - **Action bindings** run a protocol action when the trigger fires, as hotkeys do
     now: scene and preview switches, transitions, item show, hide and toggle
     ([ADR-0101](0101-item-show-hide-actions.md)), motion triggers, timelines
     ([ADR-0115](0115-timelines-on-the-wire.md)), filter enable and disable. A note
     binding fires on note-on; a CC binding fires when the value crosses the middle,
     upward, so a button that sends 127 then 0 fires once.
   - **Value bindings** let a CC (or pitch bend, or channel pressure) drive a number: an
     item property or a filter setting, mapped from the controller's range to a range
     the binding names. The value is live, like motion modifiers
     ([ADR-0099](0099-native-motion-modifiers-and-timelines.md)): applied as it is
     drawn, never stored, never an edit, so a fader fills no undo history.
   - The wire shape (a `binding.set` beside `hotkey.set`, or `hotkey.set` taking a
     trigger) is decided in the protocol PR that adds it, as an additive minor version
     under [R-API-001](../RULES.md#R-API-001). `hotkey.set` keeps working for chords.
2. **MIDI input threads never touch real-time locks.** Each input (a JTunnel stream, a
   local MIDI port) has its own thread. It parses messages with `jstream-midi`, stamps
   each with the media-clock time it maps to ([ADR-0014](0014-media-clock.md)), and
   hands it to the engine through a bounded lock-free queue. It never takes a lock the
   audio or render threads take ([R-RT-001](../RULES.md#R-RT-001)), and a full queue
   drops the newest event and counts it.
3. **Timing.** A JTunnel event's time is the sender's sample position, mapped through
   the receiver's estimate of the sender's clock, plus the stream's buffer. A local
   port's event time is the port's own timestamp, mapped to the media clock. An action
   bound to an event fires on the first render tick at or after the event's time, so a
   note on the beat switches the scene on the frame that shows the beat, not whenever
   the packet arrived. The latency from a note leaving the DAW to the frame that shows
   it is measured with the bench and recorded in the PR that wires the first trigger.
4. **Tempo.** `jstream-midi`'s tracker turns MIDI clock, start, stop, continue and song
   position into a tempo, beat and bar. The engine keeps one tempo, from the first input
   that sends clock (a setting picks one when several do), and publishes it as shared
   values `tempo.bpm`, `tempo.beat`, `tempo.bar` and `tempo.playing`, so pages and agents
   read it like any other. Motion modifiers, timelines and effects lock to it in the
   PRs that add those options.
5. **JTunnel is built in, as a receiver only.** JStream speaks JTunnel's wire version 2,
   subscribes to senders and receives their audio and MIDI. It never announces itself
   and never sends a stream. One JTunnel connection feeds both a network audio source
   (#1223) and the binding system, on one clock. A sender of another wire version is
   shown as incompatible, not guessed at.
6. **Network audio is a source kind.** A `network_audio` kind (transport `jtunnel`, and
   room for others) takes the same path as a device input: a receive thread, a jitter
   buffer at the stream's lead, adaptive resampling to the engine rate with drift
   estimated from the sender's timestamps, and the mixer's ring, under R-RT-001. Lost
   frames are concealed and counted; its latency, buffer fill, drops and drift go in
   `profile.get`. 24-bit PCM and MIDI-only streams come first. Opus needs a decoder,
   picked in its own PR (a dependency whose licence the project doesn't already accept
   is a maintainer decision).
7. **The network posture is its own decision.** Listening for mDNS on the LAN and
   receiving datagrams from a sender is a security posture question
   ([ADR-0043](0043-narrow-maintainer-tier.md)), so it is split out into a
   maintainer-tier ADR, as are VISCA's sending and listening. Until that ADR is accepted,
   nothing that opens a socket for JTunnel or VISCA is built; the crates below have
   none.
8. **Three crates for the formats.** The formats are pure code, with no I/O and no
   dependencies, that the engine, the sources and the fuzzer all need:
   - `jstream-midi` (layer 0): MIDI 1.0 messages and the tempo tracker. Local ports and
     JTunnel both produce its `Message`, so it can't live in the JTunnel crate.
   - `jstream-jtunnel` (layer 1, on `jstream-midi`): JTunnel's packets, SysEx
     reassembly, sequence tracking and its mDNS records.
   - `jstream-visca` (layer 0), when VISCA starts: commands, replies and VISCA-over-IP
     framing, for both directions.
   They're crates rather than modules of `jstream-media` or `jstream-core` because the
   dependency boundary needs it: the parsers of datagrams from the network are fuzzed on
   their own, and `jstream-sources` (layer 4), `jstream-core` (layer 5) and the audio
   path all use them without one depending on another.

<a id="consequences"></a>
## Consequences
- **Easier:** a DAW, a MIDI controller, a keyboard and later a PTZ joystick all drive
  JStream through one list of bindings and one set of actions, and an agent can set any
  of them over MCP.
- **Easier:** JTunnel's format is checked against bytes JTunnel itself writes, and
  fuzzed, before anything reads it from a socket.
- **Harder:** bindings now come from inputs with their own clocks, so the engine has to
  order events from several threads by media-clock time before a render tick takes them.
- **Must do:** the maintainer-tier network-posture ADR; then, each in its own PR, the
  protocol additions (triggers and value bindings, the `network_audio` kind, tempo
  values), the JTunnel receive thread and discovery, local MIDI ports, and the MIDI
  visualization sources (#1224).
- JTunnel's wire version is JTunnel's to change. A new version is a new parser here,
  and an older JStream shows such senders as incompatible.
