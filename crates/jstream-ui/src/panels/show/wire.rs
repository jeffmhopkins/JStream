//! The Show panel and the engine (ADR-0210): a [`ShowInfo`] from `state.get` read as the
//! [`ShowView`] the panel draws, the [`RunningShow`] from `studio.get` as its [`RunView`],
//! and each [`Command`] as the `show.*` action it runs.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::Command;
use super::model::{
    BookmarkView, ClipKind, ClipView, LoopView, NextMonitor, RunPhase, RunView, ShowView,
    TrackKind, TrackView,
};
use crate::store::Collection;
use jstream_control::protocol::{
    Action, NextMonitor as WireMonitor, RunningShow, SceneId, ShowId, ShowInfo, ShowState,
    SourceId, TimelineId, TransitionPresetId,
};

/// A scene's name, or "(missing)" for one the collection doesn't hold.
fn scene_name(collection: &Collection, scene: &SceneId) -> (String, bool) {
    collection
        .scenes
        .iter()
        .find(|s| &s.scene_id == scene)
        .map_or_else(
            || ("(missing)".to_owned(), true),
            |s| (s.name.clone(), false),
        )
}

/// A source's name, or "(missing)".
fn source_name(collection: &Collection, source: &SourceId) -> (String, bool) {
    collection
        .sources
        .iter()
        .find(|s| &s.source_id == source)
        .map_or_else(
            || ("(missing)".to_owned(), true),
            |s| (s.name.clone(), false),
        )
}

/// A sequence's name and length, or "(missing)".
fn sequence(collection: &Collection, timeline: &TimelineId) -> (String, u32, bool) {
    collection
        .scenes
        .iter()
        .flat_map(|s| &s.timelines)
        .find(|t| &t.timeline_id == timeline)
        .map_or_else(
            || ("(missing)".to_owned(), 0, true),
            |t| {
                (
                    crate::panels::sequence::timeline_name(t),
                    t.duration_ms,
                    false,
                )
            },
        )
}

/// The shape a sequence clip draws: its first lane's points, as (fraction of its length,
/// value), or none for a sequence with no lanes.
fn sequence_curve(collection: &Collection, timeline: &TimelineId) -> Vec<(f32, f32)> {
    let Some(t) = collection
        .scenes
        .iter()
        .flat_map(|s| &s.timelines)
        .find(|t| &t.timeline_id == timeline)
    else {
        return Vec::new();
    };
    let length = t.duration_ms.max(1) as f32;
    t.lanes.first().map_or_else(Vec::new, |lane| {
        lane.points
            .iter()
            .map(|p| (p.at_ms as f32 / length, p.value.get()))
            .collect()
    })
}

/// A transition preset's badge text on a cut: "Fade 0.5 s", or "Default".
pub fn preset_badge(collection: &Collection, preset: Option<&TransitionPresetId>) -> String {
    let Some(preset) = preset else {
        return "Default".to_owned();
    };
    collection
        .transitions
        .presets
        .iter()
        .find(|p| &p.preset_id == preset)
        .map_or_else(
            || "(missing)".to_owned(),
            |p| {
                if p.duration_ms == 0 {
                    p.name.clone()
                } else {
                    let s = f64::from(p.duration_ms) / 1000.0;
                    format!("{} {} s", p.name, trim(s))
                }
            },
        )
}

/// A number of seconds without a trailing `.0`: 0.5, 1, 1.25.
fn trim(s: f64) -> String {
    let text = format!("{s:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Whether `scene` holds a media file (a video or audio file), so a jump into its clip
/// seeks: the clock badge.
pub fn is_timed(collection: &Collection, scene: &SceneId) -> bool {
    collection
        .scenes
        .iter()
        .find(|s| &s.scene_id == scene)
        .is_some_and(|s| {
            s.items.iter().any(|item| {
                collection.sources.iter().any(|src| {
                    src.source_id == item.source_id
                        && matches!(&src.kind, jstream_control::protocol::SourceKind::Builtin { kind } if kind == "media")
                })
            })
        })
}

/// The key caps of the hotkeys bound to `wanted`, as people read them.
pub fn keys_for(collection: &Collection, wanted: &Action) -> Vec<String> {
    collection
        .hotkeys
        .iter()
        .filter(|h| {
            serde_json::from_value::<Action>(h.action.clone())
                .ok()
                .as_ref()
                == Some(wanted)
        })
        .map(|h| crate::hotkeys::label(&h.chord))
        .collect()
}

/// The show as the panel draws it.
pub fn view_of(show: &ShowInfo, collection: &Collection) -> ShowView {
    let mut tracks = Vec::new();
    // Overlays sit above program, the highest first.
    for (n, track) in show.overlays.iter().enumerate().rev() {
        let clips = track
            .clips
            .iter()
            .map(|c| {
                let (label, missing) = scene_name(collection, &c.scene_id);
                ClipView {
                    id: c.clip_id.clone(),
                    start_ms: c.start_ms,
                    len_ms: c.length_ms,
                    label,
                    detail: super::time::clock(c.length_ms),
                    kind: ClipKind::Scene(c.scene_id.to_string()),
                    transition_in: None,
                    timed: is_timed(collection, &c.scene_id),
                    missing,
                    fades: Some((c.fade_in_ms, c.fade_out_ms)),
                    curve: Vec::new(),
                }
            })
            .collect();
        tracks.push(TrackView {
            id: track.track_id.clone(),
            kind: TrackKind::Overlay(u8::try_from(n + 2).unwrap_or(u8::MAX)),
            clips,
            muted: false,
            locked: false,
        });
    }
    let program = show
        .program
        .iter()
        .zip(show.program_starts())
        .enumerate()
        .map(|(n, (c, start))| {
            let (label, missing) = scene_name(collection, &c.scene_id);
            ClipView {
                id: c.clip_id.clone(),
                start_ms: start,
                len_ms: c.length_ms,
                label,
                detail: super::time::clock(c.length_ms),
                kind: ClipKind::Scene(c.scene_id.to_string()),
                transition_in: (n > 0).then(|| preset_badge(collection, c.preset_id.as_ref())),
                timed: is_timed(collection, &c.scene_id),
                missing,
                fades: None,
                curve: Vec::new(),
            }
        })
        .collect();
    tracks.push(TrackView {
        id: "program".to_owned(),
        kind: TrackKind::Program,
        clips: program,
        muted: false,
        locked: false,
    });
    let keyframes = show
        .keyframes
        .iter()
        .map(|m| {
            let (label, missing) = m.keyframe_id.as_ref().map_or_else(
                || ("Scene's own".to_owned(), false),
                |k| scene_name(collection, k),
            );
            ClipView {
                id: m.marker_id.clone(),
                start_ms: m.at_ms,
                len_ms: 0,
                label,
                detail: String::new(),
                kind: ClipKind::Keyframe(
                    m.keyframe_id
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                ),
                transition_in: None,
                timed: false,
                missing,
                fades: None,
                curve: Vec::new(),
            }
        })
        .collect();
    tracks.push(TrackView {
        id: "keyframes".to_owned(),
        kind: TrackKind::Keyframes,
        clips: keyframes,
        muted: false,
        locked: false,
    });
    for (n, track) in show.sequences.iter().enumerate() {
        let clips = track
            .cues
            .iter()
            .map(|cue| match cue.plays() {
                Some(timeline) => {
                    let (label, len, missing) = sequence(collection, &timeline);
                    ClipView {
                        id: cue.cue_id.clone(),
                        start_ms: cue.at_ms,
                        len_ms: len,
                        label,
                        detail: super::time::clock(len),
                        kind: ClipKind::Sequence(timeline.to_string()),
                        transition_in: None,
                        timed: false,
                        missing,
                        fades: None,
                        curve: sequence_curve(collection, &timeline),
                    }
                }
                None => {
                    let what = cue.action().map_or_else(
                        || "(unknown action)".to_owned(),
                        |a| crate::panels::events::action_text(collection, &a),
                    );
                    ClipView {
                        id: cue.cue_id.clone(),
                        start_ms: cue.at_ms,
                        len_ms: 0,
                        label: what.clone(),
                        detail: String::new(),
                        kind: ClipKind::Event(what),
                        transition_in: None,
                        timed: false,
                        missing: false,
                        fades: None,
                        curve: Vec::new(),
                    }
                }
            })
            .collect();
        tracks.push(TrackView {
            id: track.track_id.clone(),
            kind: TrackKind::Sequences(u8::try_from(n + 1).unwrap_or(u8::MAX)),
            clips,
            muted: false,
            locked: false,
        });
    }
    for (n, track) in show.audio.iter().enumerate() {
        let clips = track
            .clips
            .iter()
            .map(|c| {
                let (label, missing) = source_name(collection, &c.source_id);
                ClipView {
                    id: c.clip_id.clone(),
                    start_ms: c.start_ms,
                    len_ms: c.length_ms,
                    label,
                    detail: fades_detail(c.fade_in_ms, c.fade_out_ms),
                    kind: ClipKind::Audio(c.source_id.to_string()),
                    transition_in: None,
                    timed: false,
                    missing,
                    fades: Some((c.fade_in_ms, c.fade_out_ms)),
                    curve: Vec::new(),
                }
            })
            .collect();
        tracks.push(TrackView {
            id: track.track_id.clone(),
            kind: TrackKind::Audio(u8::try_from(n + 1).unwrap_or(u8::MAX)),
            clips,
            muted: false,
            locked: false,
        });
    }
    let bookmarks = show
        .bookmarks
        .iter()
        .map(|b| BookmarkView {
            id: b.bookmark_id.clone(),
            name: b.name.clone(),
            at_ms: b.at_ms,
            pausing: b.pausing,
            key: keys_for(
                collection,
                &Action::ShowGoTo {
                    show_id: show.show_id.clone(),
                    bookmark_id: b.bookmark_id.clone(),
                },
            )
            .into_iter()
            .next(),
            note: b.note.clone(),
        })
        .collect();
    let loops = show
        .loops
        .iter()
        .map(|l| LoopView {
            id: l.loop_id.clone(),
            name: l.name.clone(),
            start_ms: l.start_ms,
            end_ms: l.end_ms,
        })
        .collect();
    ShowView {
        id: show.show_id.to_string(),
        name: if show.name.is_empty() {
            show.show_id.to_string()
        } else {
            show.name.clone()
        },
        tracks,
        bookmarks,
        loops,
        planned_ms: show.planned_ms,
        next_monitor: match show.next_monitor {
            WireMonitor::Live => NextMonitor::Live,
            WireMonitor::Off => NextMonitor::Off,
            _ => NextMonitor::Still,
        },
        locked: false,
    }
}

/// An audio clip's second line: "fade in 0.5 s · fade out 1 s".
fn fades_detail(fade_in: u32, fade_out: u32) -> String {
    let part = |what: &str, ms: u32| {
        (ms > 0).then(|| format!("{what} {} s", trim(f64::from(ms) / 1000.0)))
    };
    [part("fade in", fade_in), part("fade out", fade_out)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Where the runner is, from `studio.get`'s `show` heard at `heard_at`.
pub fn run_of(show: Option<&(RunningShow, f64)>) -> RunView {
    let Some((s, heard_at)) = show else {
        return RunView::default();
    };
    let phase = match s.state {
        ShowState::Paused => RunPhase::Paused {
            bookmark: s.bookmark_id.clone().unwrap_or_default(),
            since: heard_at - f64::from(s.waited_ms.unwrap_or(0)),
        },
        ShowState::Playing => RunPhase::Playing,
        // Off script it waits where it was, as at a pause.
        _ => RunPhase::Paused {
            bookmark: String::new(),
            since: *heard_at,
        },
    };
    RunView {
        show: Some(s.show_id.to_string()),
        phase,
        at_ms: s.at_ms,
        heard_at: *heard_at,
        armed_ms: s.armed_ms,
        looping: s.loop_id.clone().map(|l| (l, s.pass.unwrap_or(0) + 1)),
        rehearsing: s.rehearse,
        off_script: s.off_script.as_ref().map(ToString::to_string),
        ..RunView::default()
    }
}

/// The action a command runs on show `show` (the one open), or `None` for one that is
/// the panel's own (Lock) or a show edit (the Next monitor and + Overlay track, sent as
/// `show.set`).
pub fn action_of(command: &Command, show: Option<&str>) -> Option<Action> {
    Some(match command {
        Command::Play(id) => Action::ShowPlay {
            show_id: ShowId::new(id.clone()),
            bookmark_id: None,
            rehearse: false,
        },
        Command::Stop => Action::ShowStop,
        Command::Continue => Action::ShowContinue,
        Command::Back => Action::ShowBack,
        Command::GoTo(bookmark) => return go_to(show, bookmark),
        Command::Arm(at_ms) => Action::ShowArm { at_ms: *at_ms },
        // Rehearsing starts the open show on preview; turning it off stops it.
        Command::Rehearse(true) => Action::ShowPlay {
            show_id: ShowId::new(show?.to_owned()),
            bookmark_id: None,
            rehearse: true,
        },
        Command::Rehearse(false) => Action::ShowStop,
        Command::Lock(..) | Command::NextMonitor(..) | Command::AddOverlayTrack(_) => {
            return None;
        }
    })
}

/// `show.go_to` for `bookmark` in `show`.
pub fn go_to(show: Option<&str>, bookmark: &str) -> Option<Action> {
    Some(Action::ShowGoTo {
        show_id: ShowId::new(show?.to_owned()),
        bookmark_id: bookmark.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{
        ProgramClip, SceneInfo, ShowBookmark, ShowLoop, TransitionPresetInfo,
    };

    fn scene(id: &str, name: &str) -> SceneInfo {
        serde_json::from_value(serde_json::json!({
            "scene_id": id, "name": name, "role": "scene", "items": []
        }))
        .expect("a scene")
    }

    fn collection() -> Collection {
        let mut c = Collection {
            scenes: vec![scene("soon", "Starting soon"), scene("main", "Main")],
            ..Collection::default()
        };
        c.transitions.presets.push(TransitionPresetInfo {
            preset_id: "fade".into(),
            name: "Fade".into(),
            ..c.transitions.presets[0].clone()
        });
        if let Some(fade) = c.transitions.presets.last_mut() {
            fade.duration_ms = 500;
        }
        c
    }

    fn show() -> ShowInfo {
        let mut s = ShowInfo::new("friday");
        s.name = "Friday show".into();
        s.program = vec![
            ProgramClip {
                clip_id: "c1".into(),
                scene_id: "soon".into(),
                length_ms: 180_000,
                preset_id: None,
            },
            ProgramClip {
                clip_id: "c2".into(),
                scene_id: "main".into(),
                length_ms: 60_000,
                preset_id: Some("fade".into()),
            },
            ProgramClip {
                clip_id: "c3".into(),
                scene_id: "gone".into(),
                length_ms: 60_000,
                preset_id: None,
            },
        ];
        s.bookmarks = vec![ShowBookmark {
            bookmark_id: "b1".into(),
            name: "Intro".into(),
            at_ms: 180_000,
            pausing: true,
            preset_id: None,
            continue_after_ms: None,
            note: String::new(),
        }];
        s.loops = vec![ShowLoop {
            loop_id: "l1".into(),
            name: "Waiting room".into(),
            start_ms: 0,
            end_ms: 180_000,
            preset_id: None,
            leave: Default::default(),
        }];
        s
    }

    #[test]
    fn program_clips_start_end_to_end_with_their_cut_badges() {
        let view = view_of(&show(), &collection());
        let program = view.program();
        assert_eq!(
            program.iter().map(|c| c.start_ms).collect::<Vec<_>>(),
            [0, 180_000, 240_000]
        );
        assert_eq!(program[0].transition_in, None, "the first clip has no cut");
        assert_eq!(program[1].transition_in.as_deref(), Some("Fade 0.5 s"));
        assert_eq!(program[2].transition_in.as_deref(), Some("Default"));
        assert_eq!(program[1].label, "Main");
        assert!(program[2].missing, "a gone scene stays as a missing clip");
        assert_eq!(view.name, "Friday show");
        assert_eq!(view.bookmarks[0].name, "Intro");
        assert_eq!(view.loops[0].end_ms, 180_000);
    }

    #[test]
    fn a_paused_show_counts_the_time_waited_from_when_it_was_heard() {
        let mut running = RunningShow {
            show_id: "friday".into(),
            at_ms: 180_000,
            state: ShowState::Paused,
            bookmark_id: Some("b1".into()),
            waited_ms: Some(2_000),
            off_script: None,
            loop_id: Some("l1".into()),
            pass: Some(2),
            armed_ms: None,
            rehearse: false,
        };
        let run = run_of(Some(&(running.clone(), 10_000.0)));
        assert_eq!(
            run.phase,
            RunPhase::Paused {
                bookmark: "b1".into(),
                since: 8_000.0
            }
        );
        assert_eq!(run.looping, Some(("l1".into(), 3)), "passes count from 1");
        running.state = ShowState::Playing;
        assert_eq!(run_of(Some(&(running, 0.0))).phase, RunPhase::Playing);
        assert_eq!(run_of(None), RunView::default());
    }

    #[test]
    fn commands_run_their_show_actions() {
        assert_eq!(
            action_of(&Command::Continue, None),
            Some(Action::ShowContinue)
        );
        assert_eq!(
            action_of(&Command::GoTo("b1".into()), Some("friday")),
            Some(Action::ShowGoTo {
                show_id: "friday".into(),
                bookmark_id: "b1".into()
            })
        );
        assert_eq!(action_of(&Command::GoTo("b1".into()), None), None);
        assert_eq!(
            action_of(&Command::Arm(Some(5)), None),
            Some(Action::ShowArm { at_ms: Some(5) })
        );
        assert_eq!(action_of(&Command::Lock("friday".into(), true), None), None);
    }
}
