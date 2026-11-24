<!-- @doc
id = "ADR-0212"
title = "A timer starts its count again when the program shows it again"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0212: A timer starts its count again when the program shows it again

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Issue #1736.

A `timer` source's countdown or stopwatch counts on the media clock from its time zero,
which is when the engine first built it. The engine keeps that zero through every
rebuild that keeps its time (`SourceKind::keeps_time`: its look, its density), so a
countdown placed on a scene runs once, from when the collection loaded, and has long
ended by the time a person switches to that scene. The only ways to run it again were
an edit to its `restart` counter (#1040) or to `mode` or `duration`, and
`source.restart` ([ADR-0115](0115-timelines-on-the-wire.md)) did nothing for a timer.

A `media` file already starts again when the program shows it again
(`restart_on_show`, ADR-0115), and a show that plays from a scene already on program
needs an action it can call to start that scene's countdowns.

<a id="decision"></a>
## Decision
1. A `timer` countdown or stopwatch starts its count again whenever the program shows
   it again after plans that didn't reach it: its scene goes on program, or its item
   is shown. This is the moment ADR-0115's `restart_on_show` uses. It has no setting:
   a timer that stays on program across a switch (on both scenes, or in a nested
   scene on both) keeps counting. A clock shows the wall clock, so it has nothing to
   restart.
2. `source.restart` starts a timer's count again, and `source.seek` counts it on as if
   it had started `at_ms` ago. A seek before the show holds through it, as for media.
   Hotkeys, MIDI and other bindings, timeline and sequence cues, and plugins already
   reach both through the actions they bind, so a show runner calls `source.restart`
   for the countdowns of the scene on program when it starts playing.
3. The engine restarts a timer by moving its time zero to the restart's time, both
   for the running instance and for any build in flight that keeps it, and paints it at
   once. A later rebuild for its look keeps the restarted count. A source says it
   counts this way through `Source::counts_from_zero`. The engine then calls none of
   its `show`, `restart` or `seek`.
4. The protocol minor version goes up for the new behaviour of `source.restart` and
   `source.seek`.

<a id="consequences"></a>
## Consequences
A countdown on a scene now counts from when the scene goes on program, every time,
and a stopwatch counts the time the scene has been on program. A collection that
relied on a timer counting on while its scene was off program now restarts it on
each show. To keep a count running across scenes, place it on each of them, or in a
nested scene they share. The `restart` counter still works for an edit-driven restart.
