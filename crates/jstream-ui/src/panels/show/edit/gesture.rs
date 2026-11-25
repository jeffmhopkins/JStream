//! What a drag in the Edit view does to the show: the press took something (a [`Grab`]),
//! and with the pointer now at a time, [`dragged`] gives the show as it would be if the
//! drag ended there, with the tip that follows the pointer. The view draws that show
//! while the drag lasts and sends it as one `show.set` when the pointer comes up, so a
//! whole drag is one undo step. Pure functions of the show, tested natively.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::super::edits::{self, SNAP_MS};
use super::super::time::clock;
use super::geom::Part;
use jstream_control::protocol::ShowInfo;

/// What a press took, by its ID within the show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grab {
    /// A program clip's middle: a drag reorders it.
    Program(String),
    /// A program clip's right end: a drag trims it, and what follows ripples.
    ProgramEnd(String),
    /// An overlay or audio clip, by the part taken: its middle moves it, an end trims
    /// it, a fade handle sets that fade.
    Span(String, Part),
    /// A bookmark's flag or line.
    Bookmark(String),
    /// A keyframe marker or a cue.
    Marker(String),
    /// A loop's bar, by the part taken: its middle moves it, an end moves that end.
    Loop(String, Part),
    /// The Loop tool on the ruler: a new loop from where the press was.
    NewLoop,
}

/// Where dragged times snap: the show's edges, bookmarks, loop edges and the play head,
/// within `reach` ms; else to [`SNAP_MS`]. `free` (Shift) turns snapping off.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapper {
    /// The times to snap to, in order.
    pub points: Vec<u32>,
    /// How far a time is pulled onto one, in ms.
    pub reach: u32,
    /// Whether Shift is held.
    pub free: bool,
}

impl Snapper {
    /// `ms` snapped.
    pub fn at(&self, ms: f64) -> u32 {
        edits::snap(ms, &self.points, self.reach, self.free)
    }

    /// The point within reach of `ms` nearest it, if any.
    fn near(&self, ms: u32) -> Option<u32> {
        self.points
            .iter()
            .copied()
            .filter(|p| p.abs_diff(ms) <= self.reach)
            .min_by_key(|p| p.abs_diff(ms))
    }

    /// The start of a span `len` long moved to start about `start`: its start or its end
    /// pulled onto the nearer point within reach, else its start to [`SNAP_MS`].
    pub fn span(&self, start: f64, len: u32) -> u32 {
        let s = start.max(0.0).round() as u32;
        if self.free {
            return s;
        }
        match (self.near(s), self.near(s + len)) {
            (Some(a), Some(b)) if a.abs_diff(s) <= b.abs_diff(s + len) => a,
            (_, Some(b)) => b.saturating_sub(len),
            (Some(a), None) => a,
            (None, None) => ((s + SNAP_MS / 2) / SNAP_MS) * SNAP_MS,
        }
    }

    /// The same, without the points from `from` on: what a ripple would move anyway.
    fn before(&self, from: u32) -> Snapper {
        Snapper {
            points: self.points.iter().copied().filter(|p| *p < from).collect(),
            ..self.clone()
        }
    }
}

/// A drag's outcome so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Dragged {
    /// The show as it would be if the drag ended here.
    pub show: ShowInfo,
    /// The tip beside the pointer: "↔ drag to trim · 3:00".
    pub tip: String,
    /// Why the show can't take it, when it can't: the show is left as it was.
    pub refused: Option<String>,
}

/// Seconds as a fade reads: "0.5 s", "1 s".
fn secs(ms: u32) -> String {
    let text = format!("{:.1}", f64::from(ms) / 1000.0);
    format!("{} s", text.trim_end_matches(".0"))
}

/// Where an overlay or audio clip is: its start, length and fades.
fn span_of(show: &ShowInfo, id: &str) -> Option<(u32, u32, (u32, u32))> {
    let overlays = show
        .overlays
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.clip_id == id)
        .map(|c| (c.start_ms, c.length_ms, (c.fade_in_ms, c.fade_out_ms)));
    overlays.or_else(|| {
        show.audio
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.clip_id == id)
            .map(|c| (c.start_ms, c.length_ms, (c.fade_in_ms, c.fade_out_ms)))
    })
}

/// A program clip's start and length.
fn program_of(show: &ShowInfo, id: &str) -> Option<(u32, u32)> {
    show.program
        .iter()
        .zip(show.program_starts())
        .find(|(c, _)| c.clip_id == id)
        .map(|(c, start)| (start, c.length_ms))
}

/// Where a keyframe marker or cue is.
fn marker_at(show: &ShowInfo, id: &str) -> Option<u32> {
    show.keyframes
        .iter()
        .find(|m| m.marker_id == id)
        .map(|m| m.at_ms)
        .or_else(|| {
            show.sequences
                .iter()
                .flat_map(|t| &t.cues)
                .find(|c| c.cue_id == id)
                .map(|c| c.at_ms)
        })
}

/// `ms` moved by `by`, never below 0.
fn moved(ms: u32, by: f64) -> f64 {
    (f64::from(ms) + by).max(0.0)
}

/// The show with what `grab` took dragged from `from_ms` to `at_ms` (both in show time,
/// unsnapped), snapped by `snap`.
pub fn dragged(base: &ShowInfo, grab: &Grab, from_ms: f64, at_ms: f64, snap: &Snapper) -> Dragged {
    let by = at_ms - from_ms;
    let mut show = base.clone();
    let mut refused = None;
    let tip = match grab {
        Grab::Program(id) => {
            let to = edits::program_drop_index(base, id, at_ms.max(0.0) as u32);
            edits::move_program_clip(&mut show, id, to);
            let start = program_of(&show, id).map_or(0, |p| p.0);
            format!("↔ move · {}", clock(start))
        }
        Grab::ProgramEnd(id) => {
            let Some((start, len)) = program_of(base, id) else {
                return unchanged(base);
            };
            let end = snap.before(start + len).at(moved(start + len, by));
            edits::trim_program_clip(&mut show, id, end);
            let len = program_of(&show, id).map_or(len, |p| p.1);
            format!("↔ drag to trim · {}", clock(len))
        }
        Grab::Span(id, part) => {
            let Some((start, len, fades)) = span_of(base, id) else {
                return unchanged(base);
            };
            let end = start + len;
            let (s, l, text) = match part {
                Part::Body => {
                    let s = snap.span(moved(start, by), len);
                    (s, len, clock(s))
                }
                Part::Start => {
                    let s = snap
                        .at(moved(start, by))
                        .min(end.saturating_sub(edits::MIN_CLIP_MS));
                    (s, end - s, format!("↔ drag to trim · {}", clock(end - s)))
                }
                Part::End => {
                    let e = snap.at(moved(end, by)).max(start + edits::MIN_CLIP_MS);
                    (
                        start,
                        e - start,
                        format!("↔ drag to trim · {}", clock(e - start)),
                    )
                }
                Part::FadeIn | Part::FadeOut => {
                    let snapped = |ms: f64| ((ms.max(0.0) / 100.0).round() as u32) * 100;
                    let next = if *part == Part::FadeIn {
                        (snapped(f64::from(fades.0) + by).min(len), fades.1)
                    } else {
                        (fades.0, snapped(f64::from(fades.1) - by).min(len))
                    };
                    edits::set_fades(&mut show, id, next);
                    let now = span_of(&show, id).map_or(fades, |s| s.2);
                    let tip = if *part == Part::FadeIn {
                        format!("fade in {}", secs(now.0))
                    } else {
                        format!("fade out {}", secs(now.1))
                    };
                    return Dragged {
                        show,
                        tip,
                        refused: None,
                    };
                }
            };
            if !edits::set_span(&mut show, id, s, l) && (s, l) != (start, len) {
                refused = Some("It would overlap another clip on its track.".to_owned());
            }
            text
        }
        Grab::Bookmark(id) => {
            let Some(at) = base
                .bookmarks
                .iter()
                .find(|b| b.bookmark_id == *id)
                .map(|b| b.at_ms)
            else {
                return unchanged(base);
            };
            let to = snap.at(moved(at, by));
            if !edits::move_bookmark(&mut show, id, to) && to != at {
                refused = Some("Another bookmark is there.".to_owned());
            }
            clock(to)
        }
        Grab::Marker(id) => {
            let Some(at) = marker_at(base, id) else {
                return unchanged(base);
            };
            let to = snap.at(moved(at, by));
            edits::move_marker(&mut show, id, to);
            clock(to)
        }
        Grab::Loop(id, part) => {
            let Some((start, end)) = base
                .loops
                .iter()
                .find(|l| l.loop_id == *id)
                .map(|l| (l.start_ms, l.end_ms))
            else {
                return unchanged(base);
            };
            let (s, e) = match part {
                Part::Start => (snap.at(moved(start, by)).min(end), end),
                Part::End => (start, snap.at(moved(end, by)).max(start)),
                _ => {
                    let s = snap.span(moved(start, by), end - start);
                    (s, s + (end - start))
                }
            };
            if !edits::set_loop(&mut show, id, s, e) && (s, e) != (start, end) {
                refused = Some(loop_refusal(s, e));
            }
            format!("⟲ {} – {}", clock(s), clock(e))
        }
        Grab::NewLoop => {
            let (a, b) = (snap.at(from_ms), snap.at(at_ms));
            let (s, e) = (a.min(b), a.max(b));
            if edits::add_loop(&mut show, s, e).is_none() {
                refused = Some(loop_refusal(s, e));
            }
            format!("⟲ Loop · {}", clock(e - s))
        }
    };
    if refused.is_some() {
        show = base.clone();
    }
    Dragged { show, tip, refused }
}

/// Why a loop from `start` to `end` was refused.
fn loop_refusal(start: u32, end: u32) -> String {
    if end.saturating_sub(start) < jstream_control::protocol::MIN_SHOW_LOOP_MS {
        "A loop needs some length: drag further along the ruler.".to_owned()
    } else {
        "Loops can't overlap: this one would run into another.".to_owned()
    }
}

/// No change, and no tip.
fn unchanged(base: &ShowInfo) -> Dragged {
    Dragged {
        show: base.clone(),
        tip: String::new(),
        refused: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Starting soon 0..3:00, Main 3:00..6:00 (a pausing bookmark at its end), Outro
    /// 6:00..7:00, an overlay track with Logo at 0:10 for 0:50, an audio track with Mic at
    /// 0:00 for 1:00 with half-second fades, and a loop over 0:00..1:00.
    fn show() -> ShowInfo {
        let mut s = edits::new_show("friday", "Friday show");
        edits::insert_program_clip(&mut s, "soon".into(), "Starting soon", 0, 180_000, false);
        edits::insert_program_clip(&mut s, "main".into(), "Main", u32::MAX, 180_000, true);
        edits::insert_program_clip(&mut s, "outro".into(), "Outro", u32::MAX, 60_000, false);
        let ov = edits::add_overlay_track(&mut s).unwrap_or_default();
        edits::add_overlay_clip(&mut s, &ov, "logo".into(), 10_000, 50_000);
        let au = edits::add_audio_track(&mut s).unwrap_or_default();
        edits::add_audio_clip(&mut s, &au, "mic".into(), 0, 60_000, (500, 500));
        edits::add_loop(&mut s, 0, 60_000);
        s
    }

    fn snapper(s: &ShowInfo) -> Snapper {
        Snapper {
            points: edits::snap_points(s),
            reach: 1000,
            free: false,
        }
    }

    #[test]
    fn dragging_a_program_clip_reorders_it() {
        let s = show();
        let d = dragged(
            &s,
            &Grab::Program("clip3".into()),
            390_000.0,
            20_000.0,
            &snapper(&s),
        );
        assert_eq!(d.show.program[0].scene_id.as_str(), "outro");
        assert_eq!(d.tip, "↔ move · 0:00");
        assert_eq!(d.refused, None);
    }

    #[test]
    fn dragging_a_program_end_trims_with_ripple_and_says_the_length() {
        let s = show();
        let d = dragged(
            &s,
            &Grab::ProgramEnd("clip2".into()),
            360_000.0,
            300_400.0,
            &snapper(&s),
        );
        let starts: Vec<u32> = d.show.program_starts().collect();
        assert_eq!(starts, [0, 180_000, 300_400]);
        assert_eq!(d.tip, "↔ drag to trim · 2:00");
        assert_eq!(
            d.show.bookmarks[0].at_ms, 300_400,
            "the end's bookmark moves with it"
        );
        // Shift: no snapping.
        let free = Snapper {
            free: true,
            ..snapper(&s)
        };
        let d = dragged(
            &s,
            &Grab::ProgramEnd("clip2".into()),
            360_000.0,
            300_040.0,
            &free,
        );
        assert_eq!(d.show.program[1].length_ms, 120_040);
    }

    #[test]
    fn overlay_and_audio_clips_move_trim_and_fade() {
        let s = show();
        let snap = snapper(&s);
        let logo = s.overlays[0].clips[0].clip_id.clone();
        // Moved so its end lands near Starting soon's end: it snaps there.
        let d = dragged(
            &s,
            &Grab::Span(logo.clone(), Part::Body),
            30_000.0,
            149_500.0,
            &snap,
        );
        assert_eq!(d.show.overlays[0].clips[0].start_ms, 130_000);
        let d = dragged(
            &s,
            &Grab::Span(logo.clone(), Part::End),
            60_000.0,
            90_040.0,
            &snap,
        );
        assert_eq!(d.show.overlays[0].clips[0].length_ms, 80_000);
        assert_eq!(d.tip, "↔ drag to trim · 1:20");
        let d = dragged(
            &s,
            &Grab::Span(logo, Part::Start),
            10_000.0,
            70_000.0,
            &snap,
        );
        assert_eq!(d.show.overlays[0].clips[0].length_ms, edits::MIN_CLIP_MS);
        let mic = s.audio[0].clips[0].clip_id.clone();
        let d = dragged(
            &s,
            &Grab::Span(mic.clone(), Part::FadeIn),
            500.0,
            2_520.0,
            &snap,
        );
        assert_eq!(d.show.audio[0].clips[0].fade_in_ms, 2_500);
        assert_eq!(d.tip, "fade in 2.5 s");
        let d = dragged(
            &s,
            &Grab::Span(mic, Part::FadeOut),
            59_500.0,
            59_000.0,
            &snap,
        );
        assert_eq!(d.show.audio[0].clips[0].fade_out_ms, 1_000);
    }

    #[test]
    fn an_overlapping_move_is_refused_and_leaves_the_show() {
        let mut s = show();
        let ov = s.overlays[0].track_id.clone();
        edits::add_overlay_clip(&mut s, &ov, "title".into(), 100_000, 20_000);
        let logo = s.overlays[0].clips[0].clip_id.clone();
        let free = Snapper {
            free: true,
            ..snapper(&s)
        };
        let d = dragged(&s, &Grab::Span(logo, Part::Body), 10_000.0, 95_000.0, &free);
        assert!(d.refused.is_some());
        assert_eq!(d.show, s);
    }

    #[test]
    fn bookmarks_markers_and_loops_drag() {
        let mut s = show();
        let k = edits::add_keyframe(&mut s, 200_000, None);
        let snap = snapper(&s);
        let d = dragged(
            &s,
            &Grab::Bookmark("bm1".into()),
            360_000.0,
            330_020.0,
            &snap,
        );
        assert_eq!(d.show.bookmarks[0].at_ms, 330_000);
        assert_eq!(d.tip, "5:30");
        let d = dragged(&s, &Grab::Marker(k), 200_000.0, 179_700.0, &snap);
        assert_eq!(d.show.keyframes[0].at_ms, 180_000);
        let d = dragged(
            &s,
            &Grab::Loop("loop1".into(), Part::End),
            60_000.0,
            179_600.0,
            &snap,
        );
        assert_eq!(d.show.loops[0].end_ms, 180_000);
        let d = dragged(
            &s,
            &Grab::Loop("loop1".into(), Part::Body),
            30_000.0,
            150_000.0,
            &snap,
        );
        assert_eq!(
            (d.show.loops[0].start_ms, d.show.loops[0].end_ms),
            (120_000, 180_000)
        );
    }

    #[test]
    fn a_new_loop_is_drawn_along_the_ruler_unless_it_overlaps() {
        let s = show();
        let snap = snapper(&s);
        let d = dragged(&s, &Grab::NewLoop, 240_200.0, 180_300.0, &snap);
        assert_eq!(d.show.loops.len(), 2);
        assert_eq!(
            (d.show.loops[1].start_ms, d.show.loops[1].end_ms),
            (180_000, 240_200)
        );
        assert_eq!(d.tip, "⟲ Loop · 1:00");
        let d = dragged(&s, &Grab::NewLoop, 30_000.0, 120_000.0, &snap);
        assert_eq!(d.show.loops.len(), 1);
        assert!(d.refused.is_some_and(|r| r.contains("overlap")));
    }
}
