<!-- @doc
id = "ADR-0091"
title = "A source's live settings are live values, so a Properties dialog applies as you edit"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0091: A source's live settings are live values, so a Properties dialog applies as you edit

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The Properties dialog edits a draft and sends it on OK, as
[ADR-0032](0032-live-edits-and-undo.md) says a dialog does ("A dialog that edits a draft
and applies it on OK needs no live values at all"). The canvas therefore shows a new
size, color or text only after OK. OBS applies as you edit and puts everything back on
Cancel, and the maintainer chose that behaviour for JStream, with one undo step per
dialog.

Sending each change as a batch would apply it at once, but each batch is one undo step,
and Cancel would need one more batch to put things back. ADR-0032's live values give
exactly what the dialog needs: a value shown at once, one batch on OK (`ends_gesture`)
that is one undo step, and `live.cancel` that leaves no trace. ADR-0032 lists "fields of
source, filter and effect settings that their kind descriptor marks live, once kind
descriptors exist" among the live-adjustable parameters, but no edit carries them yet:
`is_live_adjustable` accepts only item placement and crop.

Item values reach the render thread through a parameter channel, with no snapshot
rebuild. A source's settings reach its runtime source (the text drawn, the color
filled) through the collection state that the live sources follow
(`LiveSources::sync`) and that the render plan reads. There is no per-parameter channel
for them.

<a id="decision"></a>
## Decision
1. **`source.set_settings` is live-adjustable** in `live.set`, for the parameters its
   kind's descriptor marks `live` (ADR-0025). A value that names any other parameter
   (a device, a file, a URL) is refused with `invalid_argument` at
   `values.<n>.patch.<param>`, and nothing in the message is applied. Each parameter of
   each source is a parameter of its own in the gesture table: a later value for it
   replaces the earlier one, and another gesture's value for it takes it over, as for
   items. A gesture's values for a source are dropped when a batch deletes the source.
2. **The engine draws the collection with the open gestures' setting values applied.**
   While any gesture holds a source setting, the engine keeps a copy of the collection
   with those values applied and uses it, instead of the collection, for the live
   sources, the audio mix and the render plan. The copy is rebuilt when a gesture's
   setting values change, when a gesture ends, and after every applied batch, undo and
   redo. It is never `rev`, never an undo step, never saved, and no collection read
   shows it, as for every live value. Unlike an item value, a setting value rebuilds the
   render snapshot. That is fine at the rate a person edits a dialog; it is not meant for
   60 Hz drags.
3. **Commit and cancel are ADR-0032's.** The committing batch carries the final values
   and `ends_gesture`. It is one `rev` bump and one undo step, whose inverse restores the
   values from before the gesture. `live.cancel` drops the values, and each setting shows
   its collection value again.
4. **The Properties dialog** opens a gesture on its first change to a live setting and
   sends each live setting the user changes since it opened as a live value. Settings
   that aren't live stay in its draft (the device preview shows them, DL-02). OK sends
   one batch with the name, every changed setting and `ends_gesture`. Cancel, Escape
   and the backdrop send `live.cancel`. A filter change in the dialog still applies at
   once, as before.

<a id="consequences"></a>
## Consequences
- The canvas shows a source's live settings as they are edited, and OK is one undo
  step. Cancel leaves the collection, the history and other clients untouched, apart
  from the `live.changed` and `live.ended` events.
- Other clients see the values through `live.changed`, as for a drag. Their collection
  reads show the committed values, so a second Properties dialog on the same source
  opens on those.
- Each live setting value costs a copy of the collection and a snapshot rebuild. A
  dialog sends at human rate, so this is small. If a faster sender ever needs setting
  values, a parameter channel for them is the next step.
- A source whose runtime is rebuilt when its settings change (a text source parses its
  font and lays out again) does that work on each live value. That is the same work an
  ordinary edit does.
- The mock engine, which describes no kinds, accepts a source's settings as live values
  without checking the `live` flags, and shows them in `live.list` and `live.changed`
  as the engine does. The flag check is the engine's, tested there.
