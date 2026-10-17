<!-- @doc
id = "ADR-0024"
title = "Scene structure: groups, keyframe scenes, folders and organisation"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0024: Scene structure: groups, keyframe scenes, folders and organisation

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
M1b builds scenes, scenes as sources, and placing items
([ROADMAP](../../ROADMAP.md#m1)). The chosen UI prototype
([design/ui-prototype](../../design/ui-prototype/README.md#settles), direction D) adds
four structures on top that no doc describes yet. Its
[decision register](../../design/ui-prototype/decision-register.md) (entries D4 to D7 and
D42) analyses them:
- **groups**: "a group is a nested scene". Grouping moves the selected objects into a new
  scene, nested in place, and the group's children are edited inline on the parent
  canvas;
- **keyframe scenes**: "a variant of a scene, nested under it, that may change only
  placement and visibility; everything else follows the parent. It stores only what
  differs". A keyframe switches like a scene, usually with a Move transition;
- **folders**: the Scenes panel is a tree of folders, for organising only;
- **organisation metadata**: labels with icons and colours, favourites, scene icons and
  colours, a scene background colour, "show in multiview", and scene hotkeys.

**Today.**
- Scenes are a flat list in display order, changed by `scene.reorder`. A scene is a name
  and its items in stacking order ([ARCH › Data model](../ARCHITECTURE.md#data-model)).
- A `scene` source nests a scene in another, and the graph rejects an `item.add` that
  would close a cycle.
- Item IDs are unique across all scenes. No edit moves an item to another scene: the
  only way is to remove it and add a new one, which loses its ID.
- Scenes have no size, no role, no parent, no folder and no metadata. ADR-0022 gives the
  collection one canvas, which is every scene's coordinate space.

**The forces:**
- Keyframe overrides and per-item transition settings (register D25) are keyed by item
  ID, so grouping must keep item IDs.
- Inline editing of group children works only if a group shares its parent's coordinate
  space.
- A keyframe that can't be on program, nested or hotkeyed loses the point of the
  prototype's design. A plain duplicate scene loses "everything else follows the
  parent".
- If keyframes are built later, the placement edits must still be **addressed** so that
  they can write a keyframe's override. Changing their addressing after 1.0 would be a
  breaking change ([R-API-001](../RULES.md#R-API-001)).
- [R-ARCH-001](../RULES.md#R-ARCH-001): there are no UI-only features. Folders, labels and
  favourites kept in one client are invisible to agents and to other clients.
- JStream is single-user ([ADR-0006](0006-mcp-full-access.md): full access, one control
  token), so "favourite" is not a per-person setting.

**The maintainer's choices.**
- D4, **"Groups are scenes"**: a group is a scene flagged as a group, hidden from the
  scene list by default, and placed in its parent like any nested scene. There is one
  canvas per collection (ADR-0022). An edit moves items into and out of a group, keeping
  their IDs.
- D5, **"First-class variants"**: a keyframe scene is a real scene. It can be on program,
  nested and hotkeyed. It stores only overrides of its parent's items: placement and
  visibility, plus opacity (ADR-0023). Placement edits address (scene, item); in a
  variant they write an override. The feature is built later, and the addressing is
  fixed now.
- D6, D7 and D42 follow the register's recommendations: folders and scene order in the
  collection, organisation metadata in the collection, the scene background colour in
  the scene model, and hotkey bindings in the collection.

This ADR contradicts no accepted ADR, so it supersedes none. It builds on
[ADR-0013](0013-command-model.md) (edits and their inverses),
[ADR-0007](0007-bus-matrix-audio.md) (sources independent of scenes; outputs consume a
video mix), ADR-0022 (canvas and item geometry) and ADR-0023 (placement fields).

<a id="decision"></a>
## Decision

**1. Groups are scenes.**
- `Scene` gains **`role`**: `scene` (default) or `group`. `scene.create` takes an
  optional `role`, and `scene.set_role { scene_id, role }` changes it (inverse: the same
  edit with the previous role). `scene.list` and `state.get` report it.
- The role changes **no engine behaviour**. A group renders, nests, takes scene filters
  and can be placed more than once, exactly like any scene. The role tells clients,
  agents and switchers what it is for: clients hide groups from the scene list by
  default, and a group is shown under the items that place it.
- A group shares the collection canvas (ADR-0022). A `scene` source's native size is the
  canvas, so ADR-0022's default placement (centred, canvas-sized, `fit`) draws the group
  exactly in place, and its children keep their canvas coordinates. A client may
  therefore edit a group's children inline on the parent canvas. Those edits address the
  child's own scene.
- New edit **`item.move { item_id, scene_id, index }`**. It moves an item to another
  scene at a stacking index, keeping its ID, geometry, crop, visibility, opacity, blend
  and source. Its inverse is `item.move` back to the old scene and index. It is rejected
  with `invalid_argument` if a `scene` source would close a cycle, if the target is a
  keyframe scene (decision 2), or if the index is out of range.
- **Grouping** is one client-built batch: `scene.create` (role `group`), `source.create`
  (kind `scene`), `item.add` of that source in the parent at the lowest selected item's
  index, then `item.move` of each selected item into the group, bottom first.
  **Ungrouping** is the reverse batch. If the group's placement isn't the default, the
  client also rewrites each child's geometry in the same batch, using the shared geometry
  function (ADR-0022).
- **Rendering:** a group is a scene, so it renders to its own texture and its filters,
  opacity and blend apply to the group as a whole. The engine may draw a group's children
  straight into the parent when the result is identical (no scene filters, `normal`
  blend, opacity 1, default placement). That is an optimisation, never a change of
  meaning.

**2. Keyframe scenes are first-class variants.**
- `Scene` gains **`variant_of: Option<SceneId>`**, set when the scene is created and
  never changed. A **variant** (a keyframe scene) has no items of its own. It shows its
  parent's items, with **overrides** stored per item:
  `overrides: ItemId → { geometry?, crop?, visible?, opacity? }`. An absent field follows
  the parent.
- **Invariants:**
  - `variant_of` names an existing scene that is not itself a variant. A keyframe of a
    keyframe is made on the parent.
  - An override names an item of the parent scene, or an item of a group (role `group`)
    placed in the parent, at any depth. So grouping and ungrouping keep overrides, because
    `item.move` keeps the ID. An item that leaves that tree loses its overrides, and the
    inverse restores them.
  - Everything else follows the parent: the items, their sources, stacking order, blend,
    the scene's filters and its background. Any other edit addressed to a variant
    (`item.add`, `item.reorder`, `item.set_source`, `item.set_blend`, filter edits) is
    rejected with `invalid_argument`.
  - A variant is a real scene. It is listed (under its parent), and it can be on
    program or preview, placed through a `scene` source, bound to a hotkey and used by
    filters that take a scene. The cycle check counts the variant → parent edge, so a
    variant can't be nested inside its own parent.
  - Deleting a parent is rejected while it has variants, and the error lists them, like
    every other reference. Removing a parent's item drops its overrides in every variant,
    and `item.remove`'s inverse restores them. Adding an item to the parent shows it in
    every variant at the parent's placement.
- **Addressing, fixed now.** The placement edits (`item.set_geometry`,
  `item.set_transform` while it lasts, `item.set_crop`, `item.set_visible` and
  `item.set_opacity`) gain an optional **`scene_id`**:
  - absent, or the item's own scene: the edit changes the item, as today;
  - a variant whose overrides may name the item: the edit writes that field of the
    variant's override;
  - anything else: `invalid_argument`.

  New edit `item.clear_override { scene_id, item_id, field }` removes one override field.
  It is the inverse of writing a field that was absent; writing a field that was present
  has the same edit with the old value as its inverse. "Reset keyframe" is a batch of
  clears.
- **Built later.** Until variants are built, the engine accepts `scene_id` only when it
  names the item's own scene, and `variant_of` is not yet a field. Variants then arrive
  as additive changes: a new optional field and new edits, no change of meaning.

**3. Folders and scene order.**
- Scenes keep their **flat display order**, and `scene.reorder` keeps its meaning.
- The collection gains an ordered list of **folders** `{ id, name, parent? }`, and
  `Scene` gains `folder: Option<FolderId>`. A folder's scenes are the scenes that name
  it, in display order. Folders are for organising only: they change no rendering,
  switching or nesting.
- New edits: `folder.create`, `folder.rename`, `folder.set_parent`, `folder.delete`,
  `folder.reorder` and `scene.set_folder`, each with the obvious inverse.
  `folder.delete` is rejected while a scene or a folder names the folder.
- Folders nest through the optional `parent`, so the folders form a tree: a folder's
  children show in folder order, and `folder.create` and `folder.set_parent` reject a
  parent that doesn't exist or that would put the folder inside itself. A folder with
  no parent is top level, so a collection with flat folders reads as before.
- Variants sit under their parent through `variant_of`, never through folders:
  `scene.set_folder` on a variant is rejected. Groups may sit in folders like any scene.

**4. Organisation metadata is collection state.** It is edited by undoable edits, seen by
every client and every agent, and saved with the collection.
- **Labels:** an ordered list `{ id, name, icon, color }`, edited by `label.create`,
  `label.update`, `label.delete` and `label.reorder`. `label.delete` is rejected while
  anything carries the label.
- **Sources** carry `labels: set of LabelId` and `favourite: bool`, edited by
  `source.set_meta`. They belong to the source (ADR-0023), so references share them, and
  `source.duplicate` copies them to a clone.
- **Scenes** carry `icon`, `color`, `favourite` and `show_in_multiview` (default `true`),
  edited by `scene.set_meta`. A variant has its own.
- `icon` is an opaque short name from the UI's icon set; the engine stores it and checks
  only its length. `color` is sRGB `#rrggbb`.
- **Background colour** is part of the scene model, not metadata, because the compositor
  uses it: `Scene.background: Option<color with alpha>`, edited by
  `scene.set_background`. Absent means transparent, and that is the default. A scene
  draws its background wherever it renders, nested or not. Where a video mix shows a
  transparent pixel, the output is black. A variant uses its parent's background.

**5. Hotkey bindings are collection state.**
- The collection gains `hotkeys: [{ chord, action }]`. `action` is a protocol action
  request, stored as data and checked against the action schema when it is bound. Only
  actions can be bound, never edits, so a hotkey never adds to anyone's undo history.
  Chords are unique: binding a chord that is already bound is `already_exists`, and
  rebinding is a batch of `hotkey.clear` and `hotkey.set`.
- New edits: `hotkey.set { chord, action }` and `hotkey.clear { chord }`, each with the
  obvious inverse.
- Every hotkey runs a protocol action ([R-ARCH-001](../RULES.md#R-ARCH-001)). Who
  listens for the keys (OS-global shortcuts in the desktop host, in-page shortcuts in the
  web UI, none in the headless daemon) is a client decision, outside this ADR.

**6. Compatibility.** Every decision above is additive: new optional fields, new edits
and new query fields. It ships as minor protocol releases under
[R-API-001](../RULES.md#R-API-001), with the schema snapshot as the API review
([R-TEST-002](../RULES.md#R-TEST-002)). If collections are persisted first, each new
field ships with a `schema_version` bump, a migration and a fixture test
([R-API-003](../RULES.md#R-API-003)).

<a id="consequences"></a>
## Consequences
- **Easier:**
  - grouping and ungrouping keep item IDs, so keyframes and transition settings survive
    them;
  - a group can be filtered, blended and reused like any scene, at no extra model cost;
  - keyframes switch, nest and take hotkeys like scenes, and agents edit them with the
    same placement edits plus a `scene_id`;
  - folders, labels, favourites and hotkeys are visible to agents ("hide everything
    labelled Sponsor") and identical in every client;
  - `scene.reorder` is unchanged.
- **Harder:**
  - each group costs a canvas-sized texture per tick unless the engine flattens it;
  - variants add an override layer to rendering, to `item.remove`'s inverse and to the
    cycle check;
  - an override may name an item inside a group, so rendering a variant has to pass its
    overrides down into the group's scene;
  - rejecting deletes while folders, labels or variants are in use adds blockers that
    clients must clear first. The planned `delete.plan` query (register D44) eases that.
- **Not decided here:**
  - whether a nested scene's audio is included ("include its audio when nested",
    register D18, with the audio model);
  - whether the Move transition looks inside nested scenes and groups (register D25);
  - per-scene transition overrides (register D24), and whether a variant may override
    them;
  - the multiview layout itself (register D36);
  - hotkey dispatch and chord syntax (register D42, with the client-side state ADR);
  - the cascading-delete planning query (register D44).
- **Docs to change once this is accepted** (they describe the current state, so they
  are not changed while this ADR is proposed):
  - **ARCH › Data model:** the collection tree gains `folders`, `labels` and `hotkeys`.
    `Scene` becomes `{ id, name, role, variant_of?, overrides, folder?, background?,
    icon, color, favourite, show_in_multiview, items[] }`, and `Source` gains `labels`
    and `favourite`. The "Scenes as sources" bullet adds groups, variants and the
    variant → parent cycle edge. A new bullet says that folders and metadata are
    collection state, and why.
  - **ARCH › Control protocol:** the edit list mentions `item.move` and the `scene_id`
    addressing of placement edits.
  - **ROADMAP › M1b:** add groups as scenes with `item.move`, and the `scene_id`
    addressing of placement edits. **ROADMAP › M2** (or where the maintainer places
    them): keyframe scenes, folders, organisation metadata and hotkeys, built with the
    UI that uses them.
  - **`design/ui-prototype/README.md` › Follow-ups:** the data-model follow-up for groups
    and keyframe scenes links this ADR.
- **Follow-up implementation PRs** (this ADR changes no code):
  - M1b: `jstream-protocol` and `jstream-graph` gain `role`, `scene.set_role`,
    `item.move`, the optional `scene_id` on placement edits, the cycle check on every
    edit that adds nesting, and their inverses and property tests;
  - later: variants and overrides in the graph and the compositor, folders, metadata,
    background colour and hotkeys, each with its edits, inverses and snapshot updates.

  Each changes its crate README, and ARCH where it says what is built.
