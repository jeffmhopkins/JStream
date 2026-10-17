<!-- @doc
id = "ADR-0064"
title = "Nested scene folders"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0064: Nested scene folders

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0024](0024-scene-structure.md) §3 makes folders collection state that organises the
scene list. It says "Folders are one level deep. Nested folders can come later as an
optional parent field." The chosen UI prototype's Scenes panel is a tree of folders
([design/ui-prototype](../../design/ui-prototype/README.md#settles)), and roadmap B11
builds folders. Building them one level deep and adding the parent later would mean a
second protocol bump and a second round of clients for the same structure.

<a id="decision"></a>
## Decision
This amends ADR-0024 §3. Folders nest, through the optional parent field that §3 left
room for:
- A folder is `{ id, name, parent? }`. A folder with no `parent` is top level, so a
  collection with only top-level folders reads and saves as it did under §3.
- `folder.create` takes an optional `parent`, and a new edit, `folder.set_parent`,
  moves a folder into another folder or back to the top level. Its inverse is
  `folder.set_parent` with the previous parent. A `parent` must exist, and an edit that
  would put a folder inside itself, at any depth, is rejected, so the folders always form
  a tree.
- A folder's child folders show in folder order, as its scenes show in display order.
- `folder.delete` is rejected while a scene or a folder names the folder. `delete.plan`
  for a folder moves what it holds up to its parent (the top level for a top-level
  folder), then deletes it.

Everything else in ADR-0024 stands. Folders still change no rendering, switching or
nesting of scenes, and a variant still never sits in a folder.

<a id="consequences"></a>
## Consequences
- Clients draw the scene list as a tree of any depth, and must not assume a folder's
  parent comes before it in folder order. The collection file loads folders top level
  first, then sets each nested one's parent.
- The cycle check for folders is separate from the scene graph's: a folder is not a
  dependency edge, and only `folder.create` and `folder.set_parent` can close a loop.
- Built in B11 (#323), with the rest of ADR-0024 §3 and §4.
