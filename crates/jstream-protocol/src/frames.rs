//! Frame sheets (since 0.76, #1061): pictures of the program mix or a scene taken at an
//! interval and tiled into one PNG, so an agent sees a transition, a stinger or a move
//! in one call. `video.frames` takes one of what is on screen now; the
//! `scene.switch_and_watch` action switches first and takes it from the switch on.

use crate::error::ProtocolError;
use crate::id::Rev;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most pictures one sheet holds.
pub const MAX_SHEET_FRAMES: u32 = 16;
/// The shortest interval between two pictures, in milliseconds.
pub const MIN_SHEET_INTERVAL_MS: u32 = 10;
/// The longest a sheet may take, from the call to its last picture, in milliseconds.
pub const MAX_SHEET_SPAN_MS: u32 = 10_000;

/// When to take a frame sheet's pictures: `count` of them, `interval_ms` apart, the
/// first `delay_ms` after the call (after the switch, for `scene.switch_and_watch`).
/// `delay_ms + (count - 1) × interval_ms` is at most [`MAX_SHEET_SPAN_MS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FrameStrip {
    /// How many pictures, 2 to 16.
    pub count: u32,
    /// The time between two pictures, in milliseconds, at least 10.
    pub interval_ms: u32,
    /// How long to wait before the first picture, in milliseconds. Default 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_ms: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl FrameStrip {
    /// `count` pictures `interval_ms` apart, starting at once.
    pub fn new(count: u32, interval_ms: u32) -> Self {
        FrameStrip {
            count,
            interval_ms,
            delay_ms: 0,
        }
    }

    /// Checks the limits, naming the field at fault: `invalid_argument` for a `count`
    /// outside 2 to 16, an `interval_ms` under 10, or a sheet that would take longer
    /// than [`MAX_SHEET_SPAN_MS`] (on `interval_ms`, or `delay_ms` when the delay alone
    /// is too long).
    pub fn check(&self) -> Result<(), ProtocolError> {
        if !(2..=MAX_SHEET_FRAMES).contains(&self.count) {
            return Err(ProtocolError::invalid_argument(
                "count",
                format!("a frame sheet takes 2 to {MAX_SHEET_FRAMES} pictures"),
            ));
        }
        if self.interval_ms < MIN_SHEET_INTERVAL_MS {
            return Err(ProtocolError::invalid_argument(
                "interval_ms",
                format!("pictures are at least {MIN_SHEET_INTERVAL_MS} ms apart"),
            ));
        }
        if self.delay_ms > MAX_SHEET_SPAN_MS {
            return Err(ProtocolError::invalid_argument(
                "delay_ms",
                format!("a frame sheet takes at most {MAX_SHEET_SPAN_MS} ms"),
            ));
        }
        if self.span_ms() > u64::from(MAX_SHEET_SPAN_MS) {
            return Err(ProtocolError::invalid_argument(
                "interval_ms",
                format!(
                    "a frame sheet takes at most {MAX_SHEET_SPAN_MS} ms, delay included; \
                     take fewer pictures or a shorter interval"
                ),
            ));
        }
        Ok(())
    }

    /// When picture `index` is due, in milliseconds after the call.
    pub fn offset_ms(&self, index: u32) -> u64 {
        u64::from(self.delay_ms) + u64::from(index) * u64::from(self.interval_ms)
    }

    /// When the last picture is due, in milliseconds after the call.
    pub fn span_ms(&self) -> u64 {
        self.offset_ms(self.count.saturating_sub(1))
    }
}

/// A frame sheet: the pictures, each scaled down to one tile, laid out left to right
/// and top to bottom in time order, on a dark background with a gap between tiles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrameSheet {
    /// The PNG's width in pixels.
    pub width: u32,
    /// The PNG's height in pixels.
    pub height: u32,
    /// Tiles per row.
    pub columns: u32,
    /// Rows of tiles.
    pub rows: u32,
    /// One tile's width in pixels: the canvas scaled down to fit 480 × 270.
    pub tile_width: u32,
    /// One tile's height in pixels.
    pub tile_height: u32,
    /// Each picture, in the order the tiles show them.
    pub frames: Vec<SheetFrame>,
    /// The PNG file, standard base64 with padding and no line breaks.
    pub png: String,
}

/// One picture of a [`FrameSheet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SheetFrame {
    /// When it was taken, in milliseconds after the call (after the switch, for
    /// `scene.switch_and_watch`), on the clock transitions run on. It is the time the
    /// picture shows, which can lag the time asked for by a few milliseconds.
    pub at_ms: u32,
    /// The collection revision of the layout it shows.
    pub rev: Rev,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limits_name_the_field_at_fault() {
        let field = |strip: FrameStrip| match strip.check() {
            Err(ProtocolError::InvalidArgument { field, .. }) => Some(field),
            _ => None,
        };
        assert_eq!(field(FrameStrip::new(2, 10)), None);
        assert_eq!(field(FrameStrip::new(16, 666)), None);
        assert_eq!(field(FrameStrip::new(1, 100)).as_deref(), Some("count"));
        assert_eq!(field(FrameStrip::new(17, 100)).as_deref(), Some("count"));
        assert_eq!(field(FrameStrip::new(4, 9)).as_deref(), Some("interval_ms"));
        assert_eq!(
            field(FrameStrip::new(16, 667)).as_deref(),
            Some("interval_ms")
        );
        let late = FrameStrip {
            delay_ms: 9_000,
            ..FrameStrip::new(3, 600)
        };
        assert_eq!(field(late).as_deref(), Some("interval_ms"));
        let later = FrameStrip {
            delay_ms: 10_001,
            ..FrameStrip::new(2, 10)
        };
        assert_eq!(field(later).as_deref(), Some("delay_ms"));
        assert_eq!(late.offset_ms(2), 10_200);
    }
}
