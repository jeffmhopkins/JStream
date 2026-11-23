<!-- @doc
id = "ADR-0208"
title = "A key chord or a controller trigger runs several actions, and output.toggle"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0208: A key chord or a controller trigger runs several actions, and output.toggle

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0024](0024-scene-structure.md) (scene hotkeys) and
[ADR-0122](0122-controller-input-midi-and-one-binding-system.md) decision 1 (controller
bindings), which allowed one action per chord and per trigger. It is an additive protocol
change (0.182) with no new data kind, so it is agent tier.

<a id="context"></a>
## Context
A show needs one key or pad to do several things at once: go live on two services, switch
to the opening scene and start its sequence. The engine refused a second action on a bound
chord or trigger (`already_exists`), so a client could only reach several targets through
a channel or a sequence it made for the purpose, which a person then sees as an object they
never asked for. Going live is also two actions today, `output.start` and `output.stop`, so
one key couldn't start the stream and, pressed again, end it.

<a id="decision"></a>
## Decision
1. **Several actions per chord and per trigger.** The collection keeps, for each chord and
   each controller trigger, a list of actions in the order they were bound. A press runs
   every one of them, in that order; one failing doesn't stop the rest. `hotkey.set` and
   `binding.set` on a chord or trigger already bound add the action after its others, and
   refuse with `already_exists` only an action equal to one it runs already.
2. **Clearing one action.** `hotkey.clear` and `binding.clear` take an optional `action`:
   with it, they unbind that one action (`not_found` when the chord or trigger doesn't run
   it); without it, every action, as before. A chord's labels (ADR-0133) belong to the
   chord and go with its last action.
3. **Exact inverses.** The inverse of `hotkey.set` is `hotkey.clear` naming the action.
   The inverse of a clear, and of a delete that removed some of a chord's actions, unbinds
   the actions the chord kept after the first removed one and binds every removed one and
   those again, in order, so undo restores the order exactly. It never clears an action
   bound since, which stays bound beside the restored ones.
4. **Deletes take only what names the object.** Deleting a scene, output, item, timeline or
   channel removes the actions that name it (ADR-0040 §3) and leaves the chord's or
   trigger's other actions bound.
5. **The wire keeps its shape.** `state.get`'s `hotkeys` and `bindings`, the collection
   file and `delete.plan` list one `{chord, action}` or `{trigger, action}` entry per
   action, in chord or trigger order and then bind order, so a chord can now appear more
   than once. Each of a chord's entries carries its labels. A client that registers chords
   with the OS registers each chord once and runs every entry's action. The collection
   file's schema version is unchanged: an older file loads as before, and a newer one
   naming a chord twice is refused by an older engine with `already_exists`, as any
   collection it can't rebuild is.
6. **`output.toggle`.** A new action starts an output that is stopped or failed and stops
   one that is starting, running, reconnecting or stopping. The engine runs it as the
   `output.start` or `output.stop` it resolves to, with that action's events, audit entry
   and errors, so nothing downstream sees a new kind of change. It is bindable, names its
   output, and is what a "Go live / End stream" or "Record / Stop recording" key binds, one
   per output.

<a id="consequences"></a>
## Consequences
- The GUI binds any action to a key or MIDI trigger, and several to one, from one place
  (Settings › Keys & MIDI), without making channels or sequences behind the person's back.
- Rebinding a chord to one action is still a batch of `hotkey.clear` (no `action`) then
  `hotkey.set`; a client that relied on `already_exists` to stop a second binding must now
  check what the chord runs first.
- The engine's MIDI runner and the desktop app's global listener run every action of a
  matched trigger or chord in order, and a held trigger's release (ADR-0207) releases each.
- A data trigger (ADR-0202) gets several actions per trigger with no more protocol.
