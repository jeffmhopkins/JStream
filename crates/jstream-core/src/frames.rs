//! Frame sheets (#1061): `video.frames` and `scene.switch_and_watch`.
//!
//! A sheet's pictures are taken in real time, so they show what the mix shows then:
//! a running transition, a stinger's video, a page's animation. A `jstream-frames`
//! thread per sheet wakes at each picture's due time and reads, under the collection
//! lock, what [`Engine`](crate::Engine) would draw for `video.snapshot` at that moment:
//! the published render snapshot (or the scene planned alone), the live values, `rev`
//! and the media clock's time ([`Sample`]). Reading is cheap and drawing is not, so the
//! thread only collects; once the last picture is due it hands every sample to the
//! snapshot thread as one job, which draws each at its time, scales it to a tile and
//! tiles them into one PNG ([`Sheet`]). A slow GPU then delays the answer, never a
//! picture. At most [`MAX_SHEETS`] sheets are taken at once.
//!
//! Docs: crates/jstream-core/README.md#design, crates/jstream-protocol/README.md#design

use std::sync::Arc;

use jstream_media::MediaTime;
use jstream_protocol::{CallResult, FrameSheet, ProtocolError, Rev, SheetFrame};

use crate::runtime::RenderSnapshot;
use crate::snapshot::{LiveParams, Pinned};

/// The most sheets taken at once, across every session.
pub(crate) const MAX_SHEETS: usize = 4;

/// The largest tile: a 16:9 canvas is scaled to exactly this.
const TILE_MAX: (u32, u32) = (480, 270);
/// The gap around and between tiles, in pixels.
const GAP: u32 = 4;
/// The sheet's background, behind the gaps and any empty tile at the end.
const BACKGROUND: [u8; 4] = [24, 24, 27, 255];

/// What one picture draws: as `video.snapshot` reads it, at one moment.
pub(crate) struct Sample {
    pub(crate) snapshot: Arc<RenderSnapshot>,
    /// What its frame slots held when it was read, which the picture shows (ADR-0117 §3).
    pub(crate) pinned: Pinned,
    pub(crate) live: Arc<LiveParams>,
    pub(crate) rev: Rev,
    pub(crate) at: MediaTime,
}

/// A sheet's samples, ready for the snapshot thread to draw.
pub(crate) struct Sheet {
    pub(crate) samples: Vec<Sample>,
    /// The media-clock time of the call (of the switch, for `scene.switch_and_watch`),
    /// which each picture's `at_ms` counts from.
    pub(crate) start: MediaTime,
    /// Wraps the finished sheet as the call's answer.
    pub(crate) answer: fn(FrameSheet) -> CallResult,
}

/// The answer to `video.frames`.
pub(crate) fn query_answer(sheet: FrameSheet) -> CallResult {
    CallResult::Query {
        result: jstream_protocol::QueryResult::VideoFrames { sheet },
    }
}

/// The answer to `scene.switch_and_watch`.
pub(crate) fn action_answer(sheet: FrameSheet) -> CallResult {
    CallResult::Action {
        result: Some(jstream_protocol::ActionResult::SceneSwitchAndWatch { sheet }),
    }
}

/// Where the tiles go: the grid is as square as it can be, wider than tall when it
/// can't, and each tile is the canvas scaled down to fit [`TILE_MAX`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) columns: u32,
    pub(crate) rows: u32,
    pub(crate) tile: (u32, u32),
    pub(crate) size: (u32, u32),
}

impl Layout {
    /// The layout of `count` tiles of a `canvas`-sized picture.
    pub(crate) fn new(count: u32, canvas: (u32, u32)) -> Self {
        let count = count.max(1);
        let mut columns = 1;
        while columns * columns < count {
            columns += 1;
        }
        let rows = count.div_ceil(columns);
        let tile = fit_tile(canvas);
        let size = (
            columns * tile.0 + (columns + 1) * GAP,
            rows * tile.1 + (rows + 1) * GAP,
        );
        Layout {
            columns,
            rows,
            tile,
            size,
        }
    }

    /// The top-left pixel of tile `index`.
    fn origin(&self, index: u32) -> (u32, u32) {
        let (column, row) = (index % self.columns, index / self.columns);
        (
            GAP + column * (self.tile.0 + GAP),
            GAP + row * (self.tile.1 + GAP),
        )
    }
}

/// `canvas` scaled down, keeping its shape, to fit inside [`TILE_MAX`]; never scaled up.
fn fit_tile((width, height): (u32, u32)) -> (u32, u32) {
    let (width, height) = (width.max(1), height.max(1));
    let (max_w, max_h) = TILE_MAX;
    if width <= max_w && height <= max_h {
        return (width, height);
    }
    // Compare max_w / width with max_h / height without floats.
    if u64::from(max_w) * u64::from(height) <= u64::from(max_h) * u64::from(width) {
        (max_w, scaled(height, max_w, width))
    } else {
        (scaled(width, max_h, height), max_h)
    }
}

/// `value × num / den`, rounded, at least 1.
fn scaled(value: u32, num: u32, den: u32) -> u32 {
    let n = (u64::from(value) * u64::from(num) + u64::from(den) / 2) / u64::from(den.max(1));
    u32::try_from(n).unwrap_or(u32::MAX).max(1)
}

/// Area-averaged downscale of tightly packed RGBA8, so thin lines and text in a tile
/// stay visible rather than dropping out as with nearest-neighbour. Upscaling isn't
/// needed (tiles are never larger than the canvas) and falls back to the nearest pixel.
pub(crate) fn downscale(src: (u32, u32), rgba: &[u8], dst: (u32, u32)) -> Vec<u8> {
    let (src_w, src_h) = (src.0 as usize, src.1 as usize);
    let (dst_w, dst_h) = (dst.0 as usize, dst.1 as usize);
    if src == dst {
        return rgba.to_vec();
    }
    let mut out = vec![0u8; dst_w * dst_h * 4];
    for y in 0..dst_h {
        let y0 = y * src_h / dst_h;
        let y1 = ((y + 1) * src_h / dst_h).max(y0 + 1).min(src_h);
        for x in 0..dst_w {
            let x0 = x * src_w / dst_w;
            let x1 = ((x + 1) * src_w / dst_w).max(x0 + 1).min(src_w);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = &rgba[(sy * src_w + x0) * 4..(sy * src_w + x1) * 4];
                for pixel in row.chunks_exact(4) {
                    for (total, &channel) in sum.iter_mut().zip(pixel) {
                        *total += u64::from(channel);
                    }
                }
            }
            let area = ((y1 - y0) * (x1 - x0)) as u64;
            let to = (y * dst_w + x) * 4;
            for (slot, total) in out[to..to + 4].iter_mut().zip(sum) {
                *slot = u8::try_from((total + area / 2) / area).unwrap_or(u8::MAX);
            }
        }
    }
    out
}

/// The sheet's pixels: `tiles` (each `layout.tile` sized, tightly packed RGBA8) placed in
/// order on the background.
pub(crate) fn assemble(layout: &Layout, tiles: &[Vec<u8>]) -> Vec<u8> {
    let (width, height) = (layout.size.0 as usize, layout.size.1 as usize);
    let mut out = BACKGROUND.repeat(width * height);
    let (tile_w, tile_h) = (layout.tile.0 as usize, layout.tile.1 as usize);
    for (index, tile) in (0u32..).zip(tiles) {
        let (left, top) = layout.origin(index);
        let (left, top) = (left as usize, top as usize);
        for row in 0..tile_h {
            let from = row * tile_w * 4;
            let to = ((top + row) * width + left) * 4;
            out[to..to + tile_w * 4].copy_from_slice(&tile[from..from + tile_w * 4]);
        }
    }
    out
}

/// Milliseconds from `start` to `at`, rounded; 0 if `at` is earlier.
pub(crate) fn at_ms(start: MediaTime, at: MediaTime) -> u32 {
    let ms = (at.as_secs_f64() - start.as_secs_f64()) * 1000.0;
    if ms.is_finite() && ms > 0.0 {
        // In range: a sheet spans at most 10 s.
        ms.round().min(f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

/// The finished [`FrameSheet`] for `samples` drawn as `tiles` on `layout`.
pub(crate) fn sheet(
    layout: &Layout,
    samples: &[Sample],
    start: MediaTime,
    png: String,
) -> FrameSheet {
    FrameSheet {
        width: layout.size.0,
        height: layout.size.1,
        columns: layout.columns,
        rows: layout.rows,
        tile_width: layout.tile.0,
        tile_height: layout.tile.1,
        frames: samples
            .iter()
            .map(|sample| SheetFrame {
                at_ms: at_ms(start, sample.at),
                rev: sample.rev,
            })
            .collect(),
        png,
    }
}

/// `too many sheets` for the [`MAX_SHEETS`]+1th at once.
pub(crate) fn busy() -> ProtocolError {
    ProtocolError::ActionFailed {
        reason: format!(
            "{MAX_SHEETS} frame sheets are being taken already; try again when one has answered"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_is_near_square_and_keeps_the_canvas_shape() {
        let hd = Layout::new(6, (1920, 1080));
        assert_eq!((hd.columns, hd.rows), (3, 2));
        assert_eq!(hd.tile, (480, 270));
        assert_eq!(hd.size, (3 * 480 + 4 * GAP, 2 * 270 + 3 * GAP));
        assert_eq!(Layout::new(2, (1920, 1080)).columns, 2);
        let full = Layout::new(16, (1920, 1080));
        assert_eq!((full.columns, full.rows), (4, 4));
        assert_eq!(Layout::new(7, (1920, 1080)).rows, 3);
        // A portrait canvas fits the height; a small one isn't scaled up.
        assert_eq!(Layout::new(2, (1080, 1920)).tile, (152, 270));
        assert_eq!(Layout::new(2, (320, 180)).tile, (320, 180));
        assert_eq!(Layout::new(2, (4096, 1080)).tile, (480, 127));
    }

    #[test]
    fn tiles_go_left_to_right_then_down() {
        let layout = Layout::new(4, (2, 1));
        assert_eq!(layout.tile, (2, 1));
        let tiles: Vec<Vec<u8>> = (1..=3u8).map(|n| [n, n, n, 255].repeat(2)).collect();
        let out = assemble(&layout, &tiles);
        let pixel = |x: u32, y: u32| {
            let at = ((y * layout.size.0 + x) * 4) as usize;
            out[at]
        };
        assert_eq!(pixel(GAP, GAP), 1);
        assert_eq!(pixel(GAP + 1, GAP), 1);
        assert_eq!(pixel(2 * GAP + 2, GAP), 2);
        assert_eq!(pixel(GAP, 2 * GAP + 1), 3);
        // The fourth tile is empty, and so are the gaps.
        assert_eq!(pixel(2 * GAP + 2, 2 * GAP + 1), BACKGROUND[0]);
        assert_eq!(pixel(0, 0), BACKGROUND[0]);
    }

    #[test]
    fn a_downscale_averages_so_a_thin_line_survives() {
        // A 4×1 picture with one white pixel, down to 1×1: a quarter grey, not black.
        let mut rgba = [0u8, 0, 0, 255].repeat(4);
        rgba[4..8].copy_from_slice(&[255, 255, 255, 255]);
        assert_eq!(downscale((4, 1), &rgba, (1, 1)), [64, 64, 64, 255]);
        assert_eq!(downscale((4, 1), &rgba, (4, 1)), rgba);
    }

    #[test]
    fn at_ms_counts_from_the_start() {
        let base = jstream_media::Timebase::new(1, 1000).expect("timebase");
        let start = MediaTime::new(5_000, base);
        assert_eq!(at_ms(start, MediaTime::new(5_600, base)), 600);
        assert_eq!(at_ms(start, MediaTime::new(4_000, base)), 0);
    }
}
