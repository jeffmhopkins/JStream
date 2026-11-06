<!-- @doc
id = "ADR-0119"
title = "The base frame rate stays 30 fps, and 60 fps is an option"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0119: The base frame rate stays 30 fps, and 60 fps is an option

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0014](0014-media-clock.md) fixes one base video frame rate, and every mix runs at
that rate divided by an integer. The engine shipped that rate as 30 fps. The alpha.4
Windows run could not ask the program to hold 60 fps at 1080p, because nothing could
change the rate. The maintainer decided the default stays 30 fps, and 60 fps is an
option a collection chooses.

<a id="decision"></a>
## Decision
A collection has one base frame rate, 30 or 60 frames a second. A new collection is
30. `canvas.set_frame_rate { fps }` (protocol 0.89) sets it. Anything else is
`invalid_argument`. It is collection state: a `rev`, an undo step, and the collection
file when it is not 30, so a file at 30 saves as it did before.

The program stage reads the rate when it opens and keeps it until it stops. A change
while the stage runs shows in `state.get` at once and takes effect the next time the
stage opens, as a mix's `fps_divisor` does. `fps_divisor` stays relative to the base
rate, so choosing 60 doubles every mix's rate.

The virtual camera still offers and is fed 30 fps in both modes. At 60 it takes every
second base tick. The default encoder profile stays 8000 kbit/s, YouTube's 1080p30
rate; at 60 that budget covers twice the frames.

<a id="consequences"></a>
## Consequences
- ADR-0014 still holds: one base rate at a time, and 30 and 60 share an integer divisor.
- A saved mix does not change rate until the collection opts into 60.
- `state.get` reports `base_fps`, omitted while it is 30, so an older client still
  reads a collection that has not opted in.
