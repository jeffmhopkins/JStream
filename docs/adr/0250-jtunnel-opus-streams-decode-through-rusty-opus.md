<!-- @doc
id = "ADR-0250"
title = "JTunnel's Opus streams decode through rusty-opus"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0250: JTunnel's Opus streams decode through rusty-opus

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
JTunnel's default format is Opus at 48 kHz in 5 ms frames. The receiver
([ADR-0122](0122-controller-input-midi-and-one-binding-system.md) §6) plays 24-bit PCM
and MIDI-only streams and refuses every Opus packet, counting it as `unsupported`, until
a decoder is picked "in its own PR". A sender left on its default format therefore plays
no audio, and since a frame's MIDI events ride in the same packet as its audio, its MIDI
is dropped with it. On a live setup this showed as about 200 refused packets a second
from one sender, while its state messages (its modulation params among them) went through.

[ADR-0159](0159-opus-decodes-in-pure-rust-on-linux.md) already compared the pure-Rust
Opus decoders and took `rusty-opus` (BSD-3-Clause, no dependencies, never panics on a
malformed packet) for Linux media files. Its licence is one the project accepts, so
picking it for JTunnel is not a maintainer decision.

<a id="decision"></a>
## Decision
The JTunnel receiver decodes Opus streams through `rusty-opus`, on every platform.
- `jstream-jtunnel`'s `OpusFrames` keeps one decoder per sender and decodes each frame
  once, in sequence order: a redundant copy of a frame already decoded, or a frame older
  than the last one decoded, is skipped and handed to the playout with no samples, where
  it is a repeat (or, for a frame that arrives after newer ones, held as silence with its
  events). A lost frame is left out and decoding goes on from the next. A new stream ID,
  rate or frame length makes a new decoder.
- A frame the decoder refuses is held as silence with its events kept, and counted in the
  receiver's `unsupported` counter, which now counts refused Opus frames.
- Decoding runs on the receive thread, never the audio thread. The decoder allocates when
  a stream starts and while its first frames warm it up.

<a id="consequences"></a>
## Consequences
- A sender on JTunnel's default format plays its audio and feeds its MIDI; no sender has
  to be switched to PCM for JStream.
- `rusty-opus` moves from a Linux-only dependency to one every platform builds, still with
  no transitive dependencies.
- Concealment stays the playout's silence: the decoder's own packet-loss concealment is
  not used.
