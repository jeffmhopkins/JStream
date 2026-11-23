<!-- @doc
id = "ADR-0186"
title = "A sky dropping out of warp eases out toward rest as it does toward a cruise"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0186: A sky dropping out of warp eases out toward rest as it does toward a cruise

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0167](0167-a-sky-backdrop-for-3d-scenes.md) decision 5 ("Dropping out") says how
much the warp is dropping: fully while it is above its setting, easing out over the last
0.15 toward a setting above 0. A drop to 0 stays full to the end, since the nebulae's
zoom falls to none as the warp reaches 0 anyway.

That held while a setting only jumped. A sky's `warp` can now follow a motion modifier,
such as an `audio` envelope, so its setting moves from frame to frame. When a setting
just above 0 decays to exactly 0 while the warp is within 0.15 of it, the amount jumps
from the eased value to full in one frame, and the nebulae's zoom, which it multiplies,
pops each time the music goes quiet.

<a id="decision"></a>
## Decision
ADR-0167's decisions stand, with this change: the warp eases out of dropping over the
last 0.15 toward any setting below it, 0 included. How much it is dropping is
`(warp - setting) / 0.15`, at most 1, and 0 at or below its setting. It is now continuous
in the setting, so a setting that moves moves it a little at a time.

<a id="consequences"></a>
## Consequences
- A sky driven by a modifier no longer pops its nebulae as the setting reaches 0.
- A full drop to rest looks almost the same: over its last 0.15 the zoom, already
  shrinking with the warp, shrinks a little sooner, and the deep sky blends toward the
  way it shows going into warp, which at that warp is close to fully shown too.
