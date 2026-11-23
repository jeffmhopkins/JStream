<!-- @doc
id = "ADR-0190"
title = "Values tick at a frame's pace while one is moving"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0190: Values tick at a frame's pace while one is moving

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The `jstream-values` thread drives the value bindings
([ADR-0178](0178-value-bindings-drive-any-number.md) decision 4) and the channels
([ADR-0183](0183-virtual-control-channels.md)). ADR-0178 ticks it about 30 times a second,
and [ADR-0184](0184-sequences-lanes-and-triggers.md) rule 7 ticks it at about 60 a second
only while a sequence's lane plays. So a setting a knob drives, through a learned binding
or a channel, moves in steps of about 33 ms: on a 60 fps program every other frame
repeats the last value, and a slow turn of a position or an opacity shows small steps
however its smoothing is set. The maintainer asked that nothing driven by an incoming
control move jerkily, with smoothing short enough not to lag.

<a id="decision"></a>
## Decision
1. **A frame's pace while anything moves.** The thread ticks about every 16.7 ms while a
   lane plays (ADR-0184 rule 7, unchanged), while any channel's value changed on the last
   tick, or while the value bindings' setter edits changed on it: a control turned, or its
   smoothing, a channel's fade or its averaging still settling. Otherwise it ticks about
   every 33.3 ms, as ADR-0178 decision 4 says.
2. **Nothing else changes.** Smoothing stays per input (`smoothing_ms`, the same one-pole
   smoothing on the tick's time), and a channel keeps its averaging and fades, so a faster
   tick changes how often a moving value is drawn, not how fast it moves.

<a id="consequences"></a>
## Consequences
- Smoothed motion from a control reaches the program in frame-sized steps, so the
  smoothing time sets how soft it is and nothing else adds steps.
- The thread does twice the work while a value moves, as it already did while a lane
  plays, and goes back to 30 a second once everything has settled; a resting show costs
  what it did.
- An input that never sits still, such as live data or an audio level feeding a channel,
  keeps the thread at 60 a second, under the engine's lock, for as long as it moves. That
  is the cost of drawing it without steps.
- The first tick after a control moves from rest can still be up to 33 ms away, a delay
  well within a smoothing time.
- A smoothing field edits an input's `smoothing_ms` while its control drives, so the
  engine keeps following an input that changed only its smoothing time: it keeps where it
  rested, whether it moved and its level, and the setting goes on following the control.
