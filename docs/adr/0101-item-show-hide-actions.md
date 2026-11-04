<!-- @doc
id = "ADR-0101"
title = "Item show, hide and toggle actions, with the item's own effects"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0101: Item show, hide and toggle actions, with the item's own effects

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A hotkey runs a protocol action, never an edit, so that it never adds to anyone's undo
history ([ADR-0024 §5](0024-scene-structure.md)). Until now the only action that changed
what program shows was `scene.switch`. Showing or hiding one item is an edit,
`item.set_visible`, so no hotkey could do it (#1057). A scene with several independent
states, each shown by one item, needed a whole scene variant for every combination of
them: three states of two options each make eight variants, when each press only had to
show or hide one item.

An item already says how it appears and disappears without a partner
([ADR-0029 §6](0029-transitions.md): `appear`, `disappear`, `delay_ms`, `easing`), but
only a scene switch plays those effects. An `item.set_visible` cuts. Sources with
`restart_on_show` restart when a plan reaches them again after plans that didn't, which
a scene switch and a visibility edit both cause.

Two places could hold the visibility a hotkey changes: the collection, where
`item.set_visible` keeps it, or a runtime layer over it, as live values are
([ADR-0032](0032-live-edits-and-undo.md)). A runtime layer would give an item two
visibilities: the Sources list, the saved file and every agent's `state.get` would
disagree with the picture.

<a id="decision"></a>
## Decision
1. **New actions `item.show`, `item.hide` and `item.toggle`**, each
   `{ item_id, scene_id? }`, with `item.set_visible`'s `scene_id` rules and errors. The
   field of an error names the action's field (`scene_id`), not `edits.0.scene_id`.
   `item.toggle` reads the placement's visibility first: a keyframe scene's override if
   it has one, else the item's own. Protocol 0.80.
2. **They change the collection.** This extends [ADR-0013](0013-command-model.md)'s split,
   in which only edits change the collection: these are actions that do, through the
   edit they stand for. The engine applies the `item.set_visible` the action stands for:
   `rev` moves on by one, `state.changed` follows (with no `cause`), and the collection
   saves it. A show of a shown item, or a hide of a hidden one, changes nothing and emits
   nothing. As actions they take no `expected_rev` and no idempotency key (R-API-004
   covers edits), so `item.show` and `item.hide` are the safe ones to retry: a retried
   `item.toggle` flips twice.
3. **They are never an undo step**, as ADR-0024 §5 and
   [ADR-0013](0013-command-model.md) require of an action. The change is logged for undo
   conflicts like a batch, so another client's undo that would overwrite it fails with
   `undo_conflict` rather than silently undoing a press. The sender's own undo still
   reaches its last batch.
4. **The item's own effect plays on program.** When the change shows or hides the item
   on program (the program scene's tree holds it, [`tree_items`] of its family, and
   program's own placement of it is the one that changed), the engine runs a Move from what is on screen to
   the program scene, pairing every item with itself ([ADR-0049 §3](0049-keyframe-scene-model-and-evaluation.md) pairing within a
   family). Everything else holds still, and only the changed item appears or
   disappears, with its `appear` or `disappear` effect, after its `delay_ms` and with
   its easing. The duration and easing are the default preset's. `inherit` resolves to
   the default preset's `unmatched`, or to no animation when that preset's kind has none
   (`cut`, `stinger`, a pack's kind). An item that would show or go at once with no delay
   starts no run, and a default preset of 0 ms never animates. A running transition is
   continued from what is on screen, as a switch that retargets it is
   ([ADR-0029 §7](0029-transitions.md)). Scene mixes and previews of other scenes show
   the change at once.
5. **`restart_on_show` follows.** A source the program reaches again because of the
   action is shown again (`Source::show`), so a `media` file or a `browser` page with
   `restart_on_show` restarts, as on a scene switch. The probe that reads what is on
   screen before the change is not a plan of the program, so it reaches nothing.
6. **They can be bound.** `hotkey.set` accepts them; the item, and the scene a binding
   names, must exist. `Action::named_objects` names the item, so any edit that takes an
   item away (`item.remove`, `scene.delete`, `source.delete`) removes the bindings naming
   it in the same edit, and its inverse restores them after the item
   ([ADR-0040 §3](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)).
   `delete.plan` lists them. Plugins get them with the `edit` class, as
   `item.set_visible`. MCP's `item` tool carries them, and they are in `REVERSIBLE_ACTIONS`
   ([ADR-0098](0098-mcp-destructive-hint-for-what-undo-cant-take-back.md)): the opposite
   action brings the picture back, as a `scene.switch` back does, so they don't make it
   destructive. In a keyframe with no visibility override, a hide writes one and the show
   after it leaves it in place: the keyframe no longer follows its parent's visibility of
   the item until `item.clear_override` takes the override away. The web UI binds them
   from an item's Inspector (Hotkeys: Toggle, Show, Hide), naming the keyframe when the
   item is shown in one.

[`tree_items`]: ../../crates/jstream-graph/src/model/mod.rs

<a id="consequences"></a>
## Consequences
- **Easier:** one scene holds independent states, each bound to an item toggle. A timeline cue
  (#1059) can show or hide an item through the same action.
- **Easier:** the Sources list, the saved file and agents always agree with the picture,
  since there is one visibility.
- **Harder:** a toggle on a client's own items isn't in its undo history. Pressing the
  key again is the way back, as with a scene switch.
- **Harder:** the effect's duration is the default preset's, for every item. A per-item
  or per-binding duration would be an additive field later.
- **Harder:** an item inside a nested scene that isn't a group, or in a scene a mix
  shows, changes at once: only the program's own tree plays the effect.
- **Must do:** clients that run hotkeys send these like any action. The mock applies the
  same visibility change and history rule, and plays no effect, since it draws nothing.
