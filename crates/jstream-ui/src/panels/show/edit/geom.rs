//! Where the Edit view draws things: clips and markers across the view, the bookmark
//! flags along the bottom, the zoom bar's overview, thumb and grips, and what the wheel
//! does. Pure functions of the [`ShowView`] and the [`Viewport`], tested natively.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::super::model::{BookmarkView, ClipKind, RunPhase, RunView, ShowView, TrackKind};
use super::super::time::{Viewport, clock};

/// The track heads' width, in px: the column left of the rows, the ruler and the flags.
pub const HEAD_PX: f64 = 130.0;

/// How many colours program clips cycle through (`show-clip-c0` to `-c4` in style.css).
pub const PROGRAM_COLOURS: usize = 5;

/// The least room a squeezed bookmark flag keeps, in px: enough for its pause symbol.
const MIN_FLAG_PX: f64 = 22.0;

/// The gap kept between two bookmark flags, in px.
const FLAG_GAP_PX: f64 = 3.0;

/// How wide each grip of the zoom bar's thumb is, in px, from the thumb's end inwards.
const GRIP_PX: f64 = 20.0;

/// How far outside the thumb a press still takes its nearer grip, in px.
const GRIP_SLOP_PX: f64 = 4.0;

/// Where a span of show time sits across the view, as percentages of the view's width:
/// its left edge and its width. `None` when none of it is in view.
pub fn span_pct(view: Viewport, start_ms: u32, end_ms: u32) -> Option<(f64, f64)> {
    let (start, end) = (f64::from(start_ms), f64::from(end_ms));
    if end < view.start_ms || start > view.end_ms() {
        return None;
    }
    Some((
        (start - view.start_ms) / view.span_ms * 100.0,
        (end - start) / view.span_ms * 100.0,
    ))
}

/// Where a point in show time sits across the view, as a percentage of its width.
/// `None` when it is out of view.
pub fn at_pct(view: Viewport, at_ms: u32) -> Option<f64> {
    let at = f64::from(at_ms);
    (view.start_ms..=view.end_ms())
        .contains(&at)
        .then(|| (at - view.start_ms) / view.span_ms * 100.0)
}

/// A row's height in px, by what its track holds.
pub fn row_px(kind: TrackKind) -> u32 {
    match kind {
        TrackKind::Program => 80,
        TrackKind::Overlay(_) => 40,
        TrackKind::Keyframes => 34,
        TrackKind::Sequences(_) => 46,
        TrackKind::Audio(_) => 42,
    }
}

/// Whether a row starts a new group of tracks, with a heavier line above it: pictures
/// (overlays and program), then control (keyframes and sequences), then audio.
pub fn starts_group(previous: Option<TrackKind>, kind: TrackKind) -> bool {
    let group = |k: TrackKind| match k {
        TrackKind::Program | TrackKind::Overlay(_) => 0,
        TrackKind::Keyframes | TrackKind::Sequences(_) => 1,
        TrackKind::Audio(_) => 2,
    };
    previous.is_some_and(|p| group(p) != group(kind))
}

/// Whether a jump to `at_ms` lands inside a timed clip (one that holds a video or audio
/// file), so that it seeks into the file: its flag's "seeks" tag.
pub fn seeks(show: &ShowView, at_ms: u32) -> bool {
    show.tracks
        .iter()
        .flat_map(|t| &t.clips)
        .any(|c| c.timed && c.start_ms < at_ms && at_ms < c.end_ms())
}

/// What the next Continue brings, for "Next:" in the bar: every clip that starts at the
/// first start from `from_ms` on (after it, or at it too when `at_too`), in track order.
pub fn next_up(show: &ShowView, from_ms: u32, at_too: bool) -> Vec<String> {
    let ahead = |start: u32| start > from_ms || (at_too && start == from_ms);
    let Some(first) = show
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| c.start_ms)
        .filter(|s| ahead(*s))
        .min()
    else {
        return Vec::new();
    };
    show.tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(move |c| (t.kind, c)))
        .filter(|(_, c)| c.start_ms == first)
        .map(|(kind, c)| match (&c.kind, kind) {
            (ClipKind::Keyframe(_), _) => format!("keyframe {}", c.label),
            (ClipKind::Scene(_), TrackKind::Program) => format!("scene {}", c.label),
            _ => c.label.clone(),
        })
        .collect()
}

/// The play head's flag on the ruler: "5:30", "5:30 · paused 2:14" or "1:20 · pass 3".
pub fn head_text(run: &RunView, head_ms: u32, now: f64) -> String {
    let at = clock(head_ms);
    match (&run.phase, &run.looping) {
        (RunPhase::Paused { since, .. }, _) => {
            format!("{at} · paused {}", clock((now - since).max(0.0) as u32))
        }
        (RunPhase::Playing, Some((_, pass))) => format!("{at} · pass {pass}"),
        _ => at,
    }
}

/// About how wide a bookmark's flag is, in px, before any squeezing: its name in the
/// flag's mono face, the pause symbol, the key cap and the "seeks" tag.
pub fn flag_px(bookmark: &BookmarkView, seeks: bool) -> f64 {
    const CHAR_PX: f64 = 6.1;
    let mut chars = bookmark.name.chars().count();
    if bookmark.pausing {
        chars += 3;
    }
    let mut px = 12.0 + chars as f64 * CHAR_PX;
    if let Some(key) = &bookmark.key {
        px += 11.0 + key.chars().count() as f64 * 5.5;
    }
    if seeks {
        px += 40.0;
    }
    px
}

/// Where a bookmark flag goes along the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlagPlace {
    /// Its left edge, in px across the rows.
    pub left: f64,
    /// The most it may be wide, in px; a flag with less room than it needs is cut short.
    pub max_width: f64,
    /// Whether it hangs to the left of its line (the last flag, at the right edge).
    pub flipped: bool,
}

/// Places the bookmark flags along the bottom without overlap. `flags` are each flag's
/// line, in px across rows `width` px wide, and its natural width, in time order. A flag
/// hangs right of its line, cut short where the next line comes first; the last flag
/// hangs left of its line instead when it would run off the right edge and there is room.
pub fn place_flags(flags: &[(f64, f64)], width: f64) -> Vec<FlagPlace> {
    let mut placed: Vec<FlagPlace> = Vec::with_capacity(flags.len());
    for (n, &(x, natural)) in flags.iter().enumerate() {
        let next = flags.get(n + 1).map_or(width, |f| f.0);
        let room = next
            - x
            - if n + 1 < flags.len() {
                FLAG_GAP_PX
            } else {
                0.0
            };
        let previous_end = placed.last().map_or(0.0, |p| p.left + p.max_width);
        let place = if natural <= room {
            FlagPlace {
                left: x,
                max_width: natural,
                flipped: false,
            }
        } else if n + 1 == flags.len() && x - natural >= previous_end + FLAG_GAP_PX {
            FlagPlace {
                left: x - natural,
                max_width: natural,
                flipped: true,
            }
        } else {
            FlagPlace {
                left: x,
                max_width: room.max(MIN_FLAG_PX),
                flipped: false,
            }
        };
        placed.push(place);
    }
    placed
}

/// One coloured stretch of the zoom bar's overview: a program clip, as fractions of the
/// whole show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    /// Where it starts, as a fraction of the bar.
    pub left: f64,
    /// How much of the bar it covers.
    pub width: f64,
    /// Its program colour, from 0 to [`PROGRAM_COLOURS`] − 1.
    pub colour: usize,
}

/// The zoom bar's overview: the program clips in their colours over a show `len_ms` long.
pub fn overview(show: &ShowView, len_ms: f64) -> Vec<Segment> {
    let len = len_ms.max(1.0);
    show.program()
        .iter()
        .enumerate()
        .map(|(n, c)| Segment {
            left: f64::from(c.start_ms) / len,
            width: f64::from(c.len_ms) / len,
            colour: n % PROGRAM_COLOURS,
        })
        .collect()
}

/// Which part of the zoom bar a press lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    /// The thumb's left handle: a drag moves the view's start.
    Left,
    /// The thumb's right handle: a drag moves the view's end.
    Right,
    /// The thumb's middle: a drag scrolls.
    Middle,
    /// The bar outside the thumb: a press brings the view there.
    Outside,
}

/// What a press `x` px along a zoom bar `width` px wide lands on. Each handle takes the
/// last [`GRIP_PX`] of its end of the thumb, and a narrow thumb is split between them.
pub fn grip_at(view: Viewport, x: f64, width: f64) -> Grip {
    let (a, b) = view.thumb();
    let (left, right) = (a * width, b * width);
    if x < left - GRIP_SLOP_PX || x > right + GRIP_SLOP_PX {
        return Grip::Outside;
    }
    let middle = (left + right) / 2.0;
    if x <= (left + GRIP_PX).min(middle) {
        Grip::Left
    } else if x >= (right - GRIP_PX).max(middle) {
        Grip::Right
    } else {
        Grip::Middle
    }
}

/// A drag on the zoom bar, from where it began.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomDrag {
    /// What the press took.
    pub grip: Grip,
    /// Where the press was, in px along the bar.
    pub from_x: f64,
    /// The view when it began.
    pub from: Viewport,
}

impl ZoomDrag {
    /// The view with the pointer now `x` px along a bar `width` px wide: a handle zooms
    /// from the other end, the middle scrolls.
    pub fn at(self, x: f64, width: f64) -> Viewport {
        let moved = (x - self.from_x) / width.max(1.0);
        let (a, b) = self.from.thumb();
        match self.grip {
            Grip::Left => self.from.with_left(a + moved),
            Grip::Right => self.from.with_right(b + moved),
            Grip::Middle | Grip::Outside => self.from.scrolled(moved * self.from.len_ms),
        }
    }
}

/// The view centred on the time `x` px along a zoom bar `width` px wide, at the same
/// zoom: a press on the bar outside the thumb.
pub fn centred_at(view: Viewport, x: f64, width: f64) -> Viewport {
    let at = x / width.max(1.0) * view.len_ms;
    view.scrolled(at - (view.start_ms + view.span_ms / 2.0))
}

/// What a wheel over the rows does to the view, `x` px across rows `width` px wide:
/// Ctrl zooms about the pointer, Shift (or a sideways wheel) scrolls. `None` for a plain
/// wheel, which scrolls the rows up and down.
pub fn wheeled(
    view: Viewport,
    (dx, dy): (f64, f64),
    ctrl: bool,
    shift: bool,
    x: f64,
    width: f64,
) -> Option<Viewport> {
    let per_px = view.span_ms / width.max(1.0);
    if ctrl {
        Some(view.zoom_at((dy * 0.0015).exp(), x, width))
    } else if shift {
        let d = if dy == 0.0 { dx } else { dy };
        Some(view.scrolled(d * per_px))
    } else if dx.abs() > dy.abs() {
        Some(view.scrolled(dx * per_px))
    } else {
        None
    }
}

/// A sequence clip's curve as an SVG path in a `0 0 100 100` box stretched over the clip.
pub fn curve_path(curve: &[(f32, f32)]) -> String {
    curve
        .iter()
        .enumerate()
        .map(|(n, (f, v))| {
            let x = f64::from(f.clamp(0.0, 1.0)) * 100.0;
            let y = (1.0 - f64::from(v.clamp(0.0, 1.0))) * 100.0;
            format!("{}{x:.2} {y:.2}", if n == 0 { 'M' } else { 'L' })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// An audio clip's level as an SVG path in a `0 0 1000 100` box stretched over the clip:
/// up over its fade in, flat, down over its fade out. Closed for the fill when `closed`.
pub fn fade_path(len_ms: u32, (fade_in, fade_out): (u32, u32), closed: bool) -> String {
    let len = f64::from(len_ms.max(1));
    let up = (f64::from(fade_in) / len * 1000.0).min(1000.0);
    let down = (1000.0 - f64::from(fade_out) / len * 1000.0).max(up);
    let path = format!("M0 100 L{up:.1} 4 L{down:.1} 4 L1000 100");
    if closed { format!("{path} Z") } else { path }
}

/// How near a clip's or a loop bar's end a press takes that end, in px.
pub const EDGE_PX: f64 = 6.0;

/// How tall the band along a clip's top is where its fade handles sit, in px.
pub const FADE_BAND_PX: f64 = 12.0;

/// How near a fade handle a press in the band takes it, in px.
const FADE_GRAB_PX: f64 = 7.0;

/// Which part of a clip or a loop bar a press takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The middle: a drag moves it.
    Body,
    /// Its left end: a drag moves its start.
    Start,
    /// Its right end: a drag moves its end.
    End,
    /// The handle at the end of its fade in.
    FadeIn,
    /// The handle at the start of its fade out.
    FadeOut,
}

/// What a press `(x, y)` px into a clip `width` px wide takes. `fades` are where its fade
/// handles sit, in px from its left, for a clip that has fades. A program clip has only a
/// right end to drag: its left end is the clip before's right end.
pub fn clip_part(x: f64, y: f64, width: f64, fades: Option<(f64, f64)>, program: bool) -> Part {
    if let Some((fade_in, fade_out)) = fades
        && y <= FADE_BAND_PX
    {
        let (din, dout) = ((x - fade_in).abs(), (x - fade_out).abs());
        if din <= FADE_GRAB_PX || dout <= FADE_GRAB_PX {
            return if din <= dout {
                Part::FadeIn
            } else {
                Part::FadeOut
            };
        }
    }
    // A narrow clip keeps a middle to grab.
    let edge = EDGE_PX.min(width / 4.0);
    if x >= width - edge {
        Part::End
    } else if x <= edge && !program {
        Part::Start
    } else {
        Part::Body
    }
}

/// What a press `x` px along a loop's bar `width` px wide takes: an end, or the middle.
pub fn loop_part(x: f64, width: f64) -> Part {
    let edge = EDGE_PX.min(width / 4.0);
    if x <= edge {
        Part::Start
    } else if x >= width - edge {
        Part::End
    } else {
        Part::Body
    }
}

/// Whether a press `x` px into a clip `width` px wide is nearer its right end than its
/// left: which end the Trim tool takes.
pub fn nearer_end(x: f64, width: f64) -> bool {
    x * 2.0 >= width
}

/// Where the fade handles of a clip `len_ms` long and `width` px wide sit, in px from
/// its left: at the end of its fade in and at the start of its fade out.
pub fn fade_handles(len_ms: u32, (fade_in, fade_out): (u32, u32), width: f64) -> (f64, f64) {
    let per = width / f64::from(len_ms.max(1));
    let a = f64::from(fade_in) * per;
    let b = (width - f64::from(fade_out) * per).max(a);
    (a, b)
}

/// How long the Edit view takes a show `len_ms` long to be: the show and a tenth more
/// (at least 10 s), so there is room after its end to drop the next clip.
pub fn padded_len(len_ms: u32) -> u32 {
    len_ms.saturating_add((len_ms / 10).max(10_000))
}

/// Where the "Drop a scene or a file" target sits along V1, as a percentage of the view:
/// right after program's last clip, or at the view's right edge when that is out of
/// view to the right. `None` when program ends left of the view.
pub fn drop_slot_pct(view: Viewport, program_end_ms: u32) -> Option<f64> {
    let end = f64::from(program_end_ms);
    if end < view.start_ms {
        return Some(0.0);
    }
    if end > view.end_ms() {
        return None;
    }
    Some((end - view.start_ms) / view.span_ms * 100.0)
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{RunPhase, demo};
    use super::*;

    #[test]
    fn a_press_on_a_clip_takes_its_end_its_fade_or_its_middle() {
        // An audio clip 200 px wide with handles at 20 and 160 px.
        let fades = Some((20.0, 160.0));
        assert_eq!(clip_part(198.0, 30.0, 200.0, fades, false), Part::End);
        assert_eq!(clip_part(2.0, 30.0, 200.0, fades, false), Part::Start);
        assert_eq!(clip_part(100.0, 30.0, 200.0, fades, false), Part::Body);
        assert_eq!(clip_part(22.0, 4.0, 200.0, fades, false), Part::FadeIn);
        assert_eq!(clip_part(157.0, 4.0, 200.0, fades, false), Part::FadeOut);
        assert_eq!(clip_part(100.0, 4.0, 200.0, fades, false), Part::Body);
        // A program clip has no left end to drag.
        assert_eq!(clip_part(2.0, 30.0, 200.0, None, true), Part::Body);
        assert_eq!(clip_part(197.0, 30.0, 200.0, None, true), Part::End);
        // A clip 8 px wide still has a middle.
        assert_eq!(clip_part(4.0, 30.0, 8.0, None, false), Part::Body);
        assert_eq!(loop_part(3.0, 300.0), Part::Start);
        assert_eq!(loop_part(298.0, 300.0), Part::End);
        assert_eq!(loop_part(150.0, 300.0), Part::Body);
        assert!(nearer_end(120.0, 200.0));
        assert!(!nearer_end(40.0, 200.0));
    }

    #[test]
    fn fade_handles_and_the_drop_slot_sit_in_time() {
        assert_eq!(fade_handles(10_000, (1000, 2000), 1000.0), (100.0, 800.0));
        assert_eq!(fade_handles(1000, (800, 800), 100.0), (80.0, 80.0));
        let view = fit();
        assert_eq!(drop_slot_pct(view, 330_000), Some(50.0));
        let late = view.with_left(0.9);
        assert_eq!(drop_slot_pct(late, 330_000), Some(0.0));
        let early = view.with_right(0.1);
        assert_eq!(drop_slot_pct(early, 330_000), None);
        assert_eq!(padded_len(0), 10_000);
        assert_eq!(padded_len(660_000), 726_000);
    }

    fn fit() -> Viewport {
        Viewport::fit(demo::friday().length_ms())
    }

    #[test]
    fn clips_sit_where_their_time_is() {
        let show = demo::friday();
        let main = &show.program()[2];
        let (left, width) = span_pct(fit(), main.start_ms, main.end_ms()).unwrap_or_default();
        assert!((left - 240.0 / 660.0 * 100.0).abs() < 1e-9);
        assert!((width - 180.0 / 660.0 * 100.0).abs() < 1e-9);
        // Zoomed into the last two minutes, the first clip is out of view.
        let end = fit().with_left(540.0 / 660.0);
        assert_eq!(span_pct(end, 0, 180_000), None);
        assert_eq!(at_pct(end, 600_000), Some(50.0));
        assert_eq!(at_pct(end, 1000), None);
    }

    #[test]
    fn rows_group_pictures_control_and_audio() {
        let show = demo::friday();
        let kinds: Vec<TrackKind> = show.tracks.iter().map(|t| t.kind).collect();
        let lines: Vec<bool> = kinds
            .iter()
            .enumerate()
            .map(|(n, k)| starts_group(n.checked_sub(1).map(|p| kinds[p]), *k))
            .collect();
        // V3 V2 V1 | K S1 S2 | A1 A2
        assert_eq!(
            lines,
            [false, false, false, true, false, false, true, false]
        );
        assert_eq!(row_px(TrackKind::Program), 80);
    }

    #[test]
    fn a_bookmark_inside_a_timed_clip_seeks() {
        let show = demo::friday();
        let seeking: Vec<&str> = show
            .bookmarks
            .iter()
            .filter(|b| seeks(&show, b.at_ms))
            .map(|b| b.name.as_str())
            .collect();
        // Intro at 3:00 is intro.mp4's start but lands inside the first bed.mp3, and Outro
        // at 10:00 inside the second; Waiting at 0:00 is the first bed's start.
        assert_eq!(seeking, ["Intro", "Outro"]);
    }

    #[test]
    fn next_names_what_starts_next() {
        let show = demo::friday();
        assert_eq!(
            next_up(&show, 330_000, true),
            ["keyframe Close", "Show Title"]
        );
        assert_eq!(next_up(&show, 330_000, false), ["keyframe Two-up"]);
        assert_eq!(next_up(&show, 0, true)[0], "scene Starting soon");
        assert!(next_up(&show, 660_000, false).is_empty());
    }

    #[test]
    fn the_head_flag_says_how_long_the_pause_has_been() {
        let run = RunView {
            phase: RunPhase::Paused {
                bookmark: "b3".to_owned(),
                since: 1000.0,
            },
            ..RunView::default()
        };
        assert_eq!(head_text(&run, 330_000, 135_000.0), "5:30 · paused 2:14");
        let looping = RunView {
            phase: RunPhase::Playing,
            looping: Some(("l1".to_owned(), 3)),
            ..RunView::default()
        };
        assert_eq!(head_text(&looping, 80_000, 0.0), "1:20 · pass 3");
    }

    #[test]
    fn flags_never_overlap() {
        let show = demo::friday();
        let width = 1400.0;
        let flags: Vec<(f64, f64)> = show
            .bookmarks
            .iter()
            .map(|b| {
                let x = at_pct(fit(), b.at_ms).unwrap_or_default() / 100.0 * width;
                (x, flag_px(b, seeks(&show, b.at_ms)))
            })
            .collect();
        let placed = place_flags(&flags, width);
        assert_eq!(placed.len(), show.bookmarks.len());
        for pair in placed.windows(2) {
            assert!(pair[0].left + pair[0].max_width <= pair[1].left);
        }
        // Squeezed into a narrow view, each keeps room for its pause symbol.
        let narrow = place_flags(&[(0.0, 80.0), (10.0, 80.0), (20.0, 80.0)], 100.0);
        assert!(narrow.iter().all(|p| p.max_width >= MIN_FLAG_PX));
    }

    #[test]
    fn the_last_flag_hangs_left_at_the_right_edge() {
        let placed = place_flags(&[(100.0, 60.0), (980.0, 90.0)], 1000.0);
        assert_eq!(
            placed[1],
            FlagPlace {
                left: 890.0,
                max_width: 90.0,
                flipped: true
            }
        );
        assert!(!placed[0].flipped);
    }

    #[test]
    fn the_overview_shows_program_in_its_colours() {
        let show = demo::friday();
        let segments = overview(&show, f64::from(show.length_ms()));
        assert_eq!(segments.len(), 5);
        assert_eq!(segments[0].left, 0.0);
        let last = segments[4];
        assert!((last.left + last.width - 1.0).abs() < 1e-9);
        assert_eq!(
            segments.iter().map(|s| s.colour).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn the_zoom_bar_grips_its_ends_and_middle() {
        // The middle half in view, over a 1000 px bar: the thumb is 250..750 px.
        let view = fit().with_left(0.25).with_right(0.75);
        assert_eq!(grip_at(view, 100.0, 1000.0), Grip::Outside);
        assert_eq!(grip_at(view, 248.0, 1000.0), Grip::Left);
        assert_eq!(grip_at(view, 265.0, 1000.0), Grip::Left);
        assert_eq!(grip_at(view, 500.0, 1000.0), Grip::Middle);
        assert_eq!(grip_at(view, 745.0, 1000.0), Grip::Right);
        assert_eq!(grip_at(view, 900.0, 1000.0), Grip::Outside);
        // A narrow thumb is split between its two handles.
        let narrow = fit().with_left(0.5).with_right(0.51);
        assert_eq!(grip_at(narrow, 502.0, 1000.0), Grip::Left);
        assert_eq!(grip_at(narrow, 508.0, 1000.0), Grip::Right);
    }

    #[test]
    fn dragging_the_zoom_bar_zooms_and_scrolls() {
        let from = fit();
        // The left handle dragged a quarter of the way in: the view starts there.
        let drag = ZoomDrag {
            grip: Grip::Left,
            from_x: 0.0,
            from,
        };
        let v = drag.at(250.0, 1000.0);
        assert!((v.start_ms - 165_000.0).abs() < 1e-6);
        assert_eq!(v.end_ms(), 660_000.0);
        // The middle dragged scrolls without zooming.
        let half = from.with_right(0.5);
        let drag = ZoomDrag {
            grip: Grip::Middle,
            from_x: 100.0,
            from: half,
        };
        let v = drag.at(200.0, 1000.0);
        assert!((v.start_ms - 66_000.0).abs() < 1e-6);
        assert_eq!(v.span_ms, half.span_ms);
        // A press past the thumb centres the view there.
        let v = centred_at(half, 750.0, 1000.0);
        assert!((v.start_ms + v.span_ms / 2.0 - 495_000.0).abs() < 1e-6);
    }

    #[test]
    fn the_wheel_zooms_with_ctrl_and_scrolls_with_shift() {
        let view = fit();
        let zoomed = wheeled(view, (0.0, -200.0), true, false, 500.0, 1000.0);
        assert!(zoomed.is_some_and(|v| v.span_ms < view.span_ms));
        let zoomed = zoomed.unwrap_or(view);
        let scrolled = wheeled(zoomed, (0.0, 100.0), false, true, 0.0, 1000.0);
        assert!(scrolled.is_some_and(|v| v.start_ms > zoomed.start_ms));
        assert_eq!(wheeled(view, (0.0, 100.0), false, false, 0.0, 1000.0), None);
    }

    #[test]
    fn curves_and_fades_draw_in_their_boxes() {
        assert_eq!(
            curve_path(&[(0.0, 0.0), (1.0, 1.0)]),
            "M0.00 100.00 L100.00 0.00"
        );
        assert_eq!(
            fade_path(10_000, (1000, 2000), true),
            "M0 100 L100.0 4 L800.0 4 L1000 100 Z"
        );
        // Fades longer than the clip meet instead of crossing.
        assert_eq!(
            fade_path(1000, (800, 800), false),
            "M0 100 L800.0 4 L800.0 4 L1000 100"
        );
    }
}
