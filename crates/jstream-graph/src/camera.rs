//! Scene cameras (ADR-0094): `scene.set_camera`, a keyframe scene's camera override,
//! and the effective camera a scene draws through; and a scene family's camera motion
//! and follow camera (ADR-0113): `scene.set_camera_motion` and `scene.set_camera_follow`.
//! The follow camera is a `follow_pointer` filter on the followed item's effects
//! (ADR-0120), so `scene.set_camera_follow` is a set of `filter.*` edits on the scene's
//! items, and its inverse is theirs.

use crate::apply::{Kinds, invalid, not_found};
use crate::model::{Scene, State};
use jstream_protocol::{
    CameraFollow, CameraMotionModifier, Edit, EntityKind, FOLLOW_POINTER_KIND, FilterChain,
    FilterId, FilterOwner, GeometryError, ItemId, ProtocolError, SceneCamera, SceneId, SourceKind,
    check_camera_motion,
};

/// The built-in source kinds a follow camera can follow: captures that report where
/// the pointer is (ADR-0113).
const FOLLOWABLE_KINDS: [&str; 2] = ["display", "window"];

/// Whether `source_id` is a display or window capture, which a follow camera can follow.
pub(crate) fn followable(state: &State, source_id: &jstream_protocol::SourceId) -> bool {
    state.source(source_id).is_some_and(|source| {
        matches!(source.kind(), SourceKind::Builtin { kind } if FOLLOWABLE_KINDS.contains(&kind.as_str()))
    })
}

/// Applies `scene.set_camera` and returns the edits that undo it. `None` for any other
/// edit.
pub(crate) fn apply(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    match edit {
        Edit::SceneSetCamera { scene_id, camera } => {
            Some(set_camera(state, scene_id, camera.as_ref(), at))
        }
        Edit::SceneSetCameraMotion { scene_id, motion } => {
            Some(set_motion(state, scene_id, motion, at))
        }
        Edit::SceneSetCameraFollow { scene_id, follow } => {
            Some(set_follow(state, kinds, scene_id, follow.as_ref(), at))
        }
        _ => None,
    }
}

/// The field and reason `invalid_argument` names for a camera `validate` refuses.
pub(crate) fn camera_problem(error: GeometryError) -> (&'static str, String) {
    let field = match error {
        GeometryError::FieldOfView => "camera.fov_deg",
        GeometryError::CameraAtTarget => "camera.target",
        GeometryError::FocusDistance => "camera.focus_px",
        GeometryError::Blur => "camera.blur_px",
        _ => "camera",
    };
    (field, error.to_string())
}

fn set_camera(
    state: &mut State,
    scene_id: &SceneId,
    camera: Option<&SceneCamera>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if let Some(camera) = camera {
        camera.validate().map_err(|error| {
            let (field, reason) = camera_problem(error);
            invalid(at, field, reason)
        })?;
        // A keyframe of a 2D scene may have a camera of its own (ADR-0128): a Move into
        // it glides from the 2D side as the framing camera sees it.
    }
    let old = std::mem::replace(&mut state.scenes[si].camera, camera.copied());
    let mut inverse = vec![Edit::SceneSetCamera {
        scene_id: scene_id.clone(),
        camera: old,
    }];
    // A parent that had a camera and loses it takes its keyframes' overrides with it
    // (ADR-0094 §6); a 2D parent's keyframes keep their own (ADR-0128).
    if camera.is_none() && old.is_some() && state.scenes[si].variant_of.is_none() {
        // The inverse brings them back after the parent's camera.
        for scene in &mut state.scenes {
            if scene.variant_of.as_ref() == Some(scene_id)
                && let Some(dropped) = scene.camera.take()
            {
                inverse.push(Edit::SceneSetCamera {
                    scene_id: scene.id.clone(),
                    camera: Some(dropped),
                });
            }
        }
    }
    Ok(inverse)
}

/// The index of `scene_id`, a scene that isn't a keyframe scene: camera motion and the
/// follow camera belong to the family.
fn family_scene(
    state: &State,
    scene_id: &SceneId,
    at: usize,
    field: &str,
) -> Result<usize, ProtocolError> {
    let si = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if let Some(parent) = &state.scenes[si].variant_of {
        return Err(invalid(
            at,
            field,
            format!(
                "keyframe scene `{scene_id}` draws its parent `{parent}`'s camera {}; set it \
                 on `{parent}`",
                if field == "motion" {
                    "motion"
                } else {
                    "follow"
                }
            ),
        ));
    }
    Ok(si)
}

fn set_motion(
    state: &mut State,
    scene_id: &SceneId,
    motion: &[CameraMotionModifier],
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = family_scene(state, scene_id, at, "motion")?;
    check_camera_motion(motion).map_err(|p| invalid(at, &p.field, p.reason))?;
    let old = std::mem::replace(&mut state.scenes[si].camera_motion, motion.to_vec());
    Ok(vec![Edit::SceneSetCameraMotion {
        scene_id: scene_id.clone(),
        motion: old,
    }])
}

/// `scene.set_camera_follow` (ADR-0120): with a follow, puts its numbers on the
/// followed item's `follow_pointer` filter, turned on (adding one, last in its effects,
/// when it has none); and removes every other `follow_pointer` filter of the scene's
/// items. Without one, removes them all. Returns the inverse of those filter edits.
fn set_follow(
    state: &mut State,
    kinds: &Kinds,
    scene_id: &SceneId,
    follow: Option<&CameraFollow>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = family_scene(state, scene_id, at, "follow")?;
    if let Some(follow) = follow {
        follow
            .check()
            .map_err(|p| invalid(at, &p.field, p.reason))?;
        let item = state.scenes[si]
            .items
            .iter()
            .find(|item| item.id == follow.item_id)
            .ok_or_else(|| {
                invalid(
                    at,
                    "follow.item_id",
                    format!("item `{}` is not in scene `{scene_id}`", follow.item_id),
                )
            })?;
        if !followable(state, &item.source_id) {
            return Err(invalid(
                at,
                "follow.item_id",
                format!(
                    "item `{}` doesn't show a display or window capture, so there is no pointer \
                     to follow",
                    follow.item_id
                ),
            ));
        }
    }
    let existing: Vec<(ItemId, FilterId)> = state.scenes[si]
        .items
        .iter()
        .flat_map(|item| {
            item.effects
                .video
                .iter()
                .filter(|f| f.kind == FOLLOW_POINTER_KIND)
                .map(|f| (item.id.clone(), f.id.clone()))
        })
        .collect();
    let owner = |item_id: &ItemId| FilterOwner::Item {
        item_id: item_id.clone(),
    };
    let mut steps = Vec::new();
    let mut kept = None;
    if let Some(follow) = follow {
        match existing.iter().find(|(item, _)| *item == follow.item_id) {
            Some((item, id)) => {
                kept = Some(id.clone());
                steps.push(Edit::FilterSetSettings {
                    owner: owner(item),
                    filter_id: id.clone(),
                    patch: follow.settings(),
                });
                steps.push(Edit::FilterSetEnabled {
                    owner: owner(item),
                    filter_id: id.clone(),
                    enabled: true,
                });
            }
            None => steps.push(Edit::FilterAdd {
                owner: owner(&follow.item_id),
                chain: FilterChain::Video,
                filter_id: unused_id(state, &follow.item_id),
                kind: FOLLOW_POINTER_KIND.into(),
                settings: follow.settings(),
                enabled: None,
                index: None,
            }),
        }
    }
    for (item, id) in &existing {
        let is_kept = follow.is_some_and(|f| f.item_id == *item) && kept.as_ref() == Some(id);
        if !is_kept {
            steps.push(Edit::FilterRemove {
                owner: owner(item),
                filter_id: id.clone(),
            });
        }
    }
    let mut inverse = Vec::new();
    for step in &steps {
        let undo = crate::filter::apply(state, kinds, step, at).unwrap_or_else(|| {
            Err(ProtocolError::Internal {
                reason: "invariant: a follow camera's step is a filter edit".into(),
            })
        })?;
        // Undone last first.
        inverse.splice(0..0, undo);
    }
    Ok(inverse)
}

/// A filter ID `item_id`'s chains don't use yet, for a follow camera's filter:
/// `follow-pointer`, else `follow-pointer-2` and so on.
fn unused_id(state: &State, item_id: &ItemId) -> FilterId {
    let used = |id: &FilterId| {
        state
            .item(item_id)
            .is_some_and(|(_, item)| item.effects.find(id).is_some())
    };
    (1..)
        .map(|n| {
            FilterId::new(if n == 1 {
                "follow-pointer".to_owned()
            } else {
                format!("follow-pointer-{n}")
            })
        })
        .find(|id| !used(id))
        .unwrap_or_else(|| FilterId::new("follow-pointer"))
}

/// The edits that put back a deleted scene's camera motion, after its camera. Its
/// follow camera comes back with its items' effects (ADR-0120).
pub(crate) fn restore(scene: &Scene) -> Vec<Edit> {
    let mut edits = Vec::new();
    if !scene.camera_motion.is_empty() {
        edits.push(Edit::SceneSetCameraMotion {
            scene_id: scene.id.clone(),
            motion: scene.camera_motion.clone(),
        });
    }
    edits
}

impl State {
    /// The camera `scene` draws through (ADR-0094 §6): its own, or for a keyframe scene
    /// its override, else its parent's. `None` for a 2D scene.
    pub fn effective_camera(&self, scene: &Scene) -> Option<SceneCamera> {
        scene.camera.or_else(|| {
            let parent = scene.variant_of.as_ref()?;
            let pi = self.scene_index(parent)?;
            self.scenes[pi].camera
        })
    }
}
