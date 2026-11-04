//! What the live sources run for a shader source of an effect pack (ADR-0093): a
//! stand-in whose picture is the program its chain draws first ([`crate::chain`]).
//!
//! `jstream-sources` has no pack kinds, so a source of kind `<pack>/<source>` runs a
//! [`StandIn`]: a transparent texel placed at the source's `width` by `height`. Its size
//! is what `source.size_changed` reports and what its items are laid out by. Only those
//! two settings build a new stand-in: the program takes the others live. A show makes a
//! new frame, so the plan sees one and the chain restarts the program
//! ([`crate::chain::Chain::of`]). While the pack isn't loaded, or is quarantined, no
//! program runs and the source draws nothing at its size.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0093-shader-sources-from-effect-packs.md

use std::sync::Arc;

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, Timebase,
    VideoFrame, VideoFrameInfo,
};
use jstream_protocol::Settings;
use jstream_sources::{Source, SourceError};

/// The size a stand-in takes when its settings have none in range: the manifest's
/// default fills them when the source is made, so only a hand-edited collection lacks
/// them.
const FALLBACK: (u32, u32) = (1920, 1080);

/// Whether `kind`, a source kind, is an effect pack's, `<pack>/<id>`: no built-in or
/// plugin source kind has a slash. (CLAP kinds, `clap/<id>`, are audio filter kinds, never
/// asked about here.)
pub(crate) fn is_pack_kind(kind: &str) -> bool {
    kind.contains('/')
}

/// The settings a pack source's stand-in is built from: its `width` and `height`. The
/// live sources compare these, so a change to any other setting builds nothing.
pub(crate) fn stand_in_settings(settings: &Settings) -> Settings {
    settings
        .iter()
        .filter(|(id, _)| matches!(&**id, "width" | "height"))
        .map(|(id, value)| (id.to_owned(), value.clone()))
        .collect()
}

/// The `width` and `height` in `settings`, each from 16 to 4096
/// ([`jstream_effects::SOURCE_SIDE`]), or [`FALLBACK`]'s.
fn size(settings: &Settings) -> (u32, u32) {
    let side = |id: &str, fallback: u32| {
        settings
            .get(id)
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| jstream_effects::SOURCE_SIDE.contains(v))
            .unwrap_or(fallback)
    };
    (side("width", FALLBACK.0), side("height", FALLBACK.1))
}

/// A pack source's live stand-in ([`Source`]).
pub(crate) struct StandIn {
    extent: FrameSize,
    frame: Arc<VideoFrame>,
}

impl StandIn {
    /// A stand-in at the size `settings` give.
    pub(crate) fn new(settings: &Settings) -> Result<Self, SourceError> {
        let (width, height) = size(settings);
        Ok(Self {
            extent: FrameSize::new(width, height)?,
            frame: Arc::new(texel()?),
        })
    }
}

/// One transparent RGBA8 texel.
fn texel() -> Result<VideoFrame, SourceError> {
    Ok(VideoFrame::new(
        VideoFrameInfo {
            size: FrameSize::new(1, 1)?,
            format: PixelFormat::Rgba8,
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts: MediaTime::zero(Timebase::MILLIS),
        },
        vec![Plane::new(vec![0; 4], 4)],
    )?)
}

impl Source for StandIn {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    /// A new frame, so the plan that shows it again restarts the program.
    fn show(&mut self, _: MediaTime) {
        if let Ok(frame) = texel() {
            self.frame = Arc::new(frame);
        }
    }

    /// As [`show`](Self::show): the program starts again (`source.restart`).
    fn restart(&mut self, at: MediaTime) {
        self.show(at);
    }

    /// Never called: the live sources build a new stand-in for a new size, and the
    /// program takes the other settings.
    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        *self = Self::new(settings)?;
        Ok(())
    }

    /// Its frames are placed at the source's size, whatever the density.
    fn set_density(&mut self, _: f32) -> Option<FrameSize> {
        Some(self.extent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(pairs: &[(&str, serde_json::Value)]) -> Settings {
        pairs
            .iter()
            .map(|(id, value)| ((*id).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn a_stand_in_is_placed_at_its_size_and_a_show_makes_a_new_frame() {
        let mut stand_in =
            StandIn::new(&settings(&[("width", json!(640)), ("height", json!(360))]))
                .expect("made");
        assert_eq!(
            stand_in.set_density(2.0),
            Some(FrameSize::new(640, 360).expect("size"))
        );
        let at = MediaTime::zero(Timebase::MILLIS);
        let first = stand_in.frame(at).expect("frame");
        assert_eq!(first.info().size, FrameSize::new(1, 1).expect("size"));
        assert!(Arc::ptr_eq(&first, &stand_in.frame(at).expect("frame")));
        stand_in.show(at);
        assert!(!Arc::ptr_eq(&first, &stand_in.frame(at).expect("frame")));
        // Out of range falls back.
        let mut odd = StandIn::new(&settings(&[("width", json!(8))])).expect("made");
        assert_eq!(
            odd.set_density(1.0),
            Some(FrameSize::new(1920, 1080).expect("size"))
        );
    }

    #[test]
    fn only_the_size_builds_a_stand_in() {
        let all = settings(&[
            ("width", json!(64)),
            ("height", json!(32)),
            ("speed", json!(2.0)),
        ]);
        assert_eq!(
            stand_in_settings(&all),
            settings(&[("width", json!(64)), ("height", json!(32))])
        );
        assert!(is_pack_kind("space/warp"));
        assert!(!is_pack_kind("color"));
    }
}
