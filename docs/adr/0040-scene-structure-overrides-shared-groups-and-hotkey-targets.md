<!-- @doc
id = "ADR-0040"
title = "Scene structure: orphaned overrides, shared groups and deleted hotkey targets"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0040: Scene structure: orphaned overrides, shared groups and deleted hotkey targets

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0024](0024-scene-structure.md) makes groups scenes with `role = group`, keyframe
scenes variants that store per-item `overrides`, and hotkey bindings collection state.
The review of ADR-0024 left three questions that its text doesn't answer. Issue #94
records them:

- **N2, overrides that leave the tree.** A variant's override may name an item of its
  parent, or of a group placed in the parent at any depth (ADR-0024 §2). Some edits take
  such an item out of that tree without moving it: `scene.set_role { G, scene }` on a
  group G whose children a variant overrides, removing the item that places G,
  retargeting that item with `item.set_source`, and `scene.set_role` on a variant.
  ADR-0024 says what happens for `item.move` and `item.remove` (the overrides are dropped
  and the inverse restores them), but not for these edits. They could be rejected
  instead.
- **N3, a group placed twice.** Overrides are keyed by item ID. A group placed twice in
  the same parent has one set of children, so a variant's override on one of them
  applies to both placements. Either that is intended, or one copy needs a way to be
  overridden on its own.
- **N4, a hotkey whose target is deleted.** A binding stores a protocol action, which may
  name a scene, a source or an output (ADR-0024 §5). When that object is deleted, the
  binding could block the delete, stay and be reported as broken, or go with its target.

The maintainer decided all three, relayed on #94 with each option as chosen:
- N4, **"Remove with target"**: deleting a scene, source or output also removes the
  hotkey bindings that name it, in the same undoable edit, so undo restores both.
- N2, **"Drop overrides, undoable"**: the edit succeeds and drops the overrides, and its
  inverse restores them, as `item.move` does. The same applies when the item placing G
  is removed or retargeted with `item.set_source`, and to `scene.set_role` on a variant.
- N3, **"Intended; document it"**: a group is one shared source, so an override on its
  child applies to every placement. To vary one copy, duplicate the group.

An earlier relay on #94 recorded N4 as "keep the binding and mark it broken", and
[ARCH › Data model](../ARCHITECTURE.md#data-model) was written from it. No ADR recorded
it and nothing is built, so this ADR replaces that text without superseding anything.
The N2 and N3 text in ARCH already matches the decisions here.

This is a Tier-1 decision ([ADR-0017](0017-adr-decision-tiers.md)): each answer is a
product behaviour the user sees (what a delete takes with it, what an override reaches).
It extends ADR-0024 and contradicts no accepted ADR, so it supersedes none. It builds on
[ADR-0013](0013-command-model.md) (edits, inverses and per-client undo) and
[ADR-0025](0025-typed-settings-kind-descriptors.md) (delete rejection and `delete.plan`).

<a id="decision"></a>
## Decision

**1. Overrides that leave their variant's tree are dropped, never refused (N2).**
- An edit that leaves an override naming an item outside its variant's tree (the
  variant's parent and the groups placed in it, at any depth) succeeds. It drops those
  overrides in the same edit, in every variant they belong to. Its inverse restores them,
  exactly as `item.move` and `item.remove` do under ADR-0024 §2.
- This covers, at least:
  - `scene.set_role { G, scene }` on a group G: G's children leave every tree they were
    in through G;
  - `item.remove` of the item that places G, and `item.set_source` that retargets it to
    another source;
  - `scene.set_role` on a variant;
  - any later edit that can take an item out of a variant's tree.
- The overrides an edit drops are part of its result, so a client can say what was lost
  ("this clears 3 keyframe overrides").
- An override is dropped only when no path reaches its item. If G is still placed in the
  parent's tree some other way, the overrides on its children stay.

**2. An override follows a group everywhere it is placed (N3).**
- This is intended. A group is one shared scene, placed through a `scene` source, and
  overrides are keyed by item ID. A variant's override on a group's child therefore
  applies to every placement of that group in the variant's tree: two placements in the
  same parent, or placements at different depths.
- To vary one copy, duplicate the group (a new group scene with new item IDs) and place
  the duplicate. Overrides keyed by placement path are not added.

**3. Deleting a hotkey's target removes the bindings that name it (N4).**
- `scene.delete`, `source.delete` and the output delete edit also remove every hotkey
  binding whose action names the deleted object, in the same edit. The inverse restores
  the object and the bindings, at their chords, with their actions unchanged. Deleting a
  variant is deleting a scene.
- A binding "names" an object when any field of its stored action is that object's ID,
  as the action schema types it.
- Hotkey bindings are **not delete blockers**. ADR-0025's rule (a delete is rejected
  while anything refers to the target, and `delete.plan` builds the batch that clears
  the blockers) is unchanged for every reference it covers. A binding is not one of
  them: it goes with its target, as a variant's overrides go with their item.
  `delete.plan` lists the bindings its delete will remove, so the client can show them
  ("this also removes the hotkey Ctrl+1").
- Undo follows ADR-0013: if another client has since bound one of the restored chords,
  the inverse conflicts and the undo fails, rather than overwriting that binding.
- No binding is ever left pointing at a missing object, so no "broken binding" state is
  needed.
- Other objects an action may name (a transition preset, a bus) are not decided here.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - no edit in the N2 list fails because of overrides, and one undo brings everything
    back;
  - the hotkey list never shows dead bindings, and needs no "broken" state in the
    protocol, the UI or the collection file;
  - a group means one thing everywhere it is placed, which keeps the override model
    keyed by item ID.
- **Harder:**
  - the inverses of `scene.set_role`, `item.set_source`, `item.remove` and the delete
    edits carry the dropped overrides and removed bindings, so they grow with what was
    dropped;
  - finding which overrides an edit orphans needs the reachability of each variant's
    tree, computed in the graph next to the cycle check;
  - a user who deletes a scene by mistake and doesn't undo loses its hotkeys. A
    re-created scene gets new IDs and needs new bindings.
  - varying one copy of a group costs a duplicate group.
- **Implementation (with ADR-0024's follow-ups, M1b and later):**
  - `jstream-graph`: one function that, given an edit, returns the overrides it
    orphans; the edits above drop them and record them in their inverses. Property
    tests: every edit in the N2 list followed by its inverse leaves the collection
    unchanged, and no override ever names an item outside its variant's tree;
  - the delete edits remove the bindings that name their target and restore them in the
    inverse, with a test per target type (scene, variant, source, output);
  - `delete.plan` reports the bindings a delete removes, as an additive field
    ([R-API-001](../RULES.md#R-API-001)), with a schema snapshot update
    ([R-TEST-002](../RULES.md#R-TEST-002));
  - the edit result reports dropped overrides and removed bindings, also additively.
- **Follow-on doc edits when this ADR is accepted** (not made while it is proposed):
  - [ARCH › Data model](../ARCHITECTURE.md#data-model): the hotkey sentence of the
    "Folders and organisation" bullet says that deleting a binding's target removes the
    binding in the same edit and undo restores both, replacing "the binding stays, is
    reported as broken"; the "Orphaned overrides" and "An override follows a group"
    bullets cite this ADR; `depends_on` gains this ADR.
- **Rejected alternatives:**
  - **N4, reject the delete while a binding names the target** (a blocker, like other
    references). Every scene with a hotkey would need its binding cleared first, for no
    gain: a binding without its target is useless.
  - **N4, keep the binding and mark it broken** (the earlier relay). It keeps a chord the
    user may want to re-point, but leaves dead entries in every client and a broken state
    to model, report and save.
  - **N2, reject the edit while overrides would be orphaned.** It forces the user to
    clear keyframe overrides by hand before a role change or a retarget, and ADR-0024
    already drops and restores them for `item.move` and `item.remove`.
  - **N3, overrides keyed by placement path** (variant, item placing the group, child).
    It would let one copy be overridden alone, but every override, `item.move` and the
    grouping batch would have to rewrite paths, and moving a group would orphan them.
