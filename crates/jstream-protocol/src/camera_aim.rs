//! A scene camera's live aim (ADR-0152, #1181): the pan, tilt and zoom an operator gives
//! a scene's camera at runtime, as a PTZ controller drives a head, on top of the camera
//! the scene stores and whatever moves it (ADR-0113). The aim (0, 0, 1) is the stored
//! camera itself, which is the aim's home. `scene.drive_camera` drives it at
//! signed speeds, `scene.aim_camera` moves it to an absolute aim, and
//! `scene.camera_aim` reads it. None of it is stored or undone.
//!
//! The speeds are VISCA's steps, as `ptz.drive` takes them (ADR-0146 §9); the aim is in
//! degrees and a zoom factor, since a virtual camera has no counts of its own.

use crate::finite::Finite32;
use crate::id::SceneId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The furthest an aim pans either way from the stored camera's view, in degrees.
pub const MAX_AIM_PAN_DEG: f32 = 170.0;
/// The furthest an aim tilts up or down from the stored camera's view, in degrees. The
/// view itself is held short of straight up or down, at 89° from level.
pub const MAX_AIM_TILT_DEG: f32 = 89.0;
/// The most an aim zooms in. 1 is the stored camera's own field of view.
pub const MAX_AIM_ZOOM: f32 = 8.0;

/// An aim, or part of one: each field set moves that axis, and an absent one stays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CameraAim {
    /// Pan from the stored camera's view, in degrees, -170 to 170, positive to the right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan_deg: Option<Finite32>,
    /// Tilt from the stored camera's view, in degrees, -89 to 89, positive up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt_deg: Option<Finite32>,
    /// Zoom, 1 (the stored camera's field of view) to 8 (an eighth as wide).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<Finite32>,
}

impl CameraAim {
    /// Whether it sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == CameraAim::default()
    }

    /// The first field that isn't valid, and why: one out of its range. An empty aim is
    /// valid here; `scene.aim_camera` refuses one itself.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        let out = |v: Option<Finite32>, lo: f32, hi: f32| {
            v.is_some_and(|v| !(lo..=hi).contains(&v.get()))
        };
        if out(self.pan_deg, -MAX_AIM_PAN_DEG, MAX_AIM_PAN_DEG) {
            return Some((
                "pan_deg",
                format!("must be within -{MAX_AIM_PAN_DEG}..={MAX_AIM_PAN_DEG}"),
            ));
        }
        if out(self.tilt_deg, -MAX_AIM_TILT_DEG, MAX_AIM_TILT_DEG) {
            return Some((
                "tilt_deg",
                format!("must be within -{MAX_AIM_TILT_DEG}..={MAX_AIM_TILT_DEG}"),
            ));
        }
        if out(self.zoom, 1.0, MAX_AIM_ZOOM) {
            return Some(("zoom", format!("must be within 1..={MAX_AIM_ZOOM}")));
        }
        None
    }
}

/// The result of `scene.camera_aim`: a scene family's live aim now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CameraAimInfo {
    /// Pan from the stored camera's view, in degrees, positive to the right.
    pub pan_deg: Finite32,
    /// Tilt from the stored camera's view, in degrees, positive up.
    pub tilt_deg: Finite32,
    /// Zoom, 1 to 8.
    pub zoom: Finite32,
    /// Whether a drive or an aim is moving it.
    pub moving: bool,
}

/// The first field of a `scene.drive_camera` or `scene.aim_camera` that isn't valid, and
/// why, or `None` for any other action. The engine and the mock check these before
/// they look the scene up.
pub fn camera_aim_check(action: &crate::Action) -> Option<(&'static str, String)> {
    use crate::Action;
    use crate::ptz::{MAX_PTZ_PAN_SPEED, MAX_PTZ_TILT_SPEED, MAX_PTZ_ZOOM_SPEED};
    let speed = |field, v: Option<i8>, max: u8| {
        v.is_some_and(|v| v.unsigned_abs() > max)
            .then(|| (field, format!("must be within -{max}..={max}")))
    };
    match action {
        Action::SceneDriveCamera {
            pan, tilt, zoom, ..
        } => {
            if pan.is_none() && tilt.is_none() && zoom.is_none() {
                return Some(("pan", "a drive needs pan, tilt or zoom".into()));
            }
            speed("pan", *pan, MAX_PTZ_PAN_SPEED)
                .or_else(|| speed("tilt", *tilt, MAX_PTZ_TILT_SPEED))
                .or_else(|| speed("zoom", *zoom, MAX_PTZ_ZOOM_SPEED))
        }
        Action::SceneAimCamera { aim, speed, .. } => {
            if aim.is_empty() {
                Some(("pan_deg", "an aim needs pan_deg, tilt_deg or zoom".into()))
            } else {
                aim.problem().or_else(|| {
                    speed
                        .is_some_and(|s| !(1..=MAX_PTZ_PAN_SPEED).contains(&s))
                        .then(|| ("speed", format!("must be within 1..={MAX_PTZ_PAN_SPEED}")))
                })
            }
        }
        _ => None,
    }
}

/// The scene a `scene.drive_camera` or `scene.aim_camera` names.
pub fn camera_aim_scene(action: &crate::Action) -> Option<&SceneId> {
    match action {
        crate::Action::SceneDriveCamera { scene_id, .. }
        | crate::Action::SceneAimCamera { scene_id, .. } => Some(scene_id),
        _ => None,
    }
}
