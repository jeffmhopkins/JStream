<!-- @doc
id = "ADR-0133"
title = "Labels on hotkeys and timelines, and a label's members"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0133: Labels on hotkeys and timelines, and a label's members

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A scene with motion has many related parts: its keyframe scenes, the hotkeys that switch
to them, and the timelines that sequence them. Labels are collection state
([ADR-0024](0024-scene-structure.md) §4), and sources and scenes carry them
([ADR-0078](0078-scenes-carry-labels.md)), so keyframe scenes do too. Hotkey bindings
([ADR-0024](0024-scene-structure.md) §5) and timelines
([ADR-0115](0115-timelines-on-the-wire.md)) carry none. There is no way to see
everything that makes up one effect in one place, or to change it together (#1078).

<a id="decision"></a>
## Decision
This extends ADR-0024 §4 and ADR-0115. The rest of both stands.

**1. Bindings carry labels.** `hotkey.set_labels {chord, labels}` sets the labels of the
binding of `chord`, whole. The binding must exist (`not_found` on the hotkey), then each
label (`not_found`), each listed once (`invalid_argument` on `labels`). The engine keeps
them ordered by ID, and an empty list removes them. Its inverse sets the old ones.
`hotkey.set` is unchanged and binds with none, so a client older than this minor still
binds as before. `HotkeyBinding.labels` reports them wherever a binding is reported.
Clearing a binding, or a delete that removes it (ADR-0040 §3, ADR-0101), takes its labels
with it, and the inverse puts both back: the `hotkey.set`, then the `hotkey.set_labels`.

**2. Timelines carry labels.** `TimelineInfo.labels` is part of the timeline, which
`timeline.set` writes whole. It is checked as a binding's labels are, with
`timeline.labels` as the field, and kept ordered by ID.

**3. Deleting a label.** Bindings and timelines carrying it block `label.delete`, as
sources and scenes do, with the field `labels`. `delete.plan` lists the edits that take it
off each: sources, scenes, bindings by chord, then timelines by scene then ID, then the
delete.

**4. A label's members.** The query `label.members {label_id}` returns everything that
carries the label: its scenes in display order, each with its parent when it is a keyframe
scene; its sources by ID; its bindings by chord, each with its action; and its timelines
by scene then ID, each with the scene that holds it. An unknown label is `not_found`. It
is the one call an agent makes to list a label's action group, and it reads nothing
`state.get` doesn't already return.

**5. Editing them together.** No new edit does that. A client changes the move preset of
the label's keyframes with one `scene.set_transitions` per keyframe in one batch, which is
one undo step. The web UI's label manager gains an Actions view per label: its keyframes,
with a preset control for the chosen ones, then every binding and timeline with a
checkbox that puts the label on or takes it off.

**6. Compatibility.** A protocol minor bump: the edit, the query and the two `labels`
fields are additions, each omitted when empty, so an older client ignores them. Writes are whole, though: an older
client's `timeline.set` drops a timeline's labels, and its rebind (a clear, then a
`hotkey.set`) drops a binding's, as a field left out takes its default. The
collection file gains `hotkeys[].labels` and `scenes[].timelines[].labels` at a new
schema version. An older file has neither: a binding's labels in one are refused, as that
version refused them, and a timeline's are dropped, as that version ignored them.

<a id="consequences"></a>
## Consequences
- An agent lists a label's keyframes, hotkeys and timelines in one call, and a person sees
  and edits them in one place.
- Labels mean the same thing everywhere: one set, the same checks, the same delete rule.
- A binding rebound to a new chord from the scene's properties keeps its labels; one
  replaced by a different action on the same chord doesn't, as the labels belonged to the
  old binding.
- Setting a keyframe's per-item timing (ADR-0129) for every chosen keyframe from the
  Actions view is follow-up work once that lands, with the same one-batch rule.
