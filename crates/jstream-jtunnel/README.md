<!-- @doc
id = "CRATE-jstream-jtunnel"
title = "jstream-jtunnel"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-jtunnel

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-jtunnel/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-jtunnel"]
-->

Layer-1 crate for JTunnel, the plugin that sends a DAW track's audio and MIDI over the
LAN, which JStream receives natively
([ADR-0122](../../docs/adr/0122-controller-input-midi-and-one-binding-system.md)). It is
the format only: no sockets and no threads, so the engine's receive thread, the tests and
the `jtunnel` fuzz target drive the same code. It depends on `jstream-midi` alone.

Contents:
- `wire`: version 2 of JTunnel's packets. `parse` takes a datagram and refuses what
  JTunnel's own parser refuses (another version, a malformed field, a truncated frame),
  ignoring bytes after the packet as JTunnel does. An audio packet has its header and one
  or two frames, newest first; a frame walks its events (`events`, decoded: a MIDI
  `Message`, an undefined status byte, a SysEx piece or a CLAP note expression; or
  `raw_events`, as wire bytes, which `decode` turns into an event later) with their
  sample offsets, and has its coded audio.
  `write_subscribe` and `write_bye` are what a receiver sends; `write_audio` writes what
  a sender would, for tests and fuzzing. `decode_pcm24` decodes 24-bit PCM at JTunnel's
  scale. A test checks the parser and writer against bytes JTunnel's C++ writer wrote.
- `SysexAssembler`: joins SysEx pieces into whole messages in a buffer allocated once,
  dropping a message whole when a piece is missing or out of order, or when the caller
  abandons it after a lost frame.
- `SeqTracker`: per stream, tells a fresh frame from a repeat (a redundant copy) or a
  stale one, sees a restart (a new stream id, or a jump of more than 2^16), and counts
  frames that left its 64-frame window without arriving.
- `SenderClock`: maps a sender's stream position to media-clock time from the frames'
  arrivals. It keeps the lowest offset (arrival minus sender time) in each second and
  fits a line through the last 32: the slope is the clocks' drift, reported once 15 s of
  stream have arrived, and the line, lifted onto the lowest offset, is when a sample
  arrives with no jitter. Before then it uses the lowest offset seen.
- `Playout`: one stream's jitter buffer, 64 frames preallocated. `push` takes a frame
  with its decoded samples and arrival time, telling a new stream, a repeat and a frame
  too late to play; `pop` gives back each frame when it is due (its jitter-free arrival
  plus the lead), in order, with its position, media-clock time, samples and events, a
  silent frame marked concealed in place of one that never came, and a discontinuity
  flag after a restart. The times run on from one frame to the next and slew toward the
  clock estimate by at most 0.5% of a frame, so the mixer never sees them jump; an error
  past 50 ms jumps instead and marks a discontinuity. A stream that outruns the buffer
  skips ahead, concealing what it couldn't hold.
- `dnssd`: the mDNS PTR query for `_jtunnel._udp.local`, `parse_announcements` for the
  SRV and TXT records senders answer with (DNS name compression followed with a bound),
  and `Directory`, the list of senders seen, refreshed by announcements, removed by a
  goodbye and forgotten 35 s after it was last heard.

`tests/no_alloc.rs` checks that parsing a packet, walking its events, decoding PCM,
joining SysEx, tracking sequence numbers and playing frames out allocate nothing.
`tests/link.rs` runs a sender 150 ppm off this machine's clock over 90 s of simulated
Wi-Fi (jitter, 60 ms stalls, 2% loss, redundancy on): the drift is found within 20 ppm,
every frame comes out in order with steps of 2 ms give or take 5%, and after settling
only frames whose both copies were lost are concealed.
