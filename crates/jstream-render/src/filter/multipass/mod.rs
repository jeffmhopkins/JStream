//! The multi-pass filters (roadmap V2): blur, pixelate, drop shadow, outline, glow and video delay.

use jstream_protocol::{KindDescriptor, Settings};

use crate::filter::Pass;

/// This group's kinds.
pub(crate) fn descriptors() -> Vec<KindDescriptor> {
    Vec::new()
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    let _ = (kind_id, settings);
    None
}
