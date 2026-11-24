//! The Inspector's Show page edits: each field's change to a [`ShowInfo`], which the
//! page sends as one `show.set`, so one undo step ([`super::ShowPanel::set_show`]). The
//! timeline tools' own edits are in [`super::edits`]; these are the ones only a field
//! makes. Every one returns whether it changed the show, and is tested natively.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::edits::{self, MIN_CLIP_MS, SNAP_MS};
use jstream_control::protocol::{
    Finite32, LoopLeave, MAX_SHOW_MS, MAX_SHOW_NOTE_LEN, MAX_SHOW_WAIT_MS, SceneId, ShowBookmark,
    ShowEnd, ShowInfo, SourceId, TransitionPresetId,
};

/// The quietest and loudest level an audio clip takes, in dB (the engine's gain range).
pub const GAIN_RANGE_DB: (f32, f32) = (-100.0, 24.0);

/// What happens at a program clip's end: the bookmark that sits there, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtEnd {
    /// A pausing bookmark: the show waits for Continue.
    Pausing,
    /// A bookmark that doesn't pause: a place to jump to, and the show plays on.
    PlaysThrough,
    /// No bookmark.
    Nothing,
}

impl AtEnd {
    /// Every choice, in the picker's order.
    pub const ALL: [AtEnd; 3] = [AtEnd::Pausing, AtEnd::PlaysThrough, AtEnd::Nothing];

    /// The choice as the picker reads it.
    pub fn label(self) -> &'static str {
        match self {
            AtEnd::Pausing => "Pausing bookmark",
            AtEnd::PlaysThrough => "Plays through",
            AtEnd::Nothing => "Nothing",
        }
    }

    /// The picker's value for it.
    pub fn key(self) -> &'static str {
        match self {
            AtEnd::Pausing => "pausing",
            AtEnd::PlaysThrough => "through",
            AtEnd::Nothing => "nothing",
        }
    }

    /// The choice a picker's value names.
    pub fn from_key(key: &str) -> Option<AtEnd> {
        AtEnd::ALL.into_iter().find(|a| a.key() == key)
    }
}

/// Program clip `clip`'s start and end in show time.
pub fn program_span(show: &ShowInfo, clip: &str) -> Option<(u32, u32)> {
    show.program
        .iter()
        .zip(show.program_starts())
        .find(|(c, _)| c.clip_id == clip)
        .map(|(c, start)| (start, start.saturating_add(c.length_ms)))
}

/// The bookmark at program clip `clip`'s end, if one sits exactly there.
pub fn end_bookmark<'a>(show: &'a ShowInfo, clip: &str) -> Option<&'a ShowBookmark> {
    let (_, end) = program_span(show, clip)?;
    show.bookmarks.iter().find(|b| b.at_ms == end)
}

/// What happens at program clip `clip`'s end.
pub fn at_end(show: &ShowInfo, clip: &str) -> AtEnd {
    match end_bookmark(show, clip) {
        Some(b) if b.pausing => AtEnd::Pausing,
        Some(_) => AtEnd::PlaysThrough,
        None => AtEnd::Nothing,
    }
}

/// Sets what happens at program clip `clip`'s end: flips the bookmark there, adds one
/// named after `name` (the clip's scene), or removes it.
pub fn set_at_end(show: &mut ShowInfo, clip: &str, to: AtEnd, name: &str) -> bool {
    let Some((_, end)) = program_span(show, clip) else {
        return false;
    };
    let at = show.bookmarks.iter().position(|b| b.at_ms == end);
    match (to, at) {
        (AtEnd::Nothing, None) => false,
        (AtEnd::Nothing, Some(n)) => {
            show.bookmarks.remove(n);
            if matches!(&show.end, ShowEnd::GoTo { bookmark_id } if show.bookmark(bookmark_id).is_none())
            {
                show.end = ShowEnd::Stop;
            }
            true
        }
        (to, Some(n)) => {
            let pausing = to == AtEnd::Pausing;
            let b = &mut show.bookmarks[n];
            if b.pausing == pausing {
                return false;
            }
            b.pausing = pausing;
            if !pausing {
                b.continue_after_ms = None;
            }
            true
        }
        (to, None) => {
            let Some(id) = edits::add_bookmark(show, end) else {
                return false;
            };
            if let Some(b) = show.bookmarks.iter_mut().find(|b| b.bookmark_id == id) {
                if !name.trim().is_empty() {
                    b.name = name.trim().to_owned();
                }
                b.pausing = to == AtEnd::Pausing;
            }
            true
        }
    }
}

/// Puts scene `scene` on program clip or overlay clip `clip`.
pub fn set_scene(show: &mut ShowInfo, clip: &str, scene: SceneId) -> bool {
    let slot = show
        .program
        .iter_mut()
        .find(|c| c.clip_id == clip)
        .map(|c| &mut c.scene_id)
        .or_else(|| {
            show.overlays
                .iter_mut()
                .flat_map(|t| &mut t.clips)
                .find(|c| c.clip_id == clip)
                .map(|c| &mut c.scene_id)
        });
    match slot {
        Some(slot) if *slot != scene => {
            *slot = scene;
            true
        }
        _ => false,
    }
}

/// Sets program clip `clip`'s length: the clips after it ripple, as a trim does.
pub fn set_program_length(show: &mut ShowInfo, clip: &str, length_ms: u32) -> bool {
    let Some((start, _)) = program_span(show, clip) else {
        return false;
    };
    let length_ms = length_ms.clamp(MIN_CLIP_MS, MAX_SHOW_MS);
    edits::trim_program_clip(show, clip, start.saturating_add(length_ms))
}

/// The start and length of overlay or audio clip `clip`.
pub fn span_of(show: &ShowInfo, clip: &str) -> Option<(u32, u32)> {
    show.overlays
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.clip_id == clip)
        .map(|c| (c.start_ms, c.length_ms))
        .or_else(|| {
            show.audio
                .iter()
                .flat_map(|t| &t.clips)
                .find(|c| c.clip_id == clip)
                .map(|c| (c.start_ms, c.length_ms))
        })
}

/// Moves overlay or audio clip `clip` to start at `start_ms`, keeping its length.
pub fn set_clip_start(show: &mut ShowInfo, clip: &str, start_ms: u32) -> bool {
    let Some((_, length)) = span_of(show, clip) else {
        return false;
    };
    edits::set_span(show, clip, start_ms, length)
}

/// Sets overlay or audio clip `clip`'s length, keeping its start.
pub fn set_clip_length(show: &mut ShowInfo, clip: &str, length_ms: u32) -> bool {
    let Some((start, _)) = span_of(show, clip) else {
        return false;
    };
    edits::set_span(show, clip, start, length_ms)
}

/// Puts source `source` on audio clip `clip`.
pub fn set_source(show: &mut ShowInfo, clip: &str, source: SourceId) -> bool {
    match show
        .audio
        .iter_mut()
        .flat_map(|t| &mut t.clips)
        .find(|c| c.clip_id == clip)
    {
        Some(c) if c.source_id != source => {
            c.source_id = source;
            true
        }
        _ => false,
    }
}

/// Sets audio clip `clip`'s level, kept within [`GAIN_RANGE_DB`].
pub fn set_gain(show: &mut ShowInfo, clip: &str, db: f32) -> bool {
    let Some(db) = Finite32::new(db.clamp(GAIN_RANGE_DB.0, GAIN_RANGE_DB.1)) else {
        return false;
    };
    match show
        .audio
        .iter_mut()
        .flat_map(|t| &mut t.clips)
        .find(|c| c.clip_id == clip)
    {
        Some(c) if c.gain_db != db => {
            c.gain_db = db;
            true
        }
        _ => false,
    }
}

/// Changes bookmark `id` with `f`, and says whether that changed it.
fn with_bookmark(show: &mut ShowInfo, id: &str, f: impl FnOnce(&mut ShowBookmark)) -> bool {
    let Some(b) = show.bookmarks.iter_mut().find(|b| b.bookmark_id == id) else {
        return false;
    };
    let before = b.clone();
    f(b);
    *b != before
}

/// Makes bookmark `id` pausing or not. One that doesn't pause keeps no wait.
pub fn set_pausing(show: &mut ShowInfo, id: &str, pausing: bool) -> bool {
    with_bookmark(show, id, |b| {
        b.pausing = pausing;
        if !pausing {
            b.continue_after_ms = None;
        }
    })
}

/// Sets the transition a jump to bookmark `id` arrives with; `None` is the show's.
pub fn set_arrive(show: &mut ShowInfo, id: &str, preset: Option<TransitionPresetId>) -> bool {
    with_bookmark(show, id, |b| b.preset_id = preset)
}

/// Sets how long pausing bookmark `id` waits before it continues by itself; `None`
/// waits for Continue. A bookmark that doesn't pause takes none.
pub fn set_continue_after(show: &mut ShowInfo, id: &str, wait_ms: Option<u32>) -> bool {
    with_bookmark(show, id, |b| {
        if b.pausing {
            b.continue_after_ms = wait_ms.map(|ms| ms.clamp(1, MAX_SHOW_WAIT_MS));
        }
    })
}

/// Sets bookmark `id`'s note for the operator, cut to the longest note.
pub fn set_note(show: &mut ShowInfo, id: &str, note: &str) -> bool {
    let mut note = note.trim().to_owned();
    while note.len() > MAX_SHOW_NOTE_LEN {
        note.pop();
    }
    with_bookmark(show, id, |b| b.note = note)
}

/// Moves bookmark `id` to `at_ms`, or as near after it as is free: two bookmarks never
/// share a time.
pub fn set_bookmark_at(show: &mut ShowInfo, id: &str, at_ms: u32) -> bool {
    let mut at = at_ms.min(MAX_SHOW_MS);
    while show
        .bookmarks
        .iter()
        .any(|b| b.at_ms == at && b.bookmark_id != id)
    {
        at = at.saturating_add(SNAP_MS);
    }
    edits::move_bookmark(show, id, at)
}

/// Changes loop `id` with `f`, and says whether that changed it.
fn with_loop(
    show: &mut ShowInfo,
    id: &str,
    f: impl FnOnce(&mut jstream_control::protocol::ShowLoop),
) -> bool {
    let Some(l) = show.loops.iter_mut().find(|l| l.loop_id == id) else {
        return false;
    };
    let before = l.clone();
    f(l);
    *l != before
}

/// Sets loop `id`'s start, keeping its end, unless that leaves it too short or
/// overlapping another.
pub fn set_loop_start(show: &mut ShowInfo, id: &str, start_ms: u32) -> bool {
    let Some(end) = show
        .loops
        .iter()
        .find(|l| l.loop_id == id)
        .map(|l| l.end_ms)
    else {
        return false;
    };
    start_ms < end && edits::set_loop(show, id, start_ms, end)
}

/// Sets loop `id`'s end, keeping its start, unless that leaves it too short or
/// overlapping another.
pub fn set_loop_end(show: &mut ShowInfo, id: &str, end_ms: u32) -> bool {
    let Some(start) = show
        .loops
        .iter()
        .find(|l| l.loop_id == id)
        .map(|l| l.start_ms)
    else {
        return false;
    };
    end_ms > start && edits::set_loop(show, id, start, end_ms)
}

/// Sets the transition loop `id` wraps with, from its end back to its start; `None` is
/// a cut back without one (the show's default).
pub fn set_loop_preset(show: &mut ShowInfo, id: &str, preset: Option<TransitionPresetId>) -> bool {
    with_loop(show, id, |l| l.preset_id = preset)
}

/// Sets how loop `id` is left on Continue.
pub fn set_loop_leave(show: &mut ShowInfo, id: &str, leave: LoopLeave) -> bool {
    with_loop(show, id, |l| l.leave = leave)
}

/// Sets which keyframe marker `id` loads (`None`: the scene's own values) and the
/// transition it moves with (`None`: the default).
pub fn set_marker(
    show: &mut ShowInfo,
    id: &str,
    keyframe: Option<SceneId>,
    preset: Option<TransitionPresetId>,
) -> bool {
    match show.keyframes.iter_mut().find(|m| m.marker_id == id) {
        Some(m)
            if (m.keyframe_id.as_ref(), m.preset_id.as_ref())
                != (keyframe.as_ref(), preset.as_ref()) =>
        {
            m.keyframe_id = keyframe;
            m.preset_id = preset;
            true
        }
        _ => false,
    }
}

/// Moves keyframe marker `id` to `at_ms`.
pub fn set_marker_at(show: &mut ShowInfo, id: &str, at_ms: u32) -> bool {
    let at_ms = at_ms.min(MAX_SHOW_MS);
    match show.keyframes.iter_mut().find(|m| m.marker_id == id) {
        Some(m) if m.at_ms != at_ms => {
            m.at_ms = at_ms;
            show.keyframes.sort_by_key(|m| m.at_ms);
            true
        }
        _ => false,
    }
}

/// Renames the show.
pub fn set_show_name(show: &mut ShowInfo, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || show.name == name {
        return false;
    }
    show.name = name.to_owned();
    true
}

/// Sets what the show does at its end.
pub fn set_show_end(show: &mut ShowInfo, end: ShowEnd) -> bool {
    if let ShowEnd::GoTo { bookmark_id } = &end
        && show.bookmark(bookmark_id).is_none()
    {
        return false;
    }
    if show.end == end {
        return false;
    }
    show.end = end;
    true
}

/// Sets the show's default transition; `None` is the collection's default.
pub fn set_show_preset(show: &mut ShowInfo, preset: Option<TransitionPresetId>) -> bool {
    if show.preset_id == preset {
        return false;
    }
    show.preset_id = preset;
    true
}

/// Sets how long an image dropped on program lasts.
pub fn set_image_ms(show: &mut ShowInfo, ms: u32) -> bool {
    let ms = ms.clamp(MIN_CLIP_MS, MAX_SHOW_MS);
    if show.image_ms == ms {
        return false;
    }
    show.image_ms = ms;
    true
}

/// Sets the show's planned length, for the clocks; `None` plans none.
pub fn set_planned_ms(show: &mut ShowInfo, ms: Option<u32>) -> bool {
    let ms = ms.filter(|&ms| ms > 0).map(|ms| ms.min(MAX_SHOW_MS));
    if show.planned_ms == ms {
        return false;
    }
    show.planned_ms = ms;
    true
}

#[cfg(test)]
mod tests {
    use super::super::edits::{
        add_audio_clip, add_audio_track, add_loop, insert_program_clip, new_show,
    };
    use super::*;

    fn show() -> ShowInfo {
        let mut s = new_show("friday", "Friday show");
        insert_program_clip(&mut s, "soon".into(), "Starting soon", 0, 180_000, false);
        insert_program_clip(&mut s, "main".into(), "Main", u32::MAX, 180_000, true);
        s
    }

    fn clip(s: &ShowInfo, n: usize) -> String {
        s.program[n].clip_id.clone()
    }

    #[test]
    fn a_clips_end_reads_and_flips_its_bookmark() {
        let mut s = show();
        let (soon, main) = (clip(&s, 0), clip(&s, 1));
        assert_eq!(at_end(&s, &main), AtEnd::Pausing);
        assert_eq!(at_end(&s, &soon), AtEnd::Nothing);
        assert!(set_at_end(&mut s, &main, AtEnd::PlaysThrough, "Main"));
        assert_eq!(at_end(&s, &main), AtEnd::PlaysThrough);
        assert!(!set_at_end(&mut s, &main, AtEnd::PlaysThrough, "Main"));
        assert!(set_at_end(&mut s, &soon, AtEnd::Pausing, "Starting soon"));
        let b = end_bookmark(&s, &soon).expect("added");
        assert_eq!(
            (b.at_ms, b.pausing, b.name.as_str()),
            (180_000, true, "Starting soon")
        );
        assert!(set_at_end(&mut s, &main, AtEnd::Nothing, ""));
        assert_eq!(at_end(&s, &main), AtEnd::Nothing);
        assert_eq!(s.bookmarks.len(), 1);
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn removing_the_end_bookmark_the_show_goes_to_stops_it_instead() {
        let mut s = show();
        let main = clip(&s, 1);
        let id = end_bookmark(&s, &main).expect("there").bookmark_id.clone();
        assert!(set_show_end(&mut s, ShowEnd::GoTo { bookmark_id: id }));
        assert!(set_at_end(&mut s, &main, AtEnd::Nothing, ""));
        assert_eq!(s.end, ShowEnd::Stop);
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn a_new_length_ripples_what_follows() {
        let mut s = show();
        let soon = clip(&s, 0);
        assert!(set_program_length(&mut s, &soon, 120_000));
        assert_eq!(s.program_starts().collect::<Vec<_>>(), [0, 120_000]);
        assert_eq!(s.bookmarks[0].at_ms, 300_000);
        assert!(!set_program_length(&mut s, &soon, 120_000));
        assert!(set_program_length(&mut s, &soon, 0));
        assert_eq!(s.program[0].length_ms, MIN_CLIP_MS);
    }

    #[test]
    fn a_clip_takes_another_scene() {
        let mut s = show();
        let soon = clip(&s, 0);
        assert!(set_scene(&mut s, &soon, "outro".into()));
        assert_eq!(s.program[0].scene_id.as_str(), "outro");
        assert!(!set_scene(&mut s, &soon, "outro".into()));
        assert!(!set_scene(&mut s, "nope", "outro".into()));
    }

    #[test]
    fn an_audio_clip_moves_stretches_and_takes_a_level_and_source() {
        let mut s = show();
        let track = add_audio_track(&mut s).expect("track");
        let a = add_audio_clip(&mut s, &track, "bed".into(), 0, 60_000, (0, 4_000)).expect("clip");
        assert!(set_clip_start(&mut s, &a, 10_000));
        assert_eq!(span_of(&s, &a), Some((10_000, 60_000)));
        assert!(set_clip_length(&mut s, &a, 2_000));
        assert_eq!(span_of(&s, &a), Some((10_000, 2_000)));
        // The fade shrank to fit.
        assert_eq!(s.audio[0].clips[0].fade_out_ms, 2_000);
        assert!(set_gain(&mut s, &a, -6.0));
        assert!(!set_gain(&mut s, &a, -6.0));
        assert!(set_gain(&mut s, &a, 99.0));
        assert_eq!(s.audio[0].clips[0].gain_db.get(), 24.0);
        assert!(set_source(&mut s, &a, "mic".into()));
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn a_bookmarks_fields_each_change_it() {
        let mut s = show();
        let id = s.bookmarks[0].bookmark_id.clone();
        assert!(set_continue_after(&mut s, &id, Some(5_000)));
        assert_eq!(s.bookmarks[0].continue_after_ms, Some(5_000));
        assert!(set_pausing(&mut s, &id, false));
        assert_eq!(s.bookmarks[0].continue_after_ms, None);
        // A bookmark that doesn't pause waits for nothing.
        assert!(!set_continue_after(&mut s, &id, Some(5_000)));
        assert!(set_arrive(&mut s, &id, Some("fade".into())));
        assert!(!set_arrive(&mut s, &id, Some("fade".into())));
        assert!(set_note(&mut s, &id, "  cue the host  "));
        assert_eq!(s.bookmarks[0].note, "cue the host");
        assert!(
            !set_note(&mut s, &id, &"x".repeat(5_000))
                || s.bookmarks[0].note.len() == MAX_SHOW_NOTE_LEN
        );
    }

    #[test]
    fn a_bookmark_moved_onto_another_lands_just_after_it() {
        let mut s = show();
        let other = edits::add_bookmark(&mut s, 60_000).expect("added");
        let id = s
            .bookmarks
            .iter()
            .find(|b| b.bookmark_id != other)
            .map(|b| b.bookmark_id.clone())
            .expect("there");
        assert!(set_bookmark_at(&mut s, &id, 60_000));
        assert_eq!(s.bookmark(&id).map(|b| b.at_ms), Some(60_000 + SNAP_MS));
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn a_loops_ends_move_one_at_a_time() {
        let mut s = show();
        let id = add_loop(&mut s, 0, 60_000).expect("loop");
        assert!(set_loop_end(&mut s, &id, 90_000));
        assert!(set_loop_start(&mut s, &id, 30_000));
        assert_eq!((s.loops[0].start_ms, s.loops[0].end_ms), (30_000, 90_000));
        assert!(!set_loop_start(&mut s, &id, 95_000));
        assert!(set_loop_leave(&mut s, &id, LoopLeave::FinishPass));
        assert!(set_loop_preset(&mut s, &id, Some("fade".into())));
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn a_marker_takes_a_keyframe_a_preset_and_a_time() {
        let mut s = show();
        let id = edits::add_keyframe(&mut s, 200_000, None);
        assert!(set_marker(&mut s, &id, Some("close".into()), None));
        assert!(!set_marker(&mut s, &id, Some("close".into()), None));
        assert!(set_marker(
            &mut s,
            &id,
            Some("close".into()),
            Some("move".into())
        ));
        assert!(set_marker_at(&mut s, &id, 210_000));
        assert_eq!(s.keyframes[0].at_ms, 210_000);
    }

    #[test]
    fn the_shows_own_settings() {
        let mut s = show();
        assert!(set_show_name(&mut s, " Saturday "));
        assert_eq!(s.name, "Saturday");
        assert!(!set_show_name(&mut s, "  "));
        assert!(!set_show_end(
            &mut s,
            ShowEnd::GoTo {
                bookmark_id: "nope".into()
            }
        ));
        assert!(set_show_end(&mut s, ShowEnd::Loop));
        assert!(set_show_preset(&mut s, Some("fade".into())));
        assert!(set_image_ms(&mut s, 5_000));
        assert!(set_planned_ms(&mut s, Some(1_800_000)));
        assert!(set_planned_ms(&mut s, Some(0)));
        assert_eq!(s.planned_ms, None);
        assert_eq!(s.problem(), None);
    }
}
