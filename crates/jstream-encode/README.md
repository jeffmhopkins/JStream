<!-- @doc
id = "CRATE-jstream-encode"
title = "jstream-encode"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-encode

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-encode/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#threading", "docs/ARCHITECTURE.md#data-model"]
verified = { hash = "b3:60e969ccc349d391", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Dependency ARCH#data-model changed only through ADR-0023 (opacity, blend, effects, duplicated_from); outputs keyed by mix and encoder profile still share one session, as stated" }
-->

Layer-3 encoder sessions ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). A
session owns one platform encoder, `jstream-platform`'s `VideoEncoder` or
`AudioEncoder`, and runs it on its own **encode thread**, the "Encode (per session)" row
of [ARCH › Threads](../../docs/ARCHITECTURE.md#threading). The render side (one video
mix) or the mixer side (one audio bus) pushes frames or sample blocks into it, and any
number of outputs subscribe to its packets. For M1a that is one video mix into the MF
H.264 encoder and one bus into the MF AAC encoder, each feeding one RTMP output
([ROADMAP › M1](../../ROADMAP.md#m1)); the fan-out already takes N subscribers, so
encoder sharing in M1b ([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model):
outputs with the same mix and encoder profile share one session) needs no new shape.

Its only JStream dependency is `jstream-platform` (layer 1), whose `media` re-export
supplies the frame, sample and packet types; besides that, `thiserror`. Dev-dependency:
`jstream-platform`'s `fake` feature, for `FakeCodecs`.

Why a crate: the crate map puts encoder sessions in their own layer-3 crate, between
the platform traits they drive and the layer-4 outputs that consume them, and it keeps
the encode thread's rules ([R-RT-002](../../docs/RULES.md#R-RT-002),
[R-RT-004](../../docs/RULES.md#R-RT-004)) applied to one place.

**API.**
- `VideoSession` / `AudioSession`: `open(codecs, config, options)` opens the encoder
  through `Codecs` and starts the thread; `start(encoder, config, options)` takes an
  encoder that is already open. Both check the codec is of the right media kind.
  `input()`, `subscribe()`, `subscribe_with_capacity(n)`, `info()`, `stats()` and
  `finish()`. Sessions are `Send + Sync`, so outputs can share one through an `Arc`.
- `VideoInput` / `AudioInput`: cloneable push handles. `push` never blocks.
- `Subscription`: `recv`, `try_recv`, `recv_timeout`, each returning packets in decode
  order and then a `StreamEnd`; `info()` and per-subscriber `stats()`.
- `StreamInfo`: the `CodecInfo` that was opened plus `EncoderSettings` (the video or
  audio config it was opened with).
- `SessionOptions`, `SessionStats`, `SubscriberStats`, `SessionKey`,
  `SessionRegistry`, and the errors `EncodeError`, `PushError`, `StreamEnd`.

**Tests** (`src/tests.rs`) are hermetic ([R-TEST-004](../../docs/RULES.md#R-TEST-004)) and
run real encode threads against `FakeCodecs` or scripted encoders. Each is
deterministic by construction (queues larger than the pushes, encoders gated by the
test, subscribers that read only after `finish`), never by sleeping: order and `pts`
kept for video and audio; a full input queue drops and counts; a busy encoder at
`finish` drops and counts; a slow subscriber loses packets while a fast one gets all;
a late subscriber starts at the next keyframe with the stream's `info`; `finish`
drains; an encoder error and a panic reach every subscriber; dropping a session closes
the streams and joins the thread; dropped audio blocks become silence; the fan-out's
keyframe resync; and the registry.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-encode/src/**"]
depends_on = ["docs/RULES.md#R-RT-002", "docs/RULES.md#R-RT-004", "docs/adr/0003-media-stack-and-codecs.md", "docs/adr/0014-media-clock.md"]
verified = { hash = "b3:cbab9c6628116bcf", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section, checked against session.rs/fanout.rs/audio.rs/video.rs/registry.rs: can_accept-gated recv and park+poll every poll_interval, try_send drop-newest input queue, silence fill only after a marked drop up to max_audio_gap, busy and finish_timeout drops, per-subscriber try_send with keyframe resync and Joining skip uncounted, end recorded before queues drop, catch_unwind, abort-and-join on drop, defaults 8/512/4ms/2s/1s match SessionOptions::default" }
-->

### Threads and pacing

Each session spawns one named thread (`jstream-encode-video` or `-audio`) that owns
the encoder, the input queue's receiving end and every subscriber's sending end. It
follows the platform call pattern: take an input only when `can_accept` is true, then
`poll` after every input and at least every `poll_interval` (4 ms by default), input or
not, because the Media Foundation asynchronous encoders only make progress when their
events are pumped. When the encoder can't accept, the thread parks for one interval
and polls again, leaving inputs in the queue. It never waits on the control plane
([R-RT-002](../../docs/RULES.md#R-RT-002)). Packets are fanned out by cloning an `Arc`,
and the packet and subscriber vectors are reused, so this crate's loop does not
allocate per frame (the encoder itself may).

Handles talk to the thread through `std::sync::mpsc` channels and atomics only: a
bounded channel for inputs, an unbounded one for new subscribers, atomic counters, and
`finishing`/`aborting` flags, with a `Wake` message and `unpark` so a flag is seen
without waiting out the interval. `std` covers this; the workspace has no channel
crate.

### Queues and drop policies (R-RT-004)

- **Input queue** (`input_capacity`, 8 by default). `push` uses `try_send`, so the
  render or mixer thread never blocks. When the queue is full the input is dropped and
  counted (`dropped_queue_full`). Because the thread takes inputs only when the encoder
  can accept them, a slow encoder backs up into this queue and the newest inputs are
  dropped at the source.
- **Video drops need no repair.** The encoder places keyframes by counting the frames
  it is given, so a dropped frame never removes a keyframe from the stream, and the
  `VideoEncoder` trait has no forced keyframes to protect. Frame times keep their gap.
- **Audio drops are filled with silence.** Audio encoders time packets by counting
  samples from the first block, so a dropped block would pull every later packet
  earlier and drift audio against video. The push after a drop is marked, and the
  thread encodes silence from the end of the last block it encoded to the marked
  block's `pts` (in sample frames, the input format's time base), when that gap is
  positive and at most `max_audio_gap` (1 s by default). Gaps without a drop are left
  alone, so rounding in upstream timestamps never inserts samples. Filled frames are
  counted (`silence_frames`).
- **Encoder busy.** An encoder that returns `Busy` despite `can_accept` drops the input
  (`dropped_encoder_busy`). At `finish`, queued inputs the encoder can't take within
  `finish_timeout` (2 s) are dropped the same way.
- **Subscriber queues** (`subscriber_capacity`, 512 packets by default, or per
  subscription). Each subscriber has its own bounded queue, offered with `try_send`. A
  full queue drops the packet for that subscriber only (its `dropped`, and the
  session's `subscriber_drops`); the encoder and the other subscribers are unaffected.
  After a video drop the subscriber is resynchronised: it is offered nothing until the
  next keyframe, since the packets in between reference the one it lost, and those
  skipped packets count as dropped too. A subscription that was dropped is removed on
  the next packet.

### Late subscribers and codec configuration

A new subscriber joins on the encode thread before the next packet is published, and
is offered packets starting at the next keyframe, so its stream is decodable from the
first packet; skipping to it isn't counted as a drop. Every AAC packet is a keyframe,
so audio subscribers start at once. The `VideoEncoder` trait has no keyframe request
yet ([jstream-platform › Purpose](../jstream-platform/README.md#purpose) lists
on-demand keyframes as not yet), so a video subscriber waits up to one keyframe
interval.

The codec configuration reaches a subscriber through `Subscription::info()`: the
opened `CodecInfo` and the `EncoderSettings` (frame size, frame period, bit rate,
keyframe interval; or the audio input format and bit rate). That is what a muxer's
track header needs: `jstream-mux` builds the AAC `AudioSpecificConfig` from the sample
rate and channel count, and takes the H.264 `AVCDecoderConfigurationRecord` from the
SPS and PPS of the first keyframe, which carries them in-band (Annex B). See
[Open questions](#open-questions) on whether every keyframe does.

### Ending a session

- **`finish()`** refuses further input, encodes what is still queued, calls the
  encoder's `finish` and publishes the tail, then ends every subscription with
  `StreamEnd::Finished` once its queued packets are read. It blocks until the thread
  has exited, is idempotent, and returns the final `SessionStats` or the error that
  stopped the session.
- **Errors.** Any encoder error other than `Busy` stops the session. Subscribers
  receive the packets already published, then `StreamEnd::Failed(EncodeError::Encoder)`;
  `finish` returns the same error; `push` returns `Closed`.
- **Panics** on the encode thread, in the encoder or here, are caught at the thread
  boundary and reported the same way, as `EncodeError::Panicked` with the message.
- **Dropping** a session without finishing it aborts: the thread stops without
  draining the encoder, subscribers get `StreamEnd::Closed`, and the drop joins the
  thread, so no thread outlives its session. The encoder is dropped on the encode
  thread.
- The thread records how the session ended before it drops any subscriber queue, so a
  subscriber that sees its queue close always finds the reason.

### Sharing

`SessionRegistry<K, S>` maps a key to a weakly held session: `get_or_open` returns the
live session for the key or opens and registers one. `SessionKey<M>` is the key ARCH
describes, `(mix, settings)`, with `M` the engine's mix or bus ID and
`EncoderSettings` standing in for the encoder profile the protocol doesn't have yet.
The registry isn't synchronised; the engine keeps it on the control plane. A session
lives while an output holds its `Arc`; the output that lets go last should `finish` it
(it can tell with `Arc::into_inner`).

### Stats

`SessionStats`: inputs `received`, `dropped_queue_full`, `dropped_encoder_busy`,
`encoded`, `packets`, `keyframes`, current `subscribers`, `subscriber_drops` and
`silence_frames`. `SubscriberStats`: `delivered` and `dropped`. All are relaxed atomic
counters read without stopping the thread; reporting them in `stats.tick`
([R-RT-004](../../docs/RULES.md#R-RT-004)) is the engine's job once that event exists.

### Not yet

- **Rate control** beyond what the platform encoder does with the configured
  `RateControl`: no bitrate adaptation or reconfiguration of a running session.
- **Encoder profiles** in the protocol ([R-ARCH-001](../../docs/RULES.md#R-ARCH-001)):
  sessions are configured with platform configs directly until the protocol defines
  them.
- **GPU frames**: sessions pass `VideoSurface`s through untouched, so they work once the
  encoders take GPU input; nothing here depends on the CPU path.
- `tracing` spans ([R-RS-005](../../docs/RULES.md#R-RS-005)): `tracing` is not a
  workspace dependency yet; the crate reports through its counters and `StreamEnd`.
- Performance: not measured ([R-RT-005](../../docs/RULES.md#R-RT-005)).

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

1. **Keyframe on demand.** A late video subscriber (an output started while a shared
   session runs, or an RTMP reconnect in M1d) waits up to one keyframe interval, two
   seconds in spike S1's setup. A `request_keyframe` on `VideoEncoder` (Media
   Foundation's `CODECAPI_AVEncVideoForceKeyFrame`) would let the session ask for one
   when a subscriber joins or resynchronises. That is a change to `jstream-platform`,
   the owner of the encoder traits.
2. **H.264 parameter sets.** The session relies on each keyframe carrying SPS and PPS
   in-band, which `jstream-mux` needs from the first packet a subscriber gets. Whether
   every Media Foundation hardware encoder repeats them on every IDR frame has not
   been checked on hardware. If one doesn't, the platform should expose the encoder's
   sequence header (`MF_MT_MPEG_SEQUENCE_HEADER`) as part of the stream info, or the
   session should cache the last parameter sets and prepend them for a joining
   subscriber.
3. **Drop newest or oldest.** The input queue drops the newest input, which is what
   `try_send` gives and keeps the oldest frames flowing. Dropping the oldest instead
   would lower latency under sustained overload; it needs a different queue.
4. **Hand-off cost.** The input queue is `std`'s bounded channel. `try_send` never
   waits for room, but it takes a brief internal lock to wake a receiver that is
   waiting, and `std` documents no bound on that. If the render thread's budget needs
   a strictly lock-free push, a fixed ring is the replacement.
