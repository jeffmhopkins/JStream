//! Built-in video filters (roadmap Lane V; ADR-0025 kind descriptors, ADR-0026 source
//! filter chains). Each kind is a `video_filter` descriptor plus a GPU pass that reads one
//! premultiplied linear image and writes another, possibly of a different size.
//!
//! This is the shared harness the filter groups build on. The chain runtime (V0) will
//! keep a [`VideoFilter`] per enabled filter of a source's video chain and run them in
//! order through [`crate::Compositor::apply_filter`]; until then nothing in the engine
//! calls it, and the kinds are not in `kind.list`.

#![allow(
    dead_code,
    reason = "Lane V: the harness lands before the filter groups that use it"
)]

mod aspect;
mod assets;
mod multipass;
mod pack;
mod params;
mod pass;
#[cfg(test)]
pub(crate) mod testing;

pub(crate) use pass::{Blend, Ctx, Draw, Pass, Runtime, Shader};

use jstream_protocol::{KindDescriptor, Settings};

/// A media-clock reading, for filters that move with time (scroll) or hold frames back
/// (video delay).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FilterClock {
    /// Seconds since the media clock's origin.
    pub seconds: f64,
}

/// Every built-in video filter kind, ordered by ID.
pub fn descriptors() -> Vec<KindDescriptor> {
    let mut kinds = pack::descriptors();
    kinds.extend(aspect::descriptors());
    kinds.extend(multipass::descriptors());
    kinds.extend(assets::descriptors());
    kinds.sort_by(|a, b| a.kind_id.cmp(&b.kind_id));
    kinds
}

/// The pass for a filter of `kind_id`, or `None` when it is not a built-in video kind.
fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    pack::build(kind_id, settings)
        .or_else(|| aspect::build(kind_id, settings))
        .or_else(|| multipass::build(kind_id, settings))
        .or_else(|| assets::build(kind_id, settings))
}

/// One built-in video filter with its settings read, ready to draw. It keeps whatever
/// state its kind needs between frames (a video delay's ring of frames).
pub struct VideoFilter {
    kind: String,
    pass: Box<dyn Pass>,
}

impl VideoFilter {
    /// The filter for `kind_id` with `settings`, complete as the graph stores them. A
    /// missing or mistyped setting reads as its default. `None` when `kind_id` is not a
    /// built-in video filter kind.
    pub fn new(kind_id: &str, settings: &Settings) -> Option<Self> {
        build(kind_id, settings).map(|pass| Self {
            kind: kind_id.to_owned(),
            pass,
        })
    }

    /// The filter's kind ID.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Applies new settings in place, keeping the filter's state.
    pub fn set_settings(&mut self, settings: &Settings) {
        self.pass.update(settings);
    }

    /// The size this filter draws for an input of `width` by `height`. Most filters keep
    /// the size; aspect ratio and padding filters change it, which changes the source's
    /// reported size for every placement.
    pub fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.pass.output_size(width, height)
    }

    pub(crate) fn pass_mut(&mut self) -> &mut dyn Pass {
        self.pass.as_mut()
    }
}

impl std::fmt::Debug for VideoFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFilter")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
