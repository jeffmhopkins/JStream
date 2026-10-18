//! Built-in video filters (roadmap Lane V; ADR-0025 kind descriptors, ADR-0026 source
//! filter chains). Each kind is a `video_filter` descriptor plus a GPU pass that reads one
//! premultiplied linear image and writes another, possibly of a different size.
//!
//! This is the shared harness the filter groups build on. `jstream-core`'s chain runtime
//! (V0) keeps a [`VideoFilter`] per enabled filter of a source's video chain and runs
//! them in order, each into a layer it keeps, through
//! [`crate::Compositor::apply_filter_into`]; [`descriptors`] is how its `kind.list`
//! gets these kinds.

mod aspect;
mod assets;
mod external;
mod multipass;
mod pack;
mod params;
mod pass;
#[cfg(test)]
pub(crate) mod testing;

pub use assets::{Lut, LutError, lut_image};
pub use external::ExternalFilter;
pub use pass::PRELUDE;
pub(crate) use pass::{Blend, Ctx, Draw, Pass, Runtime, Scratch, Shader};

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
/// state its kind needs between frames (a video delay's ring of frames), and a
/// multi-pass kind's intermediate layers ([`Scratch`]).
pub struct VideoFilter {
    kind: String,
    pass: Box<dyn Pass>,
    scratch: Scratch,
}

impl VideoFilter {
    /// The filter for `kind_id` with `settings`, complete as the graph stores them. A
    /// missing or mistyped setting reads as its default. `None` when `kind_id` is not a
    /// built-in video filter kind.
    pub fn new(kind_id: &str, settings: &Settings) -> Option<Self> {
        build(kind_id, settings).map(|pass| Self::of(kind_id, pass))
    }

    /// A filter of `kind` drawn by `pass`, with no layers kept yet.
    pub(crate) fn of(kind: &str, pass: Box<dyn Pass>) -> Self {
        Self {
            kind: kind.to_owned(),
            pass,
            scratch: Scratch::default(),
        }
    }

    /// A filter of a kind defined outside this crate (an effect pack), with `settings`
    /// complete as the graph stores them.
    pub fn external(
        kind_id: &str,
        filter: std::sync::Arc<dyn ExternalFilter>,
        settings: &Settings,
    ) -> Self {
        Self::of(kind_id, Box::new(external::External::new(filter, settings)))
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
    /// the size; aspect ratio reshapes it, which changes the source's reported size for
    /// every placement. An effect that draws outside its input (a drop shadow, an outside
    /// outline, a glow) keeps it, and what falls past the frame is cut (ADR-0065 §5).
    pub fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.pass.output_size(width, height)
    }

    /// Sets the image an asset or scene-reference filter reads (`apply_lut`'s table,
    /// `image_mask`'s mask, `scene_mask`'s scene layer), or clears it with `None`. The
    /// chain runtime resolves the reference and calls this whenever the image changes,
    /// for a scene every frame. Without one those kinds pass their input through; other
    /// kinds ignore it. The image must come from the compositor that draws the filter.
    pub fn set_reference(&mut self, image: Option<std::sync::Arc<crate::Image>>) {
        self.pass.set_reference(image);
    }

    /// Its pass and the pool its scratch layers come from, to draw it.
    pub(crate) fn parts(&mut self) -> (&mut dyn Pass, &mut Scratch) {
        (self.pass.as_mut(), &mut self.scratch)
    }

    /// The scratch layers it keeps, for tests.
    #[cfg(test)]
    pub(crate) fn scratch(&self) -> &Scratch {
        &self.scratch
    }
}

impl std::fmt::Debug for VideoFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFilter")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
