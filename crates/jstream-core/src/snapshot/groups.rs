//! 3D groups (ADR-0164, docs/adr/0164-3d-groups-share-their-scenes-space.md): a group
//! that shares its 3D scene's space has no layer, and each of its children is planned
//! into the scene's own pass with the chain of group items it is placed through. As each
//! frame is drawn, every link of the chain is resolved the way any item is (its live
//! values, then its motion) and the child's own resolved geometry is placed through them
//! ([`GroupPose`]), so the scene's sort, lights, models and focus take it as an item of
//! the scene. A camera that rides with a group is placed through its chain the same way.
//! In a transition, each child is a track of its own whose ends keep the child's place in
//! its group's space ([`TrackGroups`]), so the group's live values and motion move it
//! while the transition runs too.

use std::sync::Arc;

use jstream_graph::{Item as GraphItem, Placement as GraphPlacement, State};
use jstream_media::MediaTime;
use jstream_protocol::{
    Canvas, GroupPose, ItemGeometry, ItemId, SceneCamera, SceneRole, SourceKind,
};
use jstream_render::transition::{Endpoint, Track};
use jstream_render::{Crop, Placement};

use super::{LiveParams, Shot, Through, moved_track, placement};
use crate::motion::{self, Motion, TrackMotion};

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

/// A 3D group's child with a legacy transform as the box that transform draws for an
/// image of `extent` with `crop`, so it is placed through its groups as any child is
/// (ADR-0164 §1 and §2: every child is placed in the scene's space, its geometry in its
/// group's canvas pixels). A transition's ends take the same box (`endpoint_geometry`).
/// Anything else, and a child of no group, is as it is.
pub(super) fn legacy_box(
    groups: &Groups,
    placement: Placement,
    crop: Crop,
    extent: (u32, u32),
) -> Placement {
    let (Some(_), Placement::Legacy(transform)) = (groups, placement) else {
        return placement;
    };
    let (Some(position), Some(scale), Some(rotation_deg)) = (
        jstream_protocol::Vec2::new(transform.position[0], transform.position[1]),
        jstream_protocol::Vec2::new(transform.scale[0], transform.scale[1]),
        jstream_protocol::Finite32::new(transform.rotation_deg),
    ) else {
        return placement;
    };
    let transform = jstream_protocol::ItemTransform {
        position,
        scale,
        rotation_deg,
    };
    let crop = jstream_protocol::Crop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    };
    match ItemGeometry::from_legacy(transform, crop, extent.0, extent.1) {
        Ok(Some(boxed)) => Placement::Geometry(boxed.geometry),
        _ => placement,
    }
}

/// [`place_at`] for a placement: a geometry or a model's layer is placed, and anything
/// else (a legacy transform not yet made a box by [`legacy_box`]) draws nothing in a 3D
/// group.
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

/// One end of a transition's track for a 3D group's child (ADR-0164 §4): where the child
/// is in its group's space as planned, and the group items it is placed through.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupEnd {
    /// The group items, outermost first.
    pub(super) links: Arc<[GroupLink]>,
    /// The child's own endpoint, in its group's canvas pixels, at its own opacity.
    pub(super) at: Endpoint,
}

/// The [`GroupEnd`]s of a track's two sides, as [`TrackMotion`] holds their modifiers:
/// `None` on a side whose item is the scene's own, which its track already holds where it
/// is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TrackGroups {
    /// The side it leaves.
    pub(crate) from: Option<Arc<GroupEnd>>,
    /// The side it arrives at.
    pub(crate) to: Option<Arc<GroupEnd>>,
}

impl TrackGroups {
    /// One child's end on both sides.
    pub(super) fn same(end: GroupEnd) -> Self {
        let end = Arc::new(end);
        Self {
            from: Some(Arc::clone(&end)),
            to: Some(end),
        }
    }

    /// Whether neither side is a 3D group's child.
    pub(crate) fn is_none(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    /// Which sides are not a group's child, as [`TrackMotion::offsets_on`] takes them: a
    /// child's own motion moves it in its group's space, in [`resolved_track`], not on
    /// top of where it is placed.
    pub(super) fn ungrouped(&self) -> [bool; 2] {
        [self.from.is_none(), self.to.is_none()]
    }
}

/// `track` with each end that is a 3D group's child placed as the child is now (ADR-0164
/// §4): its own motion modifiers (from `moved`) in its group's space, then the group items'
/// live values and motion, as the scene's pass places it. A transition then moves the
/// child between where it is placed on each side at each frame, so a swaying rig sways
/// on while it runs. `None` when an end's groups can't be placed, so the track draws
/// nothing that frame. Allocates nothing beyond the track's clone.
pub(super) fn resolved_track(
    track: &Track,
    (ends, moved): (&TrackGroups, &TrackMotion),
    canvas_box: bool,
    shot: &Shot,
    at: MediaTime,
    live: &LiveParams,
) -> Option<Track> {
    let place = |end: &GroupEnd, (item, modifiers): &(ItemId, Motion)| -> Option<Endpoint> {
        let by = shot.motion_at(item, modifiers, at);
        let own = moved_track(canvas_box, &by, end.at);
        let (geometry, opacity) =
            place_at(&end.links, (own.geometry, own.opacity), shot, at, live)?;
        Some(Endpoint {
            geometry,
            opacity,
            ..own
        })
    };
    let mut track = track.clone();
    match &mut track {
        Track::Pair { from, to, .. } => {
            if let Some(end) = &ends.from {
                *from = place(end, &moved.from)?;
            }
            if let Some(end) = &ends.to {
                *to = place(end, &moved.to)?;
            }
        }
        Track::Appear { to, .. } | Track::Through { at: to, .. } => {
            if let Some(end) = &ends.to {
                *to = place(end, &moved.to)?;
            }
        }
        Track::Disappear { from, .. } => {
            if let Some(end) = &ends.from {
                *from = place(end, &moved.from)?;
            }
        }
    }
    Some(track)
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
