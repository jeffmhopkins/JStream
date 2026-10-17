//! The aspect ratio filter (roadmap V1a): changes the frame's shape before it is placed, so the source's reported size changes.

mod ratio;

use jstream_protocol::{KindDescriptor, Settings};

use crate::filter::Pass;

/// This group's kinds.
pub(crate) fn descriptors() -> Vec<KindDescriptor> {
    vec![ratio::descriptor()]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    match kind_id {
        ratio::KIND => Some(Box::new(ratio::AspectRatio::new(settings))),
        _ => None,
    }
}
