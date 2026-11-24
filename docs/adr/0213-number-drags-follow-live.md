<!-- @doc
id = "ADR-0213"
title = "Filter settings, opacity and gains are live values, so every number drag follows live"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0213: Filter settings, opacity and gains are live values, so every number drag follows live

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0032](0032-live-edits-and-undo.md), whose list of live-adjustable parameters
named "audio gains and sends (faders), when their edits exist" and "fields of source,
filter and effect settings that their kind descriptor marks live", and
[ADR-0091](0091-source-settings-as-live-values.md) decision 2, which said a setting value
drawn through a copy of the collection "is not meant for 60 Hz drags". It adds edits to an
existing action and promises nothing outside the protocol's minor-version rule, so it is
agent tier ([R-GOV-005](../RULES.md#R-GOV-005)).

<a id="context"></a>
## Context
Every numeric input in the UI is the shared click-drag number field (#1734). A drag hands
its value on once per animation frame, and where the engine takes the value as a live
value (an item's placement and crop, a source's `live` settings, a scene's camera) the
output follows the drag and the release commits it as one undo step. The maintainer wants
every click-drag number to follow live in the same way, including filter and effect
parameters, an item's opacity, mixer sends, and channel and bus gains.

`live.set` refuses those edits (`is_live_adjustable`), so the UI sends them once, on
release. Sending a batch on every frame instead would make each frame an undo step, and a
client's history holds 100, so one drag would wipe it.

The engine already draws and hears values that are not stored. A gesture's source
settings (ADR-0091) and a value binding's target (ADR-0178) become setter edits that
`Graph::with_values` applies to a copy of the collection, the "shown" graph. The live
sources, the render plan, the mixer's audio sources and the bus routing all follow that
copy. ADR-0178 already drives filter settings, source and bus faders and sends through it,
about 30 times a second, and 60 while a value moves (ADR-0190).

<a id="decision"></a>
## Decision
1. **More setters are live-adjustable** in `live.set` (protocol 0.178):
   `filter.set_settings`, for the parameters the filter kind's descriptor marks `live`,
   with any other parameter refused at `values.<n>.patch.<param>` as for a source's
   settings; `item.set_opacity`; and the gains: `source.set_audio`, `bus.set_fader` and
   `send.set`. A `source.set_audio` or `bus.set_fader` value sets only the gain: it must
   carry `gain_db` and nothing else, so turning a source's audio on or off, when it is
   heard, and muting stay edits sent in a batch. Otherwise `values.<n>.<field>` is
   `invalid_argument` (`jstream_protocol::live_problem`).
2. **One parameter each.** Each setting of each filter (by owner and filter ID), each
   item's opacity (an item's own, or a keyframe scene's override of it, as for placement),
   each source's fader, each bus's fader and each send are parameters of their own in the
   gesture table. A later value replaces the earlier one, another gesture takes one over,
   and a batch that removes the filter, item, keyframe scene, source, bus or send drops
   its values.
3. **Drawn and heard through the shown graph.** Like a source's settings, these values
   don't go on the render parameter channel: while a gesture holds one, the engine draws
   and hears the collection with it applied (`Graph::with_values`), under the value
   bindings' edits. The render snapshot, the live sources, the mixer's sources and the
   bus routing follow it, so a fader or a send changes what is heard, and a filter
   setting or an opacity what is drawn. A committed value is in the collection, so it
   doesn't wait for the render thread as an item's placement does.
4. **Drags are what these values are for.** The UI sends a drag's values once per
   animation frame, with one `live.set` in flight per gesture and the newest values sent
   when its answer comes. ADR-0091's "not meant for 60 Hz drags" no longer holds: each
   value rebuilds the shown graph and a render plan, as a moving value binding already
   does at that rate.
5. **The UI.** A filter's form (the Filters panel's rows and the Filters window), the
   Inspector's opacity field and slider, and the mixer's gain fields, faders and send
   fields open a gesture with a drag's first value and send each frame's value as its
   `live.set`. The release sends one batch with the final value and `ends_gesture`, one
   undo step. A drag lost before its release, or one released where it started, cancels
   the gesture. A filter parameter its kind doesn't mark `live` is still sent once, on
   release.

<a id="consequences"></a>
## Consequences
- **Easier:** every number a person drags follows on output or in the mix, and a drag is
  one undo step whatever it changes. Agents get the same live values over the protocol.
- **Harder:** each frame of such a drag clones the collection and replans, as a moving
  value binding does. A collection large enough that this costs a frame's time would need
  a parameter channel for these values, which ADR-0091 named as the next step.
- **Harder:** a fader or a send is heard as the mixer follows the shown graph, which is
  the same path a stored change takes, not a sample-accurate ramp: a fast drag steps at
  the rate the values arrive.
- **Must do:** the mock engine takes the same values, with the same gain-only check, for
  the UI's end-to-end tests; it describes no kinds, so it doesn't check `live` flags.
