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
mod external;
mod multipass;
mod pack;
mod params;
mod pass;
#[cfg(test)]
pub(crate) mod testing;

pub use external::ExternalFilter;
pub use pass::PRELUDE;
pub(crate) use pass::{Blend, Ctx, Draw, Pass, Runtime, Shader};

use jstream_protocol::{KindDescriptor, Settings};

/// A media-clock reading, for filters that move with time (scroll) or hold frames back
/// (video delay).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FilterClock {
    /// Seconds since the media clock's origin.
    pub seconds: f64,
}

/// Where a source's own frame sits in a filtered image, in the filtered image's pixels.
///
/// Drop shadow, outside outline and glow draw past the frame: their output grows and the
/// input is drawn 1:1 at an inset, so the box is smaller than the output. Every other
/// kind reports its whole output, including aspect ratio, whose output is the reshaped
/// frame. Placement fits this box to the item's bounds, so a shadow hangs outside them
/// instead of shrinking the content (#369).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentBox {
    /// Left edge, px from the output's left.
    pub x: u32,
    /// Top edge, px from the output's top.
    pub y: u32,
    /// Width, px.
    pub width: u32,
    /// Height, px.
    pub height: u32,
}

impl ContentBox {
    /// All of a `(width, height)` image.
    pub fn whole((width, height): (u32, u32)) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// This box, in the output of the next filter of a chain, whose own box for this
    /// filter's `(width, height)` output is `next`. When `next` holds that output 1:1
    /// (its size is the output's), the offsets add up. Otherwise the next filter
    /// reshaped or resampled the frame (aspect ratio), and its box becomes the content.
    pub fn then(self, output: (u32, u32), next: ContentBox) -> Self {
        if (next.width, next.height) != output {
            return next;
        }
        Self {
            x: next.x.saturating_add(self.x),
            y: next.y.saturating_add(self.y),
            width: self.width,
            height: self.height,
        }
    }
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

    /// A filter of a kind defined outside this crate (an effect pack), with `settings`
    /// complete as the graph stores them.
    pub fn external(
        kind_id: &str,
        filter: std::sync::Arc<dyn ExternalFilter>,
        settings: &Settings,
    ) -> Self {
        Self {
            kind: kind_id.to_owned(),
            pass: Box::new(external::External::new(filter, settings)),
        }
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
    /// the size; some change it (for example aspect ratio, padding, a drop shadow, an
    /// outside outline or a glow), which changes the source's reported size for every
    /// placement.
    pub fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.pass.output_size(width, height)
    }

    /// Where the input's frame sits in what this filter draws for an input of `width` by
    /// `height` ([`ContentBox`]). A chain composes its filters' boxes with
    /// [`ContentBox::then`].
    pub fn content_box(&self, width: u32, height: u32) -> ContentBox {
        self.pass.content_box(width, height)
    }

    /// Sets the image an asset or scene-reference filter reads (`apply_lut`'s table,
    /// `image_mask`'s mask, `scene_mask`'s scene layer), or clears it with `None`. The
    /// chain runtime resolves the reference and calls this whenever the image changes,
    /// for a scene every frame. Without one those kinds pass their input through; other
    /// kinds ignore it. The image must come from the compositor that draws the filter.
    pub fn set_reference(&mut self, image: Option<std::sync::Arc<crate::Image>>) {
        self.pass.set_reference(image);
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

#[cfg(test)]
mod tests {
    use super::ContentBox;
    use crate::filter::testing::filter;
    use serde_json::json;

    fn at(x: u32, y: u32, width: u32, height: u32) -> ContentBox {
        ContentBox {
            x,
            y,
            width,
            height,
        }
    }

    /// Filters that keep the frame or reshape it report their whole output.
    #[test]
    fn most_kinds_report_their_whole_output() {
        let blur = filter("blur", json!({}));
        assert_eq!(blur.content_box(16, 9), ContentBox::whole((16, 9)));
        let pad = filter("aspect_ratio", json!({"ratio": "1:1", "mode": "pad"}));
        assert_eq!(pad.output_size(16, 9), (16, 16));
        assert_eq!(pad.content_box(16, 9), ContentBox::whole((16, 16)));
    }

    /// Down a chain, boxes drawn 1:1 add up, and a reshaping filter's box replaces them.
    #[test]
    fn boxes_compose_down_a_chain() {
        let outline = filter("outline", json!({"width": 2.0}));
        let shadow = filter(
            "drop_shadow",
            json!({"offset_x": -3.0, "offset_y": 0.0, "blur": 0.0}),
        );
        let first = outline.content_box(10, 10);
        assert_eq!(first, at(2, 2, 10, 10));
        let mid = outline.output_size(10, 10);
        assert_eq!(mid, (14, 14));
        let chained = first.then(mid, shadow.content_box(mid.0, mid.1));
        assert_eq!(chained, at(5, 2, 10, 10));
        // A size-keeping filter after it changes nothing.
        let end = shadow.output_size(mid.0, mid.1);
        let blur = filter("blur", json!({}));
        assert_eq!(chained.then(end, blur.content_box(end.0, end.1)), chained);
        // A crop to 1:1 reshapes the frame: its whole output is the content.
        let crop = filter("aspect_ratio", json!({"ratio": "1:1", "mode": "crop"}));
        let cropped = crop.content_box(end.0, end.1);
        assert_eq!(cropped, ContentBox::whole((14, 14)));
        assert_eq!(chained.then(end, cropped), cropped);
    }
}
