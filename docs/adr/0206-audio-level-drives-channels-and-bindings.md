<!-- @doc
id = "ADR-0206"
title = "An audio level drives channels and value bindings"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0206: An audio level drives channels and value bindings

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An `audio` motion modifier moves an item by what the program mix, a bus or a source
sounds like ([ADR-0100](0100-audio-reactive-motion-modifiers.md)). The render thread's
follower measures each heard scope once a tick and runs each modifier's attack and
release envelope. A channel's inputs ([ADR-0183](0183-virtual-control-channels.md)) and a
value binding's input ([ADR-0178](0178-value-bindings-drive-any-number.md)) read only a
`midi` control, a `data` field or a `channel`. So a level can't drive a filter's setting,
a fader, a camera or anything else through a channel, and the mapping design for
alpha.12 lists an audio level as a source beside JTunnel Mods, MIDI and channels. The
maintainer asked for it.

<a id="decision"></a>
## Decision
1. **`audio` is an input.** A channel's `inputs` and a value binding's input may be an
   `audio` kind, with the parameters an item's has: what it hears (`audio_source`, else
   the `audio` bus, else the program mix), its measure (level, peak, onset or a band),
   `floor_db` and `ceiling_db`, `attack_ms` and `release_ms`, its curve, and `from` and
   `to`, which map it onto the channel's 0 to 1 or the setting's range. A minor protocol
   version, 0.178 ([R-API-001](../RULES.md#R-API-001)). A camera's motion still refuses
   `audio` ([ADR-0176](0176-controllers-and-live-data-drive-scene-cameras.md)); a camera
   follows a channel that hears it instead.
2. **The values thread hears it.** The scopes the engine keeps for audio motion
   (`audio::heard_by_motion`) also count each channel's and value binding's `audio`
   inputs, so the mixer feeds those buses and sources. The `jstream-values` thread reads
   the same scopes. Each audio input has a meter of its own: every tick it measures its
   scope's latest window (the FFT only for a band) and advances its envelope by the
   tick's time, exactly as the render thread's follower does for an item. The envelope
   is the input's smoothing: the input's mapping adds none of its own.
3. **It takes part as other inputs do.** As under ADR-0178, an input drives once it
   reads something other than it read when the engine started following it: silence at
   first, so it drives once a sound arrives. While the level moves, the thread ticks at
   a frame's pace ([ADR-0190](0190-moving-values-tick-at-frame-pace.md)).
4. **A scope that arrives late is found.** An input whose scope isn't held yet reads
   silence and is looked up again on each tick, and one whose scope the collection has
   replaced (a bus removed and added again) is followed afresh.

<a id="consequences"></a>
## Consequences
- **Easier:** one level drives any number of settings, faders, lights and cameras
  through a channel, with one attack and release, and a channel's Smooth and fades on
  top. A sequence starts when the music gets loud, through a channel trigger.
- **Harder:** a channel or binding hearing a sound ticks the values thread about 60
  times a second while there is sound, and measures one 2048-sample window per input on
  each tick. Two inputs hearing the same scope measure it twice; that costs well under a
  millisecond, so they aren't shared.
- **Harder:** the values thread measures on its own clock, so an item's audio modifier
  and a channel hearing the same scope can differ by up to one tick of the values
  thread.
- **Not decided here:** an audio level as a binding's trigger (a level crossing), which
  a channel trigger on a sequence covers for now.
