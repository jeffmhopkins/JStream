//! Depth-aware scene filters (ADR-0137 decision 5): kinds that read a scene's view depth
//! (`Compositor::render_view_depth`) beside their input.
//!
//! The chain runtime hands a scene chain's depth-aware filters that scene's view depth
//! through [`crate::VideoFilter::set_depth`] when the scene has a camera. Anywhere else a
//! filter has none, and passes its input through unchanged.
//!
//! These kinds are built by [`build`] but not listed by [`super::descriptors`]: their
//! own list is [`descriptors`], which the engine adds to its kinds once it produces the
//! view depth for them, so no chain holds a depth filter that could never see depth.

mod fog;

use std::sync::Arc;

use jstream_protocol::{KindDescriptor, Settings};

use crate::filter::Pass;
use crate::image::Image;

/// A scene's view depth, as a depth-aware filter reads it (ADR-0137 decisions 4 and 5).
#[derive(Clone, Debug)]
pub struct ViewDepth {
    /// From [`crate::Compositor::render_view_depth`]: red holds `1 / w = f / D`, 0 where
    /// nothing was drawn. The size of the scene's canvas.
    pub image: Arc<Image>,
    /// The camera's focal length `f`, in canvas pixels
    /// ([`jstream_protocol::CameraView::focal_px`]), which turns `1 / w` back into a
    /// distance.
    pub focal_px: f32,
}

/// The depth-aware filter kinds, ordered by ID.
pub fn descriptors() -> Vec<KindDescriptor> {
    vec![fog::descriptor()]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    match kind_id {
        fog::KIND => Some(Box::new(fog::DepthFog::new(settings))),
        _ => None,
    }
}

/// Whether a filter of `kind_id` reads a scene's view depth, so the engine draws it for
/// a scene chain that holds one (ADR-0137 decision 4).
pub fn reads_depth(kind_id: &str) -> bool {
    kind_id == fog::KIND
}
