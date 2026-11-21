<!-- @doc
id = "ADR-0169"
title = "A camera anchor is kept whatever it names"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0169: A camera anchor is kept whatever it names

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0164](0164-3d-groups-share-their-scenes-space.md) decision 5, its
"Editing" bullet: which anchors `scene.set_camera_anchor` refuses. What an anchor does,
where it lives, and how a stale one is drawn are unchanged.

<a id="context"></a>
## Context
ADR-0164 has `scene.set_camera_anchor` refuse three kinds of anchor with
`invalid_argument`: one on a scene without a camera, one naming an item that isn't in
the scene, and one naming an item that doesn't place a group. It also says that an
anchor left naming no 3D group later is kept and ignored. That covers the item being
removed, its group turned flat or into a scene, or the camera removed.

These two rules can't both hold. A kept anchor has to come back as it was, and it
comes back through this same edit. A collection file is loaded by applying the edits
that build it, and a deleted scene's undo is a batch that includes this edit. So a
saved collection whose anchor had gone stale failed to load as a whole, and undoing
that scene's delete was refused (review of #1591).

<a id="decision"></a>
## Decision
`scene.set_camera_anchor` refuses only a keyframe scene, whose family's anchor it is.
It stores any other anchor. An anchor that names no item of the scene placing a group
that shares the space, or that is on a scene without a camera, is kept and ignored,
whether it was set that way or left that way later. The camera is then drawn in the
scene's space by its own numbers, and the scene's Properties say so, as ADR-0164
already decides for a stale anchor. The scene's Properties offer only group items, so
a person can't pick an anchor that is ignored from the start.

<a id="consequences"></a>
## Consequences
- **Easier:** every collection that can be saved loads again, and every scene delete
  can be undone, whatever its anchor names.
- **Harder:** an agent that sets an anchor naming the wrong item gets no error. It
  reads the result from `SceneInfo.camera_anchor` and sees the camera stay where it
  was. The design notes say the anchor must name a sharing group's item.
- **Now:** the graph and the mock make only the keyframe check, and their docs and the
  edit's schema description say so.
