<!-- @doc
id = "ADR-0212"
title = "The Show panel's Edit view ruler moves the play head; the Run view's strip arms"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0212: The Show panel's Edit view ruler moves the play head; the Run view's strip arms

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0210](0210-shows-a-run-order-with-bookmarks.md) decision 13 says that while a show
runs, a click on the ruler arms a spot for the next Continue (`show.arm`) instead of
jumping program, so a stray click can't cut on air. Testing the Show panel, the maintainer
asked for the timeline to behave like a transport: a click on the Edit view's ruler moves
the play head there, and the play head can be dragged to scrub. `show.seek` already exists
for scrubbing and agents.

<a id="decision"></a>
## Decision
This narrows ADR-0210 decision 13 to the Run view. The rest of that decision stands.

- **Edit view.** A click or a drag on the ruler, or a drag of the play head, moves the play
  head. While a show runs it sends `show.seek`: at the click, then at most about every
  80 ms during a drag, and once more where the pointer comes up. While no show runs it
  sets where Play starts, which is the panel's own state. Play sends `show.play`, then
  `show.seek` to that spot. Snapping applies as in any drag, and Shift turns it off. With
  the Loop or Bookmark tool, the ruler keeps its own use.
- **Run view.** The view for show time keeps decision 13's guard: a click on the folded
  strip arms a spot (`show.arm`) and never jumps program. A bookmark's button or key still
  jumps at once.

<a id="consequences"></a>
## Consequences
- Editing during a live show can now jump program from the ruler. The Run view is the
  view to run a show from. Rehearse runs on preview, where a seek is harmless.
- No protocol change: `show.seek` and `show.arm` are as ADR-0210 defined them.
- Pausing anywhere, which the maintainer also asked for, is not covered here. It needs an
  action the protocol doesn't have yet.
