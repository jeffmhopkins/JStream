use super::*;
use crate::{Action, ChannelSwitch};

fn clip(id: &str, length_ms: u32) -> ProgramClip {
    ProgramClip {
        clip_id: id.into(),
        scene_id: SceneId::new("scn"),
        length_ms,
        preset_id: None,
    }
}

fn bookmark(id: &str, at_ms: u32) -> ShowBookmark {
    ShowBookmark {
        bookmark_id: id.into(),
        name: String::new(),
        at_ms,
        pausing: true,
        preset_id: None,
        continue_after_ms: None,
        note: String::new(),
    }
}

fn field(show: &ShowInfo) -> Option<String> {
    show.problem().map(|(field, _)| field)
}

#[test]
fn a_new_show_is_valid_and_saves_only_its_id() {
    let show = ShowInfo::new("s");
    assert_eq!(show.problem(), None);
    assert_eq!(
        serde_json::to_value(&show).expect("serializes"),
        serde_json::json!({"show_id": "s", "next_monitor": "still"})
    );
    let back: ShowInfo =
        serde_json::from_value(serde_json::json!({"show_id": "s"})).expect("parses");
    assert_eq!(back, show);
}

#[test]
fn program_clips_ripple_end_to_end() {
    let show = ShowInfo {
        program: vec![clip("a", 1000), clip("b", 2500), clip("c", 500)],
        ..ShowInfo::new("s")
    };
    assert_eq!(show.program_starts().collect::<Vec<_>>(), [0, 1000, 3500]);
    assert_eq!(show.length_ms(), 4000);
}

#[test]
fn ids_are_unique_across_the_whole_show() {
    let show = ShowInfo {
        program: vec![clip("x", 1000)],
        bookmarks: vec![bookmark("x", 1000)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&show).as_deref(), Some("bookmarks.0.bookmark_id"));
    let bad = ShowInfo {
        program: vec![clip("not ok", 1000)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&bad).as_deref(), Some("program.0.clip_id"));
}

#[test]
fn a_program_clip_has_a_length_and_the_show_at_most_a_day() {
    let empty = ShowInfo {
        program: vec![clip("a", 0)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&empty).as_deref(), Some("program.0.length_ms"));
    let long = ShowInfo {
        program: vec![clip("a", MAX_SHOW_MS), clip("b", 1)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&long).as_deref(), Some("program.1.length_ms"));
}

#[test]
fn bookmarks_are_in_time_order_and_never_share_a_time() {
    let same = ShowInfo {
        bookmarks: vec![bookmark("a", 10), bookmark("b", 10)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&same).as_deref(), Some("bookmarks.1.at_ms"));
    let waits = ShowInfo {
        bookmarks: vec![ShowBookmark {
            pausing: false,
            continue_after_ms: Some(1000),
            ..bookmark("a", 0)
        }],
        ..ShowInfo::new("s")
    };
    assert_eq!(
        field(&waits).as_deref(),
        Some("bookmarks.0.continue_after_ms")
    );
}

#[test]
fn loops_neither_overlap_nor_run_shorter_than_100_ms() {
    let make = |id: &str, start_ms, end_ms| ShowLoop {
        loop_id: id.into(),
        name: String::new(),
        start_ms,
        end_ms,
        preset_id: None,
        leave: LoopLeave::AtOnce,
    };
    let short = ShowInfo {
        loops: vec![make("a", 0, 99)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&short).as_deref(), Some("loops.0.end_ms"));
    let overlap = ShowInfo {
        loops: vec![make("a", 0, 1000), make("b", 999, 2000)],
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&overlap).as_deref(), Some("loops.1.start_ms"));
    let touching = ShowInfo {
        loops: vec![make("a", 0, 1000), make("b", 1000, 2000)],
        ..ShowInfo::new("s")
    };
    assert_eq!(touching.problem(), None);
}

#[test]
fn clips_on_a_track_do_not_overlap_and_their_fades_fit() {
    let overlay = |start_ms, length_ms, fade_in_ms| OverlayClip {
        clip_id: format!("c{start_ms}"),
        scene_id: SceneId::new("scn"),
        start_ms,
        length_ms,
        fade_in_ms,
        fade_out_ms: 0,
    };
    let track = |clips| ShowInfo {
        overlays: vec![OverlayTrack {
            track_id: "v2".into(),
            name: String::new(),
            clips,
        }],
        ..ShowInfo::new("s")
    };
    assert_eq!(
        field(&track(vec![overlay(0, 1000, 0), overlay(500, 1000, 0)])).as_deref(),
        Some("overlays.0.clips.1.start_ms")
    );
    assert_eq!(
        field(&track(vec![overlay(0, 1000, 1001)])).as_deref(),
        Some("overlays.0.clips.0.fade_out_ms")
    );
    assert_eq!(
        track(vec![overlay(0, 1000, 1000), overlay(1000, 5, 0)]).problem(),
        None
    );
}

#[test]
fn a_cue_runs_what_a_timeline_cue_runs_but_never_switches_program() {
    let track = |action: &Action| ShowInfo {
        sequences: vec![SequenceTrack {
            track_id: "s1".into(),
            name: String::new(),
            cues: vec![ShowCue::new("c", 0, action)],
        }],
        ..ShowInfo::new("s")
    };
    let switch = Action::SceneSwitch {
        scene_id: SceneId::new("scn"),
        preset_id: None,
        via: Vec::new(),
    };
    assert_eq!(
        field(&track(&switch)).as_deref(),
        Some("sequences.0.cues.0.action.type")
    );
    assert_eq!(
        field(&track(&Action::ShowContinue)).as_deref(),
        Some("sequences.0.cues.0.action.type")
    );
    let lights = Action::ChannelSwitch {
        channel_id: "power".into(),
        to: ChannelSwitch::On,
    };
    assert_eq!(track(&lights).problem(), None);
    let play = Action::TimelinePlay {
        timeline_id: TimelineId::new("tl"),
        delay_ms: None,
    };
    let show = track(&play);
    assert_eq!(show.problem(), None);
    assert_eq!(
        show.sequences[0].cues[0].plays(),
        Some(TimelineId::new("tl"))
    );
}

#[test]
fn the_end_goes_to_a_bookmark_the_show_holds() {
    let show = ShowInfo {
        end: ShowEnd::GoTo {
            bookmark_id: "gone".into(),
        },
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&show).as_deref(), Some("end.bookmark_id"));
    assert_eq!(
        serde_json::to_value(ShowEnd::GoTo {
            bookmark_id: "b".into()
        })
        .expect("serializes"),
        serde_json::json!({"type": "go_to", "bookmark_id": "b"})
    );
}

#[test]
fn an_end_from_a_newer_server_parses_but_is_refused() {
    let end: ShowEnd = serde_json::from_value(serde_json::json!({"type": "fade_out", "ms": 3000}))
        .expect("parses");
    assert_eq!(end, ShowEnd::Unknown);
    let show = ShowInfo {
        end,
        ..ShowInfo::new("s")
    };
    assert_eq!(field(&show).as_deref(), Some("end.type"));
}

#[test]
fn show_actions_are_checked_as_the_call_would_be() {
    assert_eq!(
        Action::ShowGoToNumber { number: 9 }.bind_problem(),
        Some(("number", "must be within 1..=8"))
    );
    assert_eq!(Action::ShowGoToNumber { number: 1 }.bind_problem(), None);
    assert_eq!(
        Action::ShowSeek {
            at_ms: MAX_SHOW_MS + 1
        }
        .bind_problem()
        .map(|(field, _)| field),
        Some("at_ms")
    );
    assert_eq!(Action::ShowContinue.bind_problem(), None);
    assert!(Action::ShowContinue.cue_problem().is_some());
}

#[test]
fn a_bookmark_holds_up_to_16_kib_of_show_notes() {
    let mut show = ShowInfo::new("s");
    show.program = vec![clip("c", 10_000)];
    let mut notes = bookmark("b", 0);
    notes.note = "# Segment\n\n- greet\n".repeat(1000)[..MAX_SHOW_NOTE_LEN].to_owned();
    show.bookmarks = vec![notes];
    assert_eq!(field(&show), None);
    show.bookmarks[0].note.push('x');
    assert_eq!(field(&show).as_deref(), Some("bookmarks.0.note"));
}
