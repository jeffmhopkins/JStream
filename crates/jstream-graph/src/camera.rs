//! Scene cameras (ADR-0094): `scene.set_camera`, a keyframe scene's camera override,
//! and the effective camera a scene draws through; and a scene family's camera motion
//! and follow camera (ADR-0113): `scene.set_camera_motion` and `scene.set_camera_follow`.

use crate::apply::{invalid, not_found};
use crate::model::{Scene, State};
use jstream_protocol::{
    CameraFollow, CameraMotionModifier, Edit, EntityKind, GeometryError, ProtocolError,
    SceneCamera, SceneId, SourceKind, check_camera_motion,
};

/// The built-in source kinds a follow camera can follow: captures that report where
/// the pointer is (ADR-0113).
const FOLLOWABLE_KINDS: [&str; 2] = ["display", "window"];

/// Applies `scene.set_camera` and returns the edits that undo it. `None` for any other
/// edit.
pub(crate) fn apply(
    state: &mut State,
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
            Some(set_follow(state, scene_id, follow.as_ref(), at))
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
        if let Some(parent) = state.scenes[si].variant_of.clone() {
            let parent_has_one = state
                .scene_index(&parent)
                .is_some_and(|pi| state.scenes[pi].camera.is_some());
            if !parent_has_one {
                return Err(invalid(
                    at,
                    "camera",
                    format!(
                        "keyframe scene `{scene_id}` shows its parent `{parent}`, which has no \
                         camera; set one on `{parent}` first"
                    ),
                ));
            }
        }
    }
    let old = std::mem::replace(&mut state.scenes[si].camera, camera.copied());
    let mut inverse = vec![Edit::SceneSetCamera {
        scene_id: scene_id.clone(),
        camera: old,
    }];
    if camera.is_none() && state.scenes[si].variant_of.is_none() {
        // A 2D scene's keyframes are 2D too: their overrides go, and the inverse brings
        // them back after the parent's camera.
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
                if field == "motion" { "motion" } else { "follow" }
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

fn set_follow(
    state: &mut State,
    scene_id: &SceneId,
    follow: Option<&CameraFollow>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = family_scene(state, scene_id, at, "follow")?;
    if let Some(follow) = follow {
        follow.check().map_err(|p| invalid(at, &p.field, p.reason))?;
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
        let followable = state.source(&item.source_id).is_some_and(|source| {
            matches!(source.kind(), SourceKind::Builtin { kind } if FOLLOWABLE_KINDS.contains(&kind.as_str()))
        });
        if !followable {
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
    let old = std::mem::replace(&mut state.scenes[si].camera_follow, follow.cloned());
    Ok(vec![Edit::SceneSetCameraFollow {
        scene_id: scene_id.clone(),
        follow: old,
    }])
}

/// The edits that put back a deleted scene's camera motion and follow camera, after its
/// camera and items.
pub(crate) fn restore(scene: &Scene) -> Vec<Edit> {
    let mut edits = Vec::new();
    if !scene.camera_motion.is_empty() {
        edits.push(Edit::SceneSetCameraMotion {
            scene_id: scene.id.clone(),
            motion: scene.camera_motion.clone(),
        });
    }
    if let Some(follow) = &scene.camera_follow {
        edits.push(Edit::SceneSetCameraFollow {
            scene_id: scene.id.clone(),
            follow: Some(follow.clone()),
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
