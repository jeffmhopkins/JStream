//! Depth-aware scene filters (ADR-0143 decision 5): kinds that read a scene's view depth
//! (`Compositor::render_view_depth`) beside their input.
//!
//! The chain runtime hands a scene chain's depth-aware filters that scene's view depth
//! through [`crate::VideoFilter::set_depth`] when the scene has a camera. Anywhere else a
//! filter has none, and passes its input through unchanged.
//!
//! These kinds are built by [`build`] but not listed by [`super::descriptors`]: their
//! own list is [`descriptors`], which an engine adds to its kinds when it produces the
//! view depth for them, as `jstream-core` does, so no chain holds a depth filter that
//! could never see depth.

mod bloom;
mod fog;
mod occlusion;
mod reflection;
mod volumetric;

use std::sync::Arc;

use jstream_protocol::{CameraView, KindDescriptor, SceneCamera, Settings};

use crate::SceneLight;

use crate::filter::Pass;
use crate::image::Image;

/// A scene's view depth, as a depth-aware filter reads it (ADR-0143 decisions 4 and 5).
#[derive(Clone, Debug)]
pub struct ViewDepth {
    /// From [`crate::Compositor::render_view_depth`]: red holds `1 / w = f / D`, 0 where
    /// nothing was drawn. The size of the scene's canvas.
    pub image: Arc<Image>,
    /// The camera's focal length `f`, in canvas pixels
    /// ([`jstream_protocol::CameraView::focal_px`]), which turns `1 / w` back into a
    /// distance.
    pub focal_px: f32,
    /// The camera as seen on the scene's canvas (ADR-0151 decision 1): its position,
    /// axes and principal point, which turn a pixel and its `1 / w` back into a point
    /// of the scene's space. `None` leaves the kinds that need it to pass through.
    pub view: Option<CameraView>,
    /// The scene's lights, as a `scene`-lit model receives them (ADR-0143 decision 3).
    pub lights: Arc<[SceneLight]>,
    /// The shadow maps of the lights that cast shadows (ADR-0151 decision 3), for the
    /// kinds that [`reads_shadows`]: at most [`crate::MAX_SHADOW_MAPS`] are read.
    pub shadows: Arc<[DepthShadow]>,
    /// The scene drawn mirrored in its floor, for the kinds that
    /// [`reflection_floor`] names a floor for; `None` when none was drawn.
    pub reflection: Option<DepthReflection>,
}

/// A scene drawn through its camera mirrored in the plane `y = floor_y` (ADR-0151
/// decision 4), as a right-handed camera, so its x runs the other way about the
/// principal point: the picture and its view depth, both the scene's canvas size.
#[derive(Clone, Debug)]
pub struct DepthReflection {
    /// The mirrored picture.
    pub image: Image,
    /// Its view depth, `1 / w` through the mirrored camera.
    pub depth: Image,
    /// The floor's height, in the scene's space (y down).
    pub floor_y: f32,
}

/// One light's shadow map as a depth-aware filter reads it: what
/// [`crate::ShadowMap`] holds, with the map kept.
#[derive(Clone, Debug)]
pub struct DepthShadow {
    /// Which of [`ViewDepth::lights`] casts it.
    pub light: usize,
    /// The map: red holds `1 / w` of the nearest caster seen from the light.
    pub image: Image,
    /// The light's view, seen on the map's canvas.
    pub camera: SceneCamera,
}

impl ViewDepth {
    /// A view depth with no camera or lights: what the kinds that read only the depth
    /// need.
    pub fn new(image: Arc<Image>, focal_px: f32) -> Self {
        Self {
            image,
            focal_px,
            view: None,
            lights: Arc::from([]),
            shadows: Arc::from([]),
            reflection: None,
        }
    }
}

/// The depth-aware filter kinds, ordered by ID.
pub fn descriptors() -> Vec<KindDescriptor> {
    vec![
        occlusion::descriptor(),
        bloom::descriptor(),
        fog::descriptor(),
        reflection::descriptor(),
        volumetric::descriptor(),
    ]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    match kind_id {
        fog::KIND => Some(Box::new(fog::DepthFog::new(settings))),
        bloom::KIND => Some(Box::new(bloom::DepthBloom::new(settings))),
        occlusion::KIND => Some(Box::new(occlusion::AmbientOcclusion::new(settings))),
        volumetric::KIND => Some(Box::new(volumetric::VolumetricFog::new(settings))),
        reflection::KIND => Some(Box::new(reflection::FloorReflection::new(settings))),
        _ => None,
    }
}

/// Whether a filter of `kind_id` reads a scene's view depth, so the engine draws it for
/// a scene chain that holds one (ADR-0143 decision 4).
pub fn reads_depth(kind_id: &str) -> bool {
    [
        bloom::KIND,
        fog::KIND,
        occlusion::KIND,
        reflection::KIND,
        volumetric::KIND,
    ]
    .contains(&kind_id)
}

/// How far past its input a filter of this group's `kind_id` draws at `settings`, in
/// canvas pixels; `None` for a kind that draws within it, or another group's.
pub(crate) fn outset(kind_id: &str, settings: &Settings) -> Option<f32> {
    (kind_id == bloom::KIND).then(|| bloom::outset(settings))
}

/// The floor a filter of `kind_id` with `settings` mirrors its scene in, so the engine
/// draws the scene mirrored there for a scene chain that holds one (ADR-0151 decision
/// 4); `None` for every other kind.
pub fn reflection_floor(kind_id: &str, settings: &Settings) -> Option<f32> {
    (kind_id == reflection::KIND).then(|| reflection::floor(settings))
}

/// Whether a filter of `kind_id` also reads its scene's shadow maps, so the engine draws
/// them for a scene chain that holds one even when no model receives them (ADR-0151
/// decision 6: a spot light's beam through fog is cut by what stands in it).
pub fn reads_shadows(kind_id: &str) -> bool {
    kind_id == volumetric::KIND
}

#[cfg(test)]
mod tests {
    use jstream_protocol::Settings;

    /// Every kind listed reads the depth, builds, and is listed in order.
    #[test]
    fn every_depth_kind_reads_depth_and_builds() {
        let kinds = super::descriptors();
        let ids: Vec<_> = kinds.iter().map(|kind| kind.kind_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "ambient_occlusion",
                "depth_bloom",
                "depth_fog",
                "floor_reflection",
                "volumetric_fog"
            ]
        );
        for id in ids {
            assert!(super::reads_depth(id), "{id}");
            assert!(super::build(id, &Settings::default()).is_some(), "{id}");
        }
        assert!(!super::reads_depth("blur"));
        assert!(super::reads_shadows("volumetric_fog"));
        assert!(!super::reads_shadows("depth_fog"));
        let floor = super::reflection_floor("floor_reflection", &Settings::default());
        assert_eq!(floor, Some(super::reflection::DEFAULT_FLOOR));
        assert_eq!(
            super::reflection_floor("depth_fog", &Settings::default()),
            None
        );
    }
}
