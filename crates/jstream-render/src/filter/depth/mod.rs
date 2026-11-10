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

mod fog;
mod occlusion;
mod volumetric;

use std::sync::Arc;

use jstream_protocol::{CameraView, KindDescriptor, Settings};

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
        }
    }
}

/// The depth-aware filter kinds, ordered by ID.
pub fn descriptors() -> Vec<KindDescriptor> {
    vec![
        occlusion::descriptor(),
        fog::descriptor(),
        volumetric::descriptor(),
    ]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    match kind_id {
        fog::KIND => Some(Box::new(fog::DepthFog::new(settings))),
        occlusion::KIND => Some(Box::new(occlusion::AmbientOcclusion::new(settings))),
        volumetric::KIND => Some(Box::new(volumetric::VolumetricFog::new(settings))),
        _ => None,
    }
}

/// Whether a filter of `kind_id` reads a scene's view depth, so the engine draws it for
/// a scene chain that holds one (ADR-0143 decision 4).
pub fn reads_depth(kind_id: &str) -> bool {
    [fog::KIND, occlusion::KIND, volumetric::KIND].contains(&kind_id)
}

#[cfg(test)]
mod tests {
    use jstream_protocol::Settings;

    /// Every kind listed reads the depth, builds, and is listed in order.
    #[test]
    fn every_depth_kind_reads_depth_and_builds() {
        let kinds = super::descriptors();
        let ids: Vec<_> = kinds.iter().map(|kind| kind.kind_id.as_str()).collect();
        assert_eq!(ids, ["ambient_occlusion", "depth_fog", "volumetric_fog"]);
        for id in ids {
            assert!(super::reads_depth(id), "{id}");
            assert!(super::build(id, &Settings::default()).is_some(), "{id}");
        }
        assert!(!super::reads_depth("blur"));
    }
}
