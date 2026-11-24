<!-- @doc
id = "ADR-0219"
title = "A scene's own folders"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0219: A scene's own folders

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0024](0024-scene-structure.md) §3 makes folders collection state that organises the
scene list, and [ADR-0064](0064-nested-scene-folders.md) lets them nest. A scene's
keyframe scenes sit under it through `variant_of`, and §3 says "`scene.set_folder` on a
variant is rejected". Its sequences ([ADR-0184](0184-sequences-lanes-and-triggers.md),
timelines on the wire since [ADR-0115](0115-timelines-on-the-wire.md)) are listed after
its keyframes. A scene that gathers many keyframes and sequences becomes a long flat list
under its row, and the maintainer asked for folders under a scene to organise them.

Organising them in one client only would break
[R-ARCH-001](../RULES.md#R-ARCH-001): there are no UI-only features, so the folders are
collection state that every client and agent sees.

<a id="decision"></a>
## Decision
This amends ADR-0024 §3 (and ADR-0064) for keyframe scenes and timelines. A folder may
belong to one scene, and then organises what sits under that scene:
- A folder is `{ id, name, parent?, scene? }`. A folder with no `scene` is a folder of
  the scene list, as before. One with a `scene` is that scene's own and shows under it.
  The scene must exist and not be a keyframe scene, whose keyframes and sequences are
  its parent's.
- `folder.create` takes an optional `scene_id`. A folder's parent, at creation and
  through `folder.set_parent`, belongs to the same scene, or like it to none, so each
  scene's folders, and the scene list's, form their own trees.
- A keyframe scene may now be filed with `scene.set_folder`, but only in a folder of its
  parent. Any other scene is filed only in a folder of the scene list.
- A timeline gains an optional `folder`, set with the rest of it by `timeline.set`: a
  folder of the scene that holds it, or of that scene's parent for a keyframe scene's.
- A folder holding a timeline can't be deleted, as one holding a scene or folder can't;
  its `delete.plan` moves the timelines up to its parent with `timeline.set`.
- A scene's folders go with it: `scene.delete` removes them, and its inverse puts them
  back in their places in folder order, before the scene's timelines.

Every check is `invalid_argument` on the field that names the wrong folder or scene, or
`not_found` for one that doesn't exist. Folders still change no rendering, switching or
nesting.

<a id="consequences"></a>
## Consequences
- One folder list and one set of `folder.*` edits serve both, so clients rename, reorder
  and delete a scene's folders as they do the scene list's. A client draws the scene
  list from the folders with no `scene`, and must not show a scene's folders there.
- The collection file (`schema_version` 23) saves a folder's `scene`, a keyframe scene's
  `folder` and a timeline's `folder`, and creates a scene's folders after the scenes.
- The Scenes panel shows a scene's folders first under it, then its unfiled keyframes,
  then its unfiled sequences, and moves a keyframe, a sequence or one of the scene's
  folders only among the rows under that scene.
