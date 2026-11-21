//! 3D groups (ADR-0164, docs/adr/0164-3d-groups-share-their-scenes-space.md): a group
//! that shares its 3D scene's space has no layer, and each of its children is planned
//! into the scene's own pass with the chain of group items it is placed through. As each
//! frame is drawn, every link of the chain is resolved the way any item is (its live
//! values, then its motion) and the child's own resolved geometry is placed through them
//! ([`GroupPose`]), so the scene's sort, lights, models and focus take it as an item of
//! the scene. A camera that rides with a group is placed through its chain the same way.

use std::sync::Arc;

use jstream_graph::{Item as GraphItem, Placement as GraphPlacement, State};
use jstream_media::MediaTime;
use jstream_protocol::{
    Canvas, GroupPose, ItemGeometry, ItemId, SceneCamera, SceneRole, SourceKind,
};
use jstream_render::{Crop, Placement};

use super::{LiveParams, Shot, Through, placement};
use crate::motion::{self, TrackMotion};

/// One group item a child of a 3D group is placed through, as planned: everything that
/// resolves its pose at a frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GroupLink {
    /// The group item, whose live values override `placement`.
    pub(super) item: ItemId,
    /// The keyframe scene that draws it, if any: which live values apply.
    pub(super) through: Option<Through>,
    /// Its placement as planned: a geometry, since a legacy group draws flat.
    pub(super) placement: Placement,
    /// Its opacity, which multiplies its children's (ADR-0164 §3).
    pub(super) opacity: f32,
    /// Its motion modifiers (ADR-0099), which move every child.
    pub(super) motion: TrackMotion,
}

/// The group items a child is placed through, outermost first; `None` for an item of
/// the scene itself.
pub(super) type Groups = Option<Arc<[GroupLink]>>;

impl GroupLink {
    /// The link for `item` when it places a group that shares its 3D scene's space
    /// (ADR-0164 §1): a `scene` source whose scene has the `group` role and
    /// `share_space` on, placed by a geometry. `None` for anything else, which draws as
    /// it does today.
    pub(super) fn of(
        state: &State,
        item: &GraphItem,
        through: Option<Through>,
    ) -> Option<(jstream_protocol::SceneId, Self)> {
        let source = state.source(item.source_id())?;
        let SourceKind::Scene { scene_id } = source.kind() else {
            return None;
        };
        let group = state.scene(scene_id)?;
        if group.role() != SceneRole::Group || !group.options().share_space {
            return None;
        }
        let GraphPlacement::Geometry(_) = item.placement() else {
            return None;
        };
        Some((
            scene_id.clone(),
            Self {
                item: item.id().clone(),
                through,
                placement: placement(item.placement()),
                opacity: item.opacity().get(),
                motion: TrackMotion::same(item.id().clone(), motion::of(item)),
            },
        ))
    }

    /// Its geometry and opacity at `at`: its live values, then its motion. `None` for a
    /// placement that isn't a geometry.
    fn at(&self, shot: &Shot, at: MediaTime, live: &LiveParams) -> Option<(ItemGeometry, f32)> {
        let (placement, crop) = live.apply_to(
            &self.item,
            self.through.as_ref(),
            self.placement,
            Crop::NONE,
        );
        let (item, modifiers) = &self.motion.to;
        let by = shot.motion_at(item, modifiers, at);
        let (placement, _, opacity) = motion::apply(&by, placement, crop, self.opacity);
        match placement {
            Placement::Geometry(geometry) => Some((geometry, opacity)),
            _ => None,
        }
    }

    /// Its geometry as planned, with no live values or motion.
    pub(super) fn committed(&self) -> Option<(ItemGeometry, f32)> {
        match self.placement {
            Placement::Geometry(geometry) => Some((geometry, self.opacity)),
            _ => None,
        }
    }
}

/// The pose of the innermost of `links`, each placed through the ones outside it, and
/// their opacities multiplied, with `pose_of` giving each link's own geometry. `None`
/// when a link has no geometry or its pose can't be placed.
fn chain_pose(
    links: &[GroupLink],
    canvas: Canvas,
    mut pose_of: impl FnMut(&GroupLink) -> Option<(ItemGeometry, f32)>,
) -> Option<(GroupPose, f32)> {
    let mut pose: Option<GroupPose> = None;
    let mut opacity = 1.0;
    for link in links {
        let (geometry, own) = pose_of(link)?;
        let geometry = match &pose {
            Some(outer) => outer.place(geometry).ok()?,
            None => geometry,
        };
        pose = Some(GroupPose::of(geometry, canvas).ok()?);
        opacity *= own;
    }
    pose.map(|pose| (pose, opacity))
}

/// The pose `links` place their child through at `at`, with their live values and
/// motion, and their opacities multiplied. `None` without links.
pub(super) fn pose_at(
    links: &[GroupLink],
    shot: &Shot,
    at: MediaTime,
    live: &LiveParams,
) -> Option<(GroupPose, f32)> {
    chain_pose(links, shot.canvas, |link| link.at(shot, at, live))
}

/// The pose `links` place their child through as planned, with no live values or
/// motion: a transition's ends.
pub(super) fn pose_committed(links: &[GroupLink], canvas: Canvas) -> Option<(GroupPose, f32)> {
    chain_pose(links, canvas, GroupLink::committed)
}

/// `geometry`, a child's in its group's pixels, placed in its scene through `groups` at
/// `at`, with its opacity multiplied by theirs; as it is without groups. `None` when a
/// link can't be placed, so the child draws nothing that frame.
pub(super) fn place_at(
    groups: &[GroupLink],
    (geometry, opacity): (ItemGeometry, f32),
    shot: &Shot,
    at: MediaTime,
    live: &LiveParams,
) -> Option<(ItemGeometry, f32)> {
    if groups.is_empty() {
        return Some((geometry, opacity));
    }
    let (pose, theirs) = pose_at(groups, shot, at, live)?;
    Some((pose.place(geometry).ok()?, opacity * theirs))
}

/// [`place_at`] for a placement: a geometry or a model's layer is placed, and anything
/// else (a legacy transform, which has no box to place) draws nothing in a 3D group.
pub(super) fn place_placement(
    groups: &Groups,
    (placement, opacity): (Placement, f32),
    shot: &Shot,
    at: MediaTime,
    live: &LiveParams,
) -> Option<(Placement, f32)> {
    let Some(groups) = groups.as_deref() else {
        return Some((placement, opacity));
    };
    match placement {
        Placement::Geometry(geometry) => place_at(groups, (geometry, opacity), shot, at, live)
            .map(|(geometry, opacity)| (Placement::Geometry(geometry), opacity)),
        Placement::Layer(geometry) => place_at(groups, (geometry, opacity), shot, at, live)
            .map(|(geometry, opacity)| (Placement::Layer(geometry), opacity)),
        Placement::Legacy(_) => None,
    }
}

/// `camera`, whose numbers are in the space of the group `anchor` places it through,
/// placed in the scene's space at `at` (ADR-0164 §5). A camera that can't be placed is
/// drawn by its own numbers, as an anchor that names no 3D group is.
pub(super) fn place_camera_at(
    anchor: &[GroupLink],
    camera: SceneCamera,
    shot: &Shot,
    at: MediaTime,
    live: &LiveParams,
) -> SceneCamera {
    pose_at(anchor, shot, at, live)
        .and_then(|(pose, _)| pose.place_camera(camera, shot.canvas).ok())
        .unwrap_or(camera)
}

/// [`place_camera_at`] as planned, with no live values or motion.
pub(super) fn place_camera_committed(
    anchor: &[GroupLink],
    camera: SceneCamera,
    canvas: Canvas,
) -> SceneCamera {
    pose_committed(anchor, canvas)
        .and_then(|(pose, _)| pose.place_camera(camera, canvas).ok())
        .unwrap_or(camera)
}

/// The link a scene's camera rides with (ADR-0164 §5): its family's anchor, when that
/// names an item `scene` shows (at its effective values, for a keyframe scene) that
/// places a group sharing its space. `None` otherwise, and the camera is in the scene's
/// own space; and `None` while the family has a follow camera, which frames its item in
/// the scene's space, so the anchor is kept and ignored.
pub(super) fn anchor_of(state: &State, scene: &jstream_graph::Scene) -> Groups {
    let family = state.scene(scene.variant_of().unwrap_or(scene.id()))?;
    if family.camera_follow().is_some() {
        return None;
    }
    let anchor = family.camera_anchor()?;
    let items = state.shown_items(scene);
    let item = items.iter().find(|item| item.id() == anchor)?;
    let (_, link) = GroupLink::of(state, item, Through::of(scene, anchor))?;
    Some(Arc::from([link]))
}

/// The camera scene `scene_id` draws through as committed, placed through its anchor's
/// group as planned (ADR-0164 §5): where a transition starting from it starts.
pub(crate) fn placed_camera(
    state: &State,
    scene_id: &jstream_protocol::SceneId,
) -> Option<SceneCamera> {
    let scene = state.scene(scene_id)?;
    let camera = state.effective_camera(scene)?;
    Some(match anchor_of(state, scene) {
        Some(anchor) => place_camera_committed(&anchor, camera, state.canvas()),
        None => camera,
    })
}
