//! The Show panel's edits: each tool's gesture as a change to a [`ShowInfo`], which the
//! panel sends as one `show.set`, so one undo step ([`super::ShowPanel::set_show`]).
//! All of them are plain functions, tested natively.
//!
//! The program track ripples by itself: its clips carry lengths, not starts, so trimming
//! or moving one moves every clip after it and program never has a gap. Bookmarks, loops,
//! keyframes and cues sit at times of their own and stay where they are.
//!
//! Docs: crates/jstream-ui/README.md#design

use jstream_control::protocol::{
    AudioClip, AudioTrack, KeyframeMarker, MAX_SHOW_TRACKS, MIN_SHOW_LOOP_MS, OverlayClip,
    OverlayTrack, ProgramClip, SceneId, ShowBookmark, ShowId, ShowInfo, ShowLoop, SourceId,
    TransitionPresetId,
};

/// Where a dragged edge or point snaps to without Shift, in ms.
pub const SNAP_MS: u32 = 100;

/// The shortest clip a trim leaves, in ms.
pub const MIN_CLIP_MS: u32 = 100;

/// How far, in ms, a dragged time is pulled onto a nearby edge (a clip's start or end,
/// a bookmark, a loop edge, the play head) when the view is at `ms_per_px`: about 6 px.
pub fn snap_reach(ms_per_px: f64) -> u32 {
    (ms_per_px * 6.0).round().max(0.0) as u32
}

/// Every time a drag snaps to in `show`: clip starts and ends on every track, bookmarks
/// and loop edges, in order with no repeats.
pub fn snap_points(show: &ShowInfo) -> Vec<u32> {
    let mut points: Vec<u32> = show.program_starts().collect();
    points.push(show.length_ms());
    for track in &show.overlays {
        for c in &track.clips {
            points.extend([c.start_ms, c.start_ms.saturating_add(c.length_ms)]);
        }
    }
    for track in &show.audio {
        for c in &track.clips {
            points.extend([c.start_ms, c.start_ms.saturating_add(c.length_ms)]);
        }
    }
    points.extend(show.bookmarks.iter().map(|b| b.at_ms));
    for l in &show.loops {
        points.extend([l.start_ms, l.end_ms]);
    }
    points.sort_unstable();
    points.dedup();
    points
}

/// `at_ms` pulled onto the nearest of `points` within `reach`, else rounded to
/// [`SNAP_MS`]; `free` (Shift held) leaves it as it is.
pub fn snap(at_ms: f64, points: &[u32], reach: u32, free: bool) -> u32 {
    let ms = at_ms.max(0.0).round() as u32;
    if free {
        return ms;
    }
    let near = points
        .iter()
        .copied()
        .min_by_key(|p| p.abs_diff(ms))
        .filter(|p| p.abs_diff(ms) <= reach);
    near.unwrap_or_else(|| ((ms + SNAP_MS / 2) / SNAP_MS) * SNAP_MS)
}

/// An ID for a new element of `show`, `<prefix>N`, unique across the show's elements.
pub fn fresh_id(show: &ShowInfo, prefix: &str) -> String {
    let mut taken: Vec<&str> = Vec::new();
    taken.extend(show.program.iter().map(|c| c.clip_id.as_str()));
    for t in &show.overlays {
        taken.push(&t.track_id);
        taken.extend(t.clips.iter().map(|c| c.clip_id.as_str()));
    }
    taken.extend(show.keyframes.iter().map(|m| m.marker_id.as_str()));
    for t in &show.sequences {
        taken.push(&t.track_id);
        taken.extend(t.cues.iter().map(|c| c.cue_id.as_str()));
    }
    for t in &show.audio {
        taken.push(&t.track_id);
        taken.extend(t.clips.iter().map(|c| c.clip_id.as_str()));
    }
    taken.extend(show.bookmarks.iter().map(|b| b.bookmark_id.as_str()));
    taken.extend(show.loops.iter().map(|l| l.loop_id.as_str()));
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|id| !taken.contains(&id.as_str()))
        .unwrap_or_else(|| prefix.to_owned())
}

/// A new, empty show with `id` and `name`.
pub fn new_show(id: &str, name: &str) -> ShowInfo {
    let mut show = ShowInfo::new(id.to_owned());
    show.name = name.to_owned();
    show
}

/// The bookmarks kept in time order with no two at one time: a later one at the same
/// time as an earlier one moves on by [`SNAP_MS`].
fn tidy_bookmarks(show: &mut ShowInfo) {
    show.bookmarks.sort_by_key(|b| b.at_ms);
    for n in 1..show.bookmarks.len() {
        let before = show.bookmarks[n - 1].at_ms;
        if show.bookmarks[n].at_ms <= before {
            show.bookmarks[n].at_ms = before + SNAP_MS;
        }
    }
}

/// Drops scene `scene` onto the program track at `at_ms`: the clip goes in before the
/// clip `at_ms` falls in the first half of, else after it, `length_ms` long. With
/// `pause_at_end` (a scene, not a video, decision 15), a pausing bookmark named after
/// the scene goes at its end. Clips after it, and the bookmarks, loops, keyframes and
/// cues at or after its start, move on by its length, so they stay with what they were
/// on. Returns the new clip's ID.
pub fn insert_program_clip(
    show: &mut ShowInfo,
    scene: SceneId,
    scene_name: &str,
    at_ms: u32,
    length_ms: u32,
    pause_at_end: bool,
) -> String {
    let id = fresh_id(show, "clip");
    let length_ms = length_ms.max(MIN_CLIP_MS);
    let starts: Vec<u32> = show.program_starts().collect();
    let index = show
        .program
        .iter()
        .zip(&starts)
        .position(|(c, start)| at_ms < start + c.length_ms / 2)
        .unwrap_or(show.program.len());
    let start = starts
        .get(index)
        .copied()
        .unwrap_or_else(|| show.length_ms());
    // A bookmark right at the cut is the end of the clip before (its pausing bookmark):
    // it stays between the two. A clip added after the last moves nothing at all.
    if index < show.program.len() {
        let at_cut: Vec<String> = show
            .bookmarks
            .iter()
            .filter(|b| b.at_ms == start)
            .map(|b| b.bookmark_id.clone())
            .collect();
        shift_from(show, start, i64::from(length_ms));
        for b in &mut show.bookmarks {
            if at_cut.contains(&b.bookmark_id) {
                b.at_ms = start;
            }
        }
        tidy_bookmarks(show);
    }
    show.program.insert(
        index,
        ProgramClip {
            clip_id: id.clone(),
            scene_id: scene,
            length_ms,
            preset_id: None,
        },
    );
    if pause_at_end {
        let bookmark_id = fresh_id(show, "bm");
        show.bookmarks.push(ShowBookmark {
            bookmark_id,
            name: scene_name.to_owned(),
            at_ms: start + length_ms,
            pausing: true,
            preset_id: None,
            continue_after_ms: None,
            note: String::new(),
        });
        tidy_bookmarks(show);
    }
    id
}

/// Moves everything at or after `from` by `by` ms (never below `from + by`, where a
/// shortened or removed clip now ends): bookmarks,
/// loops, keyframes, cues, overlay and audio clips. Program clips move by themselves.
fn shift_from(show: &mut ShowInfo, from: u32, by: i64) {
    let moved = |t: u32| {
        if t >= from {
            let floor = (i64::from(from) + by.min(0)).max(0);
            u32::try_from((i64::from(t) + by).max(floor)).unwrap_or(u32::MAX)
        } else {
            t
        }
    };
    for b in &mut show.bookmarks {
        b.at_ms = moved(b.at_ms);
    }
    for l in &mut show.loops {
        l.start_ms = moved(l.start_ms);
        l.end_ms = moved(l.end_ms);
    }
    for m in &mut show.keyframes {
        m.at_ms = moved(m.at_ms);
    }
    for t in &mut show.sequences {
        for c in &mut t.cues {
            c.at_ms = moved(c.at_ms);
        }
    }
    for t in &mut show.overlays {
        for c in &mut t.clips {
            c.start_ms = moved(c.start_ms);
        }
    }
    for t in &mut show.audio {
        for c in &mut t.clips {
            c.start_ms = moved(c.start_ms);
        }
    }
    tidy_bookmarks(show);
}

/// Trims program clip `clip` to end at `end_ms` (its right edge dragged): the clips after
/// it ripple, and what sat at or after its old end moves with them. Returns whether it
/// changed.
pub fn trim_program_clip(show: &mut ShowInfo, clip: &str, end_ms: u32) -> bool {
    let starts: Vec<u32> = show.program_starts().collect();
    let Some(n) = show.program.iter().position(|c| c.clip_id == clip) else {
        return false;
    };
    let start = starts[n];
    let old_end = start + show.program[n].length_ms;
    let length = end_ms.saturating_sub(start).max(MIN_CLIP_MS);
    if length == show.program[n].length_ms {
        return false;
    }
    show.program[n].length_ms = length;
    shift_from(
        show,
        old_end,
        i64::from(start + length) - i64::from(old_end),
    );
    true
}

/// Moves program clip `clip` to place `to` (0 first) among the program clips, as a drag
/// reorders it. What sits on other tracks stays at its time. Returns whether it moved.
pub fn move_program_clip(show: &mut ShowInfo, clip: &str, to: usize) -> bool {
    let Some(from) = show.program.iter().position(|c| c.clip_id == clip) else {
        return false;
    };
    let to = to.min(show.program.len() - 1);
    if from == to {
        return false;
    }
    let c = show.program.remove(from);
    show.program.insert(to, c);
    true
}

/// Sets the transition on the cut into program clip `clip`; `None` is Default.
pub fn set_cut(show: &mut ShowInfo, clip: &str, preset: Option<TransitionPresetId>) -> bool {
    match show.program.iter_mut().find(|c| c.clip_id == clip) {
        Some(c) if c.preset_id != preset => {
            c.preset_id = preset;
            true
        }
        _ => false,
    }
}

/// Removes clip, marker, cue, bookmark or loop `id`, wherever it is. A program clip's
/// removal ripples: what sat after it moves back by its length.
pub fn remove(show: &mut ShowInfo, id: &str) -> bool {
    let starts: Vec<u32> = show.program_starts().collect();
    if let Some(n) = show.program.iter().position(|c| c.clip_id == id) {
        let c = show.program.remove(n);
        let end = starts[n] + c.length_ms;
        shift_from(show, end, -i64::from(c.length_ms));
        return true;
    }
    let before = show.clone();
    for t in &mut show.overlays {
        t.clips.retain(|c| c.clip_id != id);
    }
    for t in &mut show.audio {
        t.clips.retain(|c| c.clip_id != id);
    }
    for t in &mut show.sequences {
        t.cues.retain(|c| c.cue_id != id);
    }
    show.keyframes.retain(|m| m.marker_id != id);
    show.bookmarks.retain(|b| b.bookmark_id != id);
    show.loops.retain(|l| l.loop_id != id);
    *show != before
}

/// Adds a bookmark at `at_ms` (the Bookmark tool's click), pausing, named "Bookmark N".
/// Returns its ID, or `None` when one is already there.
pub fn add_bookmark(show: &mut ShowInfo, at_ms: u32) -> Option<String> {
    if show.bookmarks.iter().any(|b| b.at_ms == at_ms) {
        return None;
    }
    let id = fresh_id(show, "bm");
    let n = show.bookmarks.len() + 1;
    show.bookmarks.push(ShowBookmark {
        bookmark_id: id.clone(),
        name: format!("Bookmark {n}"),
        at_ms,
        pausing: true,
        preset_id: None,
        continue_after_ms: None,
        note: String::new(),
    });
    tidy_bookmarks(show);
    Some(id)
}

/// Moves bookmark `id` to `at_ms`, unless another is there.
pub fn move_bookmark(show: &mut ShowInfo, id: &str, at_ms: u32) -> bool {
    if show
        .bookmarks
        .iter()
        .any(|b| b.at_ms == at_ms && b.bookmark_id != id)
    {
        return false;
    }
    match show.bookmarks.iter_mut().find(|b| b.bookmark_id == id) {
        Some(b) if b.at_ms != at_ms => {
            b.at_ms = at_ms;
            show.bookmarks.sort_by_key(|b| b.at_ms);
            true
        }
        _ => false,
    }
}

/// Flips bookmark `id` between pausing and non-pausing. A non-pausing one keeps no wait.
pub fn toggle_pausing(show: &mut ShowInfo, id: &str) -> bool {
    let Some(b) = show.bookmarks.iter_mut().find(|b| b.bookmark_id == id) else {
        return false;
    };
    b.pausing = !b.pausing;
    if !b.pausing {
        b.continue_after_ms = None;
    }
    true
}

/// Renames bookmark, loop or track `id`.
pub fn rename(show: &mut ShowInfo, id: &str, name: &str) -> bool {
    let name = name.trim().to_owned();
    let slot = show
        .bookmarks
        .iter_mut()
        .find(|b| b.bookmark_id == id)
        .map(|b| &mut b.name)
        .or_else(|| {
            show.loops
                .iter_mut()
                .find(|l| l.loop_id == id)
                .map(|l| &mut l.name)
        })
        .or_else(|| {
            show.overlays
                .iter_mut()
                .find(|t| t.track_id == id)
                .map(|t| &mut t.name)
        });
    match slot {
        Some(slot) if *slot != name => {
            *slot = name;
            true
        }
        _ => false,
    }
}

/// Adds a loop from `start_ms` to `end_ms` (the Loop tool's drag along the ruler), named
/// "Loop N". Refused (`None`) when shorter than the shortest loop or when it would
/// overlap another: loops don't nest or overlap in this version.
pub fn add_loop(show: &mut ShowInfo, start_ms: u32, end_ms: u32) -> Option<String> {
    let (start_ms, end_ms) = (start_ms.min(end_ms), start_ms.max(end_ms));
    if end_ms - start_ms < MIN_SHOW_LOOP_MS || overlaps(show, None, start_ms, end_ms) {
        return None;
    }
    let id = fresh_id(show, "loop");
    let n = show.loops.len() + 1;
    show.loops.push(ShowLoop {
        loop_id: id.clone(),
        name: format!("Loop {n}"),
        start_ms,
        end_ms,
        preset_id: None,
        leave: Default::default(),
    });
    show.loops.sort_by_key(|l| l.start_ms);
    Some(id)
}

/// Whether `start..end` overlaps a loop other than `except`.
fn overlaps(show: &ShowInfo, except: Option<&str>, start: u32, end: u32) -> bool {
    show.loops
        .iter()
        .filter(|l| Some(l.loop_id.as_str()) != except)
        .any(|l| start < l.end_ms && l.start_ms < end)
}

/// Moves loop `id`'s edges (one dragged, or the whole bar), unless it would overlap
/// another loop or get shorter than the shortest.
pub fn set_loop(show: &mut ShowInfo, id: &str, start_ms: u32, end_ms: u32) -> bool {
    if end_ms < start_ms + MIN_SHOW_LOOP_MS || overlaps(show, Some(id), start_ms, end_ms) {
        return false;
    }
    match show.loops.iter_mut().find(|l| l.loop_id == id) {
        Some(l) if (l.start_ms, l.end_ms) != (start_ms, end_ms) => {
            l.start_ms = start_ms;
            l.end_ms = end_ms;
            show.loops.sort_by_key(|l| l.start_ms);
            true
        }
        _ => false,
    }
}

/// Adds an overlay track on top. Returns its ID, or `None` at the most tracks a show
/// holds.
pub fn add_overlay_track(show: &mut ShowInfo) -> Option<String> {
    if show.overlays.len() >= MAX_SHOW_TRACKS {
        return None;
    }
    let id = fresh_id(show, "ov");
    show.overlays.push(OverlayTrack {
        track_id: id.clone(),
        name: String::new(),
        clips: Vec::new(),
    });
    Some(id)
}

/// Adds an audio track at the bottom. Returns its ID, or `None` at the most tracks.
pub fn add_audio_track(show: &mut ShowInfo) -> Option<String> {
    if show.audio.len() >= MAX_SHOW_TRACKS {
        return None;
    }
    let id = fresh_id(show, "au");
    show.audio.push(AudioTrack {
        track_id: id.clone(),
        name: String::new(),
        clips: Vec::new(),
    });
    Some(id)
}

/// Where a clip of `length_ms` fits on a track of `spans` (start, length) near `at_ms`:
/// at `at_ms` if free, else up against the clip it hit, on the side nearer the drop.
/// `None` when no gap there is long enough.
fn fit(spans: &[(u32, u32)], at_ms: u32, length_ms: u32) -> Option<u32> {
    let free = |s: u32| spans.iter().all(|&(a, l)| s + length_ms <= a || a + l <= s);
    if free(at_ms) {
        return Some(at_ms);
    }
    let hit = spans
        .iter()
        .find(|&&(a, l)| at_ms < a + l && a < at_ms + length_ms)?;
    let after = hit.0 + hit.1;
    let before = hit.0.checked_sub(length_ms);
    let mid = hit.0 + hit.1 / 2;
    let order = if at_ms < mid {
        [before, Some(after)]
    } else {
        [Some(after), before]
    };
    order.into_iter().flatten().find(|&s| free(s))
}

/// Places scene `scene` on overlay track `track` at `at_ms`, `length_ms` long, where it
/// fits without overlapping. Returns the clip's ID, or `None` when it doesn't fit there.
pub fn add_overlay_clip(
    show: &mut ShowInfo,
    track: &str,
    scene: SceneId,
    at_ms: u32,
    length_ms: u32,
) -> Option<String> {
    let id = fresh_id(show, "clip");
    let t = show.overlays.iter_mut().find(|t| t.track_id == track)?;
    let spans: Vec<(u32, u32)> = t.clips.iter().map(|c| (c.start_ms, c.length_ms)).collect();
    let start_ms = fit(&spans, at_ms, length_ms.max(MIN_CLIP_MS))?;
    t.clips.push(OverlayClip {
        clip_id: id.clone(),
        scene_id: scene,
        start_ms,
        length_ms: length_ms.max(MIN_CLIP_MS),
        fade_in_ms: 0,
        fade_out_ms: 0,
    });
    t.clips.sort_by_key(|c| c.start_ms);
    Some(id)
}

/// Places source `source` on audio track `track` at `at_ms`, `length_ms` long, with
/// `fades` (in, out) fitted inside it. Returns the clip's ID, or `None` when it doesn't
/// fit there.
pub fn add_audio_clip(
    show: &mut ShowInfo,
    track: &str,
    source: SourceId,
    at_ms: u32,
    length_ms: u32,
    fades: (u32, u32),
) -> Option<String> {
    let id = fresh_id(show, "clip");
    let t = show.audio.iter_mut().find(|t| t.track_id == track)?;
    let spans: Vec<(u32, u32)> = t.clips.iter().map(|c| (c.start_ms, c.length_ms)).collect();
    let length_ms = length_ms.max(MIN_CLIP_MS);
    let start_ms = fit(&spans, at_ms, length_ms)?;
    let (fade_in_ms, fade_out_ms) = fit_fades(fades, length_ms);
    t.clips.push(AudioClip {
        clip_id: id.clone(),
        source_id: source,
        start_ms,
        length_ms,
        fade_in_ms,
        fade_out_ms,
        gain_db: Default::default(),
    });
    t.clips.sort_by_key(|c| c.start_ms);
    Some(id)
}

/// Fades shrunk in proportion so that together they fit in `length_ms`.
fn fit_fades((fade_in, fade_out): (u32, u32), length_ms: u32) -> (u32, u32) {
    let total = u64::from(fade_in) + u64::from(fade_out);
    if total <= u64::from(length_ms) || total == 0 {
        return (fade_in, fade_out);
    }
    let scale = |f: u32| u32::try_from(u64::from(f) * u64::from(length_ms) / total).unwrap_or(0);
    (scale(fade_in), scale(fade_out))
}

/// Moves or trims overlay or audio clip `clip` to start at `start_ms` and last
/// `length_ms`, unless that would overlap another clip on its track. An audio clip's
/// fades shrink to fit.
pub fn set_span(show: &mut ShowInfo, clip: &str, start_ms: u32, length_ms: u32) -> bool {
    let length_ms = length_ms.max(MIN_CLIP_MS);
    let clear = |spans: Vec<(u32, u32)>| {
        spans
            .iter()
            .all(|&(a, l)| start_ms + length_ms <= a || a + l <= start_ms)
    };
    for t in &mut show.overlays {
        if let Some(n) = t.clips.iter().position(|c| c.clip_id == clip) {
            let others = t
                .clips
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != n)
                .map(|(_, c)| (c.start_ms, c.length_ms))
                .collect();
            let c = &mut t.clips[n];
            if !clear(others) || (c.start_ms, c.length_ms) == (start_ms, length_ms) {
                return false;
            }
            c.start_ms = start_ms;
            c.length_ms = length_ms;
            (c.fade_in_ms, c.fade_out_ms) = fit_fades((c.fade_in_ms, c.fade_out_ms), length_ms);
            t.clips.sort_by_key(|c| c.start_ms);
            return true;
        }
    }
    for t in &mut show.audio {
        if let Some(n) = t.clips.iter().position(|c| c.clip_id == clip) {
            let others = t
                .clips
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != n)
                .map(|(_, c)| (c.start_ms, c.length_ms))
                .collect();
            let c = &mut t.clips[n];
            if !clear(others) || (c.start_ms, c.length_ms) == (start_ms, length_ms) {
                return false;
            }
            c.start_ms = start_ms;
            c.length_ms = length_ms;
            (c.fade_in_ms, c.fade_out_ms) = fit_fades((c.fade_in_ms, c.fade_out_ms), length_ms);
            t.clips.sort_by_key(|c| c.start_ms);
            return true;
        }
    }
    false
}

/// Sets audio or overlay clip `clip`'s fades (a fade handle dragged), fitted inside it.
pub fn set_fades(show: &mut ShowInfo, clip: &str, fades: (u32, u32)) -> bool {
    let fades = (
        fades.0.min(jstream_control::protocol::MAX_SHOW_FADE_MS),
        fades.1.min(jstream_control::protocol::MAX_SHOW_FADE_MS),
    );
    for t in &mut show.audio {
        if let Some(c) = t.clips.iter_mut().find(|c| c.clip_id == clip) {
            let f = fit_fades(fades, c.length_ms);
            if (c.fade_in_ms, c.fade_out_ms) == f {
                return false;
            }
            (c.fade_in_ms, c.fade_out_ms) = f;
            return true;
        }
    }
    for t in &mut show.overlays {
        if let Some(c) = t.clips.iter_mut().find(|c| c.clip_id == clip) {
            let f = fit_fades(fades, c.length_ms);
            if (c.fade_in_ms, c.fade_out_ms) == f {
                return false;
            }
            (c.fade_in_ms, c.fade_out_ms) = f;
            return true;
        }
    }
    false
}

/// Adds a keyframe marker at `at_ms` loading `keyframe` (or the scene's own values).
pub fn add_keyframe(show: &mut ShowInfo, at_ms: u32, keyframe: Option<SceneId>) -> String {
    let id = fresh_id(show, "kf");
    show.keyframes.push(KeyframeMarker {
        marker_id: id.clone(),
        at_ms,
        keyframe_id: keyframe,
        preset_id: None,
    });
    show.keyframes.sort_by_key(|m| m.at_ms);
    id
}

/// Splits program clip `clip` at `at_ms` into two clips of the same scene, the second
/// cut in with Default, as a new clip is. Returns the new clip's ID, or
/// `None` when `at_ms` is not inside it by at least the shortest clip either side.
pub fn split_program_clip(show: &mut ShowInfo, clip: &str, at_ms: u32) -> Option<String> {
    let starts: Vec<u32> = show.program_starts().collect();
    let n = show.program.iter().position(|c| c.clip_id == clip)?;
    let (start, length) = (starts[n], show.program[n].length_ms);
    if at_ms < start + MIN_CLIP_MS || at_ms + MIN_CLIP_MS > start + length {
        return None;
    }
    let id = fresh_id(show, "clip");
    let scene = show.program[n].scene_id.clone();
    show.program[n].length_ms = at_ms - start;
    show.program.insert(
        n + 1,
        ProgramClip {
            clip_id: id.clone(),
            scene_id: scene,
            length_ms: start + length - at_ms,
            preset_id: None,
        },
    );
    Some(id)
}

/// Where program clip `clip` goes among the others when dragged to `at_ms`: the place
/// (0 first) after every other clip whose middle comes before `at_ms`, counted with
/// `clip` taken out, as [`move_program_clip`] takes it.
pub fn program_drop_index(show: &ShowInfo, clip: &str, at_ms: u32) -> usize {
    let mut start = 0_u32;
    let mut index = 0;
    for c in show.program.iter().filter(|c| c.clip_id != clip) {
        if start + c.length_ms / 2 <= at_ms {
            index += 1;
        }
        start += c.length_ms;
    }
    index
}

/// Moves keyframe marker or cue `id` to `at_ms` (its marker dragged).
pub fn move_marker(show: &mut ShowInfo, id: &str, at_ms: u32) -> bool {
    if let Some(m) = show.keyframes.iter_mut().find(|m| m.marker_id == id) {
        if m.at_ms == at_ms {
            return false;
        }
        m.at_ms = at_ms;
        show.keyframes.sort_by_key(|m| m.at_ms);
        return true;
    }
    for t in &mut show.sequences {
        if let Some(c) = t.cues.iter_mut().find(|c| c.cue_id == id) {
            if c.at_ms == at_ms {
                return false;
            }
            c.at_ms = at_ms;
            t.cues.sort_by_key(|c| c.at_ms);
            return true;
        }
    }
    false
}

/// A copy of `show` as a new show `id` called `name`: the picker's Duplicate. Element
/// IDs are per show, so they stay as they are.
pub fn duplicate(show: &ShowInfo, id: &str, name: &str) -> ShowInfo {
    let mut copy = show.clone();
    copy.show_id = ShowId::new(id);
    copy.name = name.to_owned();
    copy
}

/// A name for a new show: "Show N", the first no show in `taken` has.
pub fn new_show_name(taken: &[&str]) -> String {
    (1..)
        .map(|n| format!("Show {n}"))
        .find(|name| !taken.contains(&name.as_str()))
        .unwrap_or_else(|| "Show".to_owned())
}

/// What a copy of a show called `name` is called: "Friday show copy", then "… copy 2".
pub fn copy_name(name: &str, taken: &[&str]) -> String {
    let first = format!("{name} copy");
    if !taken.contains(&first.as_str()) {
        return first;
    }
    (2..)
        .map(|n| format!("{name} copy {n}"))
        .find(|n| !taken.contains(&n.as_str()))
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show() -> ShowInfo {
        let mut s = new_show("friday", "Friday show");
        insert_program_clip(&mut s, "soon".into(), "Starting soon", 0, 180_000, false);
        insert_program_clip(&mut s, "main".into(), "Main", u32::MAX, 180_000, true);
        s
    }

    fn starts(s: &ShowInfo) -> Vec<u32> {
        s.program_starts().collect()
    }

    #[test]
    fn a_scene_dropped_on_program_ends_in_a_pausing_bookmark() {
        let s = show();
        assert_eq!(starts(&s), [0, 180_000]);
        assert_eq!(s.bookmarks.len(), 1);
        assert_eq!(
            (s.bookmarks[0].at_ms, s.bookmarks[0].pausing),
            (360_000, true)
        );
        assert_eq!(s.bookmarks[0].name, "Main");
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn a_drop_into_a_clips_first_half_goes_before_it_and_pushes_the_rest_on() {
        let mut s = show();
        add_bookmark(&mut s, 200_000);
        insert_program_clip(&mut s, "intro".into(), "intro", 190_000, 60_000, false);
        assert_eq!(starts(&s), [0, 180_000, 240_000]);
        assert_eq!(s.program[1].scene_id.as_str(), "intro");
        let times: Vec<u32> = s.bookmarks.iter().map(|b| b.at_ms).collect();
        assert_eq!(times, [260_000, 420_000], "what sat on Main moved with it");
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn trimming_a_program_clip_ripples() {
        let mut s = show();
        assert!(trim_program_clip(&mut s, "clip1", 120_000));
        assert_eq!(starts(&s), [0, 120_000]);
        assert_eq!(s.bookmarks[0].at_ms, 300_000);
        assert!(
            !trim_program_clip(&mut s, "clip1", 120_000),
            "no change, no edit"
        );
        assert!(trim_program_clip(&mut s, "clip1", 0));
        assert_eq!(s.program[0].length_ms, MIN_CLIP_MS);
    }

    #[test]
    fn removing_a_program_clip_closes_the_gap() {
        let mut s = show();
        assert!(remove(&mut s, "clip1"));
        assert_eq!(starts(&s), [0]);
        assert_eq!(s.bookmarks[0].at_ms, 180_000);
        assert!(remove(&mut s, "bm1"));
        assert!(s.bookmarks.is_empty());
        assert!(!remove(&mut s, "nothing"));
    }

    #[test]
    fn bookmarks_stay_one_per_time_in_order() {
        let mut s = show();
        let id = add_bookmark(&mut s, 10_000).unwrap_or_default();
        assert_eq!(add_bookmark(&mut s, 10_000), None);
        assert!(!move_bookmark(&mut s, &id, 360_000), "another is there");
        assert!(move_bookmark(&mut s, &id, 400_000));
        assert_eq!(
            s.bookmarks.last().map(|b| b.bookmark_id.as_str()),
            Some(id.as_str())
        );
        assert!(toggle_pausing(&mut s, &id));
        assert!(!s.bookmarks[1].pausing);
        assert!(rename(&mut s, &id, " Demo "));
        assert_eq!(s.bookmarks[1].name, "Demo");
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn loops_never_overlap() {
        let mut s = show();
        let id = add_loop(&mut s, 180_000, 0).unwrap_or_default();
        assert_eq!((s.loops[0].start_ms, s.loops[0].end_ms), (0, 180_000));
        assert_eq!(add_loop(&mut s, 100_000, 200_000), None);
        assert_eq!(add_loop(&mut s, 200_000, 200_050), None, "too short");
        assert!(
            add_loop(&mut s, 180_000, 240_000).is_some(),
            "touching is fine"
        );
        assert!(!set_loop(&mut s, &id, 0, 200_000));
        assert!(set_loop(&mut s, &id, 0, 170_000));
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn overlay_clips_fit_around_each_other() {
        let mut s = show();
        let track = add_overlay_track(&mut s).unwrap_or_default();
        let a = add_overlay_clip(&mut s, &track, "logo".into(), 10_000, 50_000);
        assert!(a.is_some());
        // Dropped onto the first clip's second half: placed after it.
        let b = add_overlay_clip(&mut s, &track, "title".into(), 40_000, 20_000);
        assert!(b.is_some());
        let spans: Vec<(u32, u32)> = s.overlays[0]
            .clips
            .iter()
            .map(|c| (c.start_ms, c.length_ms))
            .collect();
        assert_eq!(spans, [(10_000, 50_000), (60_000, 20_000)]);
        let b = b.unwrap_or_default();
        assert!(!set_span(&mut s, &b, 30_000, 20_000), "would overlap");
        assert!(set_span(&mut s, &b, 70_000, 5_000));
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn audio_fades_fit_inside_their_clip() {
        let mut s = show();
        let track = add_audio_track(&mut s).unwrap_or_default();
        let id = add_audio_clip(&mut s, &track, "mic".into(), 0, 1_000, (1_500, 500))
            .unwrap_or_default();
        let c = &s.audio[0].clips[0];
        assert_eq!((c.fade_in_ms, c.fade_out_ms), (750, 250));
        assert!(set_fades(&mut s, &id, (100, 200)));
        assert!(set_span(&mut s, &id, 0, 200));
        let c = &s.audio[0].clips[0];
        assert!(c.fade_in_ms + c.fade_out_ms <= 200);
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn splitting_and_reordering_program_clips() {
        let mut s = show();
        let id = split_program_clip(&mut s, "clip1", 60_000).unwrap_or_default();
        assert_eq!(starts(&s), [0, 60_000, 180_000]);
        assert_eq!(split_program_clip(&mut s, &id, 60_050), None);
        assert!(move_program_clip(&mut s, "clip2", 0));
        assert_eq!(s.program[0].scene_id.as_str(), "main");
        assert!(set_cut(&mut s, &id, Some("fade".into())));
        assert!(!set_cut(&mut s, &id, Some("fade".into())));
        assert_eq!(s.problem(), None);
    }

    #[test]
    fn drags_snap_to_edges_then_to_a_tenth_of_a_second() {
        let s = show();
        let points = snap_points(&s);
        assert_eq!(points, [0, 180_000, 360_000]);
        assert_eq!(snap(179_400.0, &points, 1000, false), 180_000);
        assert_eq!(snap(170_040.0, &points, 1000, false), 170_000);
        assert_eq!(snap(170_040.0, &points, 1000, true), 170_040);
        assert_eq!(snap_reach(100.0), 600);
    }

    #[test]
    fn a_dragged_program_clip_lands_by_the_middles_it_passes() {
        let mut s = show();
        insert_program_clip(&mut s, "outro".into(), "Outro", u32::MAX, 60_000, false);
        // Starting soon 0..3:00, Main 3:00..6:00, Outro 6:00..7:00.
        assert_eq!(program_drop_index(&s, "clip1", 0), 0);
        assert_eq!(program_drop_index(&s, "clip1", 100_000), 1);
        assert_eq!(program_drop_index(&s, "clip1", 500_000), 2);
        assert_eq!(program_drop_index(&s, "clip3", 10_000), 0);
        assert!(move_program_clip(&mut s, "clip3", 0));
        assert_eq!(s.program[0].scene_id.as_str(), "outro");
    }

    #[test]
    fn a_clip_added_at_the_end_leaves_the_pause_before_it() {
        let mut s = show();
        insert_program_clip(&mut s, "outro".into(), "Outro", u32::MAX, 60_000, true);
        let pauses: Vec<(u32, &str)> = s
            .bookmarks
            .iter()
            .map(|b| (b.at_ms, b.name.as_str()))
            .collect();
        assert_eq!(pauses, [(360_000, "Main"), (420_000, "Outro")]);
        // A clip put in between keeps Main's pause at Main's end.
        insert_program_clip(&mut s, "break".into(), "Break", 361_000, 10_000, false);
        let times: Vec<u32> = s.bookmarks.iter().map(|b| b.at_ms).collect();
        assert_eq!(times, [360_000, 430_000]);
        assert_eq!(s.program[2].scene_id.as_str(), "break");
    }

    #[test]
    fn markers_move_and_shows_copy() {
        let mut s = show();
        let k = add_keyframe(&mut s, 10_000, None);
        add_keyframe(&mut s, 20_000, None);
        assert!(move_marker(&mut s, &k, 30_000));
        assert_eq!(s.keyframes[1].marker_id, k);
        assert!(!move_marker(&mut s, &k, 30_000));
        assert!(!move_marker(&mut s, "nothing", 1));
        let copy = duplicate(&s, "show2", "Friday show copy");
        assert_eq!(copy.show_id.as_str(), "show2");
        assert_eq!(copy.program, s.program);
        assert_eq!(new_show_name(&["Show 1"]), "Show 2");
        assert_eq!(copy_name("A", &["A copy"]), "A copy 2");
    }
}
