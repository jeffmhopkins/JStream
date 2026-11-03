//! Scene cameras (ADR-0094): `scene.set_camera`, a keyframe scene's camera override,
//! and the effective camera a scene draws through.

use crate::apply::{invalid, not_found};
use crate::model::{Scene, State};
use jstream_protocol::{Edit, EntityKind, GeometryError, ProtocolError, SceneCamera, SceneId};

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
        _ => None,
    }
}

/// The field and reason `invalid_argument` names for a camera `validate` refuses.
pub(crate) fn camera_problem(error: GeometryError) -> (&'static str, String) {
    let field = match error {
        GeometryError::FieldOfView => "camera.fov_deg",
        GeometryError::CameraAtTarget => "camera.target",
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
