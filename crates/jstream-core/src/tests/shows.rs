//! Shows (ADR-0210) through `InProcessControl`: `show.play` puts the program track on
//! program on the animation clock, a pausing bookmark waits for `show.continue`, jumps
//! land the state playing there would leave, a hand switch takes the show off script,
//! and `studio.get` and `show.changed` report it.

use super::{block_on, code, connected, create, drain, query};
use crate::Engine;
use jstream_control::Control;
use jstream_media::Timebase;
use jstream_platform::fake::FakeClock;
use jstream_protocol::{
    Action, Batch, Call, Edit, Event, ProgramClip, Query, QueryResult, RunningShow, SceneId,
    SequenceTrack, ShowBookmark, ShowChange, ShowCue, ShowId, ShowInfo, ShowState,
};
use serde_json::json;
use std::sync::Arc;

fn apply(control: &dyn Control, edits: Vec<Edit>) {
    let batch = Batch {
        edits,
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    };
    block_on(control.call(Call::Batch(batch))).expect("applies");
}

fn act(control: &dyn Control, action: Action) -> Result<(), &'static str> {
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|error| code(&error).unwrap_or("not a protocol error"))
}

fn step(control: &dyn Control, ms: u32) {
    act(control, Action::ClockStep { ms }).expect("step");
}

fn clip(id: &str, scene: &str, length_ms: u32) -> ProgramClip {
    ProgramClip {
        clip_id: id.into(),
        scene_id: SceneId::new(scene),
        length_ms,
        preset_id: None,
    }
}

fn bookmark(id: &str, at_ms: u32, pausing: bool) -> ShowBookmark {
    ShowBookmark {
        bookmark_id: id.into(),
        name: id.into(),
        at_ms,
        pausing,
        preset_id: None,
        continue_after_ms: None,
        note: String::new(),
    }
}

/// Scenes `a`, `b` and `c`, a show `s` of one second of each, a pausing bookmark
/// `p` at 1000 and a non-pausing `n` at 2000, and a cue setting value `v` at 1500. The
/// clock is held, so tests step it.
fn engine_with_show() -> (Engine, crate::InProcessControl) {
    let engine = Engine::new();
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock));
    let control = connected(&engine);
    let mut show = ShowInfo::new("s");
    show.program = vec![
        clip("ca", "a", 1000),
        clip("cb", "b", 1000),
        clip("cc", "c", 1000),
    ];
    show.bookmarks = vec![bookmark("p", 1000, true), bookmark("n", 2000, false)];
    show.sequences = vec![SequenceTrack {
        track_id: "s1".into(),
        name: String::new(),
        cues: vec![ShowCue::new(
            "v",
            1500,
            &Action::ValuesSet {
                name: "v".into(),
                value: Some(json!(1)),
            },
        )],
    }];
    apply(
        &control,
        vec![
            create("a"),
            create("b"),
            create("c"),
            Edit::ShowSet { show },
        ],
    );
    act(&control, Action::ClockHold).expect("hold");
    (engine, control)
}

fn studio(control: &dyn Control) -> (Option<SceneId>, Option<Box<RunningShow>>) {
    match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet {
            program_scene,
            show,
            ..
        }) => (program_scene, show),
        other => panic!("not studio.get: {other:?}"),
    }
}

fn program(control: &dyn Control) -> Option<String> {
    studio(control).0.map(|id| id.as_str().to_owned())
}

fn running(control: &dyn Control) -> RunningShow {
    *studio(control).1.expect("a show runs")
}

fn value_set(control: &dyn Control) -> bool {
    match query(control, Query::ValuesList { prefix: None }) {
        Ok(QueryResult::ValuesList { values }) => values.iter().any(|v| v.name == "v"),
        other => panic!("not values.list: {other:?}"),
    }
}

fn play() -> Action {
    Action::ShowPlay {
        show_id: ShowId::new("s"),
        bookmark_id: None,
        rehearse: false,
    }
}

#[test]
fn a_show_plays_its_program_track_and_waits_at_a_pausing_bookmark() {
    let (_engine, control) = engine_with_show();
    let mut events = control.events();
    act(&control, play()).expect("play");
    assert_eq!(program(&control).as_deref(), Some("a"));
    let show = running(&control);
    assert_eq!(show.state, ShowState::Playing);
    assert_eq!(show.at_ms, 0);

    step(&control, 600);
    step(&control, 600);
    assert_eq!(
        program(&control).as_deref(),
        Some("b"),
        "the clip the bookmark starts is up while it waits"
    );
    let show = running(&control);
    assert_eq!(show.state, ShowState::Paused);
    assert_eq!(show.at_ms, 1000);
    assert_eq!(show.bookmark_id.as_deref(), Some("p"));

    step(&control, 500);
    assert_eq!(
        running(&control).at_ms,
        1000,
        "show time stops while paused"
    );

    act(&control, Action::ShowContinue).expect("continue");
    step(&control, 1);
    assert_eq!(program(&control).as_deref(), Some("b"));
    step(&control, 600);
    assert!(value_set(&control), "the cue at 1500 ran");
    step(&control, 500);
    assert_eq!(program(&control).as_deref(), Some("c"), "n doesn't pause");
    assert_eq!(running(&control).state, ShowState::Playing);

    step(&control, 1000);
    assert!(studio(&control).1.is_none(), "it ended");
    assert_eq!(
        program(&control).as_deref(),
        Some("c"),
        "the last scene stays"
    );

    let changes: Vec<ShowChange> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::ShowChanged { change, .. } => Some(change),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        [
            ShowChange::Started,
            ShowChange::Paused,
            ShowChange::Continued,
            ShowChange::Ended
        ]
    );
}

#[test]
fn go_to_lands_the_state_playing_there_would_leave() {
    let (_engine, control) = engine_with_show();
    act(&control, play()).expect("play");
    act(
        &control,
        Action::ShowGoTo {
            show_id: ShowId::new("s"),
            bookmark_id: "n".into(),
        },
    )
    .expect("go to");
    assert_eq!(program(&control).as_deref(), Some("c"));
    assert!(value_set(&control), "the state cue before n is applied");
    assert_eq!(running(&control).state, ShowState::Playing);

    act(&control, Action::ShowBack).expect("back");
    let show = running(&control);
    assert_eq!(show.state, ShowState::Paused);
    assert_eq!(show.at_ms, 1000);
    assert_eq!(
        program(&control).as_deref(),
        Some("b"),
        "Back cuts to the bookmark's clip"
    );

    act(&control, Action::ShowGoToNumber { number: 2 }).expect("number 2");
    assert_eq!(running(&control).at_ms, 2000);
}

#[test]
fn continue_while_playing_jumps_past_the_next_pause() {
    let (_engine, control) = engine_with_show();
    act(&control, play()).expect("play");
    step(&control, 100);
    act(&control, Action::ShowContinue).expect("continue");
    step(&control, 1);
    let show = running(&control);
    assert_eq!(show.state, ShowState::Playing);
    assert!(show.at_ms >= 1000);
    assert_eq!(program(&control).as_deref(), Some("b"));
}

#[test]
fn a_hand_switch_takes_the_show_off_script_and_continue_goes_on() {
    let (_engine, control) = engine_with_show();
    act(&control, play()).expect("play");
    step(&control, 200);
    act(
        &control,
        Action::SceneSwitch {
            scene_id: SceneId::new("c"),
            preset_id: None,
            via: Vec::new(),
        },
    )
    .expect("switch");
    let show = running(&control);
    assert_eq!(show.state, ShowState::OffScript);
    assert_eq!(show.off_script.as_ref().map(SceneId::as_str), Some("c"));
    step(&control, 500);
    assert_eq!(running(&control).at_ms, 200, "show time stops off script");
    act(&control, Action::ShowContinue).expect("continue");
    step(&control, 1);
    assert_eq!(
        program(&control).as_deref(),
        Some("b"),
        "on to the next clip"
    );
}

#[test]
fn stop_and_delete_end_the_show_and_bad_calls_are_refused() {
    let (_engine, control) = engine_with_show();
    assert_eq!(
        act(
            &control,
            Action::ShowPlay {
                show_id: ShowId::new("nope"),
                bookmark_id: None,
                rehearse: false,
            }
        ),
        Err("not_found")
    );
    assert_eq!(
        act(
            &control,
            Action::ShowPlay {
                show_id: ShowId::new("s"),
                bookmark_id: Some("nope".into()),
                rehearse: false,
            }
        ),
        Err("invalid_argument")
    );
    // With no show running, the run actions do nothing.
    act(&control, Action::ShowContinue).expect("continue");
    act(&control, Action::ShowStop).expect("stop");

    act(&control, play()).expect("play");
    act(&control, Action::ShowStop).expect("stop");
    assert!(studio(&control).1.is_none());

    act(&control, play()).expect("play");
    apply(
        &control,
        vec![Edit::ShowDelete {
            show_id: ShowId::new("s"),
        }],
    );
    assert!(studio(&control).1.is_none(), "deleting it stops it");
}

#[test]
fn arm_sets_the_spot_the_next_continue_jumps_to() {
    let (_engine, control) = engine_with_show();
    act(&control, play()).expect("play");
    act(&control, Action::ShowArm { at_ms: Some(2500) }).expect("arm");
    assert_eq!(running(&control).armed_ms, Some(2500));
    act(&control, Action::ShowContinue).expect("continue");
    let show = running(&control);
    assert_eq!(show.armed_ms, None);
    assert_eq!(show.at_ms, 2500);
    assert_eq!(program(&control).as_deref(), Some("c"));
}

fn overlays(control: &dyn Control) -> Vec<String> {
    match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { overlays, .. }) => {
            overlays.iter().map(|id| id.as_str().to_owned()).collect()
        }
        other => panic!("not studio.get: {other:?}"),
    }
}

fn scene_ids(control: &dyn Control) -> Vec<String> {
    match query(control, Query::SceneList) {
        Ok(QueryResult::SceneList { scenes, .. }) => scenes
            .iter()
            .map(|scene| scene.scene_id.as_str().to_owned())
            .collect(),
        other => panic!("not scene.list: {other:?}"),
    }
}

#[test]
fn an_overlay_clip_puts_its_scene_over_program_for_its_span() {
    let (_engine, control) = engine_with_show();
    apply(&control, vec![create("logo")]);
    let mut show = match query(&control, Query::StateGet) {
        Ok(QueryResult::StateGet { shows, .. }) => shows[0].clone(),
        other => panic!("not state.get: {other:?}"),
    };
    show.overlays = vec![jstream_protocol::OverlayTrack {
        track_id: "v2".into(),
        name: String::new(),
        clips: vec![jstream_protocol::OverlayClip {
            clip_id: "o".into(),
            scene_id: SceneId::new("logo"),
            start_ms: 200,
            length_ms: 300,
            fade_in_ms: 0,
            fade_out_ms: 0,
        }],
    }];
    apply(&control, vec![Edit::ShowSet { show }]);
    act(&control, play()).expect("play");
    assert!(overlays(&control).is_empty());
    let before = scene_ids(&control);
    assert!(
        !before.iter().any(|id| id.starts_with("show-scene")),
        "the show scene is never listed: {before:?}"
    );
    step(&control, 250);
    assert_eq!(overlays(&control), ["logo"]);
    assert_eq!(
        program(&control).as_deref(),
        Some("a"),
        "program stays under it"
    );
    step(&control, 300);
    assert!(overlays(&control).is_empty(), "down at the clip's end");

    // Deleting the running show takes its overlays down with it.
    act(&control, Action::ShowSeek { at_ms: 250 }).expect("seek");
    assert_eq!(overlays(&control), ["logo"]);
    apply(
        &control,
        vec![Edit::ShowDelete {
            show_id: ShowId::new("s"),
        }],
    );
    assert!(studio(&control).1.is_none(), "stopped");
    assert!(overlays(&control).is_empty(), "and its overlay is down");
}

/// The overlay-removed report: an overlay clip taken out of the running show comes
/// down at once, and one added under the play head goes up.
#[test]
fn an_edit_of_the_running_show_takes_a_removed_overlay_down_and_puts_an_added_one_up() {
    let (_engine, control) = engine_with_show();
    apply(&control, vec![create("logo"), create("bug")]);
    let mut show = match query(&control, Query::StateGet) {
        Ok(QueryResult::StateGet { shows, .. }) => shows[0].clone(),
        other => panic!("not state.get: {other:?}"),
    };
    let overlay = |id: &str, scene: &str| jstream_protocol::OverlayClip {
        clip_id: id.into(),
        scene_id: SceneId::new(scene),
        start_ms: 0,
        length_ms: 3000,
        fade_in_ms: 0,
        fade_out_ms: 0,
    };
    show.overlays = vec![jstream_protocol::OverlayTrack {
        track_id: "v2".into(),
        name: String::new(),
        clips: vec![overlay("ov-logo", "logo")],
    }];
    apply(&control, vec![Edit::ShowSet { show: show.clone() }]);
    act(&control, play()).expect("play");
    step(&control, 100);
    assert_eq!(overlays(&control), ["logo"]);
    show.overlays[0].clips = vec![overlay("ov-bug", "bug")];
    apply(&control, vec![Edit::ShowSet { show: show.clone() }]);
    assert_eq!(overlays(&control), ["bug"], "logo down, bug up");
    show.overlays.clear();
    apply(&control, vec![Edit::ShowSet { show }]);
    assert!(overlays(&control).is_empty(), "down when its track goes");
    assert_eq!(running(&control).state, ShowState::Playing);
}

#[test]
fn overlay_actions_work_with_no_show_running() {
    let (_engine, control) = engine_with_show();
    let logo = || SceneId::new("b");
    act(&control, Action::OverlayHide { scene_id: logo() }).expect("nothing to hide");
    act(&control, Action::OverlayShow { scene_id: logo() }).expect("show");
    assert_eq!(overlays(&control), ["b"]);
    act(&control, Action::OverlayToggle { scene_id: logo() }).expect("toggle");
    assert!(overlays(&control).is_empty());
    act(&control, Action::OverlayToggle { scene_id: logo() }).expect("toggle");
    assert_eq!(overlays(&control), ["b"]);
    assert_eq!(
        act(
            &control,
            Action::OverlayShow {
                scene_id: SceneId::new("nope")
            }
        ),
        Err("not_found")
    );
}

#[test]
fn the_show_scene_can_not_be_deleted_and_hears_its_overlays() {
    let (engine, control) = engine_with_show();
    act(
        &control,
        Action::OverlayShow {
            scene_id: SceneId::new("b"),
        },
    )
    .expect("show");
    let show_scene = engine.show_scene_id().expect("made");
    let result = block_on(control.call(Call::Batch(Batch {
        edits: vec![Edit::SceneDelete {
            scene_id: show_scene.clone(),
        }],
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    })));
    assert_eq!(result.as_ref().err().and_then(code), Some("not_found"));
    assert_eq!(engine.show_scene_id(), Some(show_scene));
}

/// A sequence the show starts has its cues ticked with the show: under `clock.step`, a
/// cue of it the step passes fires before the step returns (#1713 B1).
#[test]
fn a_sequence_the_show_starts_fires_its_cues_on_time() {
    use jstream_protocol::{TimelineCue, TimelineId, TimelineInfo, TimelineLoop};
    let engine = Engine::new();
    engine.set_source_clock(Arc::new(FakeClock::new(Timebase::MILLIS)));
    let control = connected(&engine);
    let mut show = ShowInfo::new("s");
    show.program = vec![clip("ca", "a", 3000)];
    show.sequences = vec![SequenceTrack {
        track_id: "s1".into(),
        name: String::new(),
        cues: vec![ShowCue::new(
            "t",
            100,
            &Action::TimelinePlay {
                delay_ms: None,
                timeline_id: TimelineId::new("t"),
            },
        )],
    }];
    apply(
        &control,
        vec![
            create("a"),
            Edit::TimelineSet {
                scene_id: SceneId::new("a"),
                timeline: TimelineInfo {
                    lanes: Vec::new(),
                    starts: Vec::new(),
                    stops: Vec::new(),
                    labels: Vec::new(),
                    timeline_id: TimelineId::new("t"),
                    name: String::new(),
                    duration_ms: 1000,
                    looping: TimelineLoop::Once,
                    cues: vec![TimelineCue::new(
                        50,
                        &Action::ValuesSet {
                            name: "v".into(),
                            value: Some(json!(1)),
                        },
                    )],
                },
            },
            Edit::ShowSet { show },
        ],
    );
    act(&control, Action::ClockHold).expect("hold");
    act(&control, play()).expect("play");
    // One step over both the sequence's start, at 100, and its cue, at 150.
    step(&control, 160);
    assert!(
        value_set(&control),
        "the sequence's cue at 150 didn't fire by 160"
    );
}

/// A show jump made while the live ticker waits for the show's next point wakes it, so
/// what follows the landing runs on time (#1713 B2). Real time: program `a` 5 s then
/// `b`, a pausing bookmark at 4 s; Continue soon after the start jumps to 4 s, so `b`
/// is due a second later, not when the ticker planned to wake at 4 s.
#[test]
fn a_show_jump_wakes_the_live_ticker() {
    use std::time::{Duration, Instant};
    let engine = Engine::new();
    let control = connected(&engine);
    let mut show = ShowInfo::new("s");
    show.program = vec![clip("ca", "a", 5000), clip("cb", "b", 1000)];
    show.bookmarks = vec![bookmark("p", 4000, true)];
    apply(
        &control,
        vec![create("a"), create("b"), Edit::ShowSet { show }],
    );
    act(&control, play()).expect("play");
    assert_eq!(program(&control).as_deref(), Some("a"));
    std::thread::sleep(Duration::from_millis(100));
    let continued = Instant::now();
    act(&control, Action::ShowContinue).expect("continue");
    let deadline = continued + Duration::from_secs(6);
    while Instant::now() < deadline && program(&control).as_deref() != Some("b") {
        std::thread::sleep(Duration::from_millis(5));
    }
    let elapsed = continued.elapsed();
    assert_eq!(program(&control).as_deref(), Some("b"));
    assert!(
        elapsed < Duration::from_millis(2500),
        "the cut due 1 s after Continue came after {elapsed:?}"
    );
}

#[test]
fn pause_holds_the_play_head_until_continue_or_play() {
    let (_engine, control) = engine_with_show();
    let mut events = control.events();
    act(&control, play()).expect("play");
    step(&control, 400);
    drain(&mut events);
    act(&control, Action::ShowPause).expect("pause");
    let show = running(&control);
    assert_eq!(show.state, ShowState::Paused);
    assert_eq!(show.bookmark_id, None);
    assert_eq!(show.at_ms, 400);
    assert!(
        drain(&mut events).iter().any(|e| matches!(
            e,
            Event::ShowChanged {
                change: ShowChange::Paused,
                ..
            }
        )),
        "show.changed paused"
    );
    // The clock runs on; the show doesn't, past the pausing bookmark at 1000 or not.
    step(&control, 2000);
    assert_eq!(running(&control).at_ms, 400);
    assert_eq!(program(&control).as_deref(), Some("a"));
    // Pausing a paused show does nothing.
    act(&control, Action::ShowPause).expect("pause again");
    act(&control, Action::ShowContinue).expect("continue");
    let show = running(&control);
    assert_eq!(show.state, ShowState::Playing);
    assert_eq!(show.at_ms, 400);
    step(&control, 100);
    assert_eq!(running(&control).at_ms, 500);
    // show.play of the held show plays on rather than starting again.
    act(&control, Action::ShowPause).expect("pause");
    act(&control, play()).expect("play on");
    let show = running(&control);
    assert_eq!(show.state, ShowState::Playing);
    assert_eq!(show.at_ms, 500);
}

/// A keyframe marker moves program itself to the keyframe scene of the scene on
/// program, and a marker with none back to the scene's own values.
#[test]
fn a_keyframe_marker_moves_program_to_its_keyframe_scene() {
    use jstream_protocol::KeyframeMarker;
    let engine = Engine::new();
    engine.set_source_clock(Arc::new(FakeClock::new(Timebase::MILLIS)));
    let control = connected(&engine);
    let mut show = ShowInfo::new("s");
    show.program = vec![clip("ca", "a", 3000)];
    show.keyframes = vec![
        KeyframeMarker {
            marker_id: "k1".into(),
            at_ms: 500,
            keyframe_id: Some(SceneId::new("a_k1")),
            preset_id: None,
        },
        KeyframeMarker {
            marker_id: "base".into(),
            at_ms: 1500,
            keyframe_id: None,
            preset_id: None,
        },
    ];
    apply(
        &control,
        vec![
            create("a"),
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("a_k1"),
                variant_of: SceneId::new("a"),
                name: "K1".into(),
            },
            Edit::ShowSet { show },
        ],
    );
    act(&control, Action::ClockHold).expect("hold");
    act(&control, play()).expect("play");
    assert_eq!(program(&control).as_deref(), Some("a"));
    step(&control, 600);
    assert_eq!(program(&control).as_deref(), Some("a_k1"));
    assert_eq!(running(&control).off_script, None, "the show's own move");
    step(&control, 1000);
    assert_eq!(program(&control).as_deref(), Some("a"));
}

/// Editing the running show carries it over: a clip changed under the play head cuts
/// to its new scene, with show time where it was.
#[test]
fn an_edit_of_the_running_show_takes_effect_at_the_play_head() {
    let (_engine, control) = engine_with_show();
    act(&control, play()).expect("play");
    step(&control, 300);
    assert_eq!(program(&control).as_deref(), Some("a"));
    let mut show = ShowInfo::new("s");
    show.program = vec![
        clip("ca", "c", 1000),
        clip("cb", "b", 1000),
        clip("cc", "c", 1000),
    ];
    apply(&control, vec![Edit::ShowSet { show }]);
    assert_eq!(program(&control).as_deref(), Some("c"));
    let show = running(&control);
    assert_eq!(show.state, ShowState::Playing);
    assert_eq!(show.at_ms, 300);
    step(&control, 800);
    assert_eq!(
        program(&control).as_deref(),
        Some("b"),
        "plays on in the new plan"
    );
}

fn preview(control: &dyn Control) -> Option<String> {
    match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { preview_scene, .. }) => {
            preview_scene.map(|id| id.as_str().to_owned())
        }
        other => panic!("not studio.get: {other:?}"),
    }
}

/// The program clip after the one under the play head is the upcoming scene, kept warm
/// with its media held, so the cut to it starts nothing (ADR-0210 decision 9).
#[test]
fn the_next_program_clip_is_the_upcoming_scene() {
    let (engine, control) = engine_with_show();
    assert_eq!(engine.upcoming(), None);
    act(&control, play()).expect("play");
    assert_eq!(engine.upcoming().as_deref(), Some("b"));
    step(&control, 1100);
    assert_eq!(
        program(&control).as_deref(),
        Some("b"),
        "waits at p with b up"
    );
    assert_eq!(engine.upcoming().as_deref(), Some("c"));
    act(&control, Action::ShowContinue).expect("continue");
    step(&control, 1100);
    assert_eq!(program(&control).as_deref(), Some("c"));
    assert_eq!(engine.upcoming(), None, "nothing after the last clip");
    act(&control, Action::ShowSeek { at_ms: 0 }).expect("seek");
    assert_eq!(engine.upcoming().as_deref(), Some("b"));
    act(&control, Action::ShowStop).expect("stop");
    assert_eq!(engine.upcoming(), None, "no show, nothing upcoming");
}

/// A rehearsal runs the show on preview: its clips go to preview, program stays where
/// it is, a hand switch doesn't take it off script, and its cues don't run (ADR-0210
/// decision 13).
#[test]
fn a_rehearsal_runs_the_show_on_preview_and_leaves_program_alone() {
    let (engine, control) = engine_with_show();
    act(
        &control,
        Action::SceneSwitch {
            scene_id: SceneId::new("c"),
            preset_id: None,
            via: Vec::new(),
        },
    )
    .expect("c on program");
    act(
        &control,
        Action::ShowPlay {
            show_id: ShowId::new("s"),
            bookmark_id: None,
            rehearse: true,
        },
    )
    .expect("rehearse");
    assert_eq!(preview(&control).as_deref(), Some("a"));
    assert_eq!(program(&control).as_deref(), Some("c"));
    assert!(running(&control).rehearse);
    assert_eq!(engine.upcoming(), None, "a rehearsal warms nothing");
    act(
        &control,
        Action::SceneSwitch {
            scene_id: SceneId::new("a"),
            preset_id: None,
            via: Vec::new(),
        },
    )
    .expect("a hand switch");
    assert_eq!(
        running(&control).state,
        ShowState::Playing,
        "still on script"
    );
    step(&control, 1100);
    assert_eq!(preview(&control).as_deref(), Some("b"));
    assert_eq!(running(&control).state, ShowState::Paused);
    act(&control, Action::ShowContinue).expect("continue");
    step(&control, 600);
    assert!(!value_set(&control), "its cues don't run");
    assert_eq!(
        program(&control).as_deref(),
        Some("a"),
        "program as the hand left it"
    );
}

/// The fader of `source` as the engine hears it.
fn heard(engine: &Engine, source: &str) -> f32 {
    let id = jstream_protocol::SourceId::new(source);
    engine.drawn(|state| {
        state
            .source(&id)
            .and_then(|s| s.audio())
            .map_or(f32::NAN, |a| a.gain_db.get())
    })
}

/// An audio clip of a webcam's microphone, which is heard only while shown: before
/// its clip the show holds its fader at the floor; the clip shows it over program
/// (heard), its fader rising over the fade in to the clip's level and falling over
/// the fade out; after it, the floor again; a hand on the fader takes over; and when
/// the show stops the source goes back to its own fader.
#[test]
fn an_audio_clip_brings_its_source_up_with_its_fades() {
    let (engine, control) = engine_with_show();
    apply(&control, vec![super::webcam()]);
    let mut show = match query(&control, Query::StateGet) {
        Ok(QueryResult::StateGet { shows, .. }) => shows[0].clone(),
        other => panic!("not state.get: {other:?}"),
    };
    // No pausing bookmark, so the show plays through.
    show.bookmarks.clear();
    show.audio = vec![jstream_protocol::AudioTrack {
        track_id: "a1".into(),
        name: String::new(),
        clips: vec![jstream_protocol::AudioClip {
            clip_id: "mic".into(),
            source_id: jstream_protocol::SourceId::new("webcam"),
            start_ms: 500,
            length_ms: 1000,
            fade_in_ms: 200,
            fade_out_ms: 200,
            gain_db: jstream_protocol::Finite32::new(-6.0).expect("finite"),
        }],
    }];
    apply(&control, vec![Edit::ShowSet { show }]);
    let own = heard(&engine, "webcam");
    assert!(own.is_finite(), "a webcam has a fader");
    let shown = |engine: &Engine| {
        engine.drawn(|state| {
            let program = Some(SceneId::new("a"));
            crate::activity::activity_gains(state, program.as_ref())
                .get(&jstream_protocol::SourceId::new("webcam"))
                .copied()
                .unwrap_or(0.0)
        })
    };
    act(&control, play()).expect("play");
    step(&control, 100);
    assert_eq!(heard(&engine, "webcam"), -100.0, "down before its clip");
    assert_eq!(shown(&engine), 0.0, "not shown before its clip");
    step(&control, 500);
    let rising = heard(&engine, "webcam");
    assert!(rising > -100.0 && rising < -6.0, "rising: {rising}");
    assert_eq!(shown(&engine), 1.0, "its clip shows it over program");
    step(&control, 400);
    assert_eq!(heard(&engine, "webcam"), -6.0, "at the clip's level");
    step(&control, 350);
    let falling = heard(&engine, "webcam");
    assert!(falling > -100.0 && falling < -6.0, "falling: {falling}");
    step(&control, 200);
    assert_eq!(heard(&engine, "webcam"), -100.0, "down after its clip");
    assert_eq!(shown(&engine), 0.0, "hidden after its clip");
    // A hand on the fader takes it over.
    apply(
        &control,
        vec![Edit::SourceSetAudio {
            source_id: jstream_protocol::SourceId::new("webcam"),
            enabled: None,
            follow: None,
            gain_db: jstream_protocol::Finite32::new(-3.0),
            muted: None,
        }],
    );
    step(&control, 50);
    assert_eq!(heard(&engine, "webcam"), -3.0, "the hand's level");
    act(&control, Action::ShowStop).expect("stop");
    assert_eq!(
        heard(&engine, "webcam"),
        -3.0,
        "its own fader after the show"
    );
}

/// Cut off by a jump, a clip's source fades down over about 0.3 s, then goes.
#[test]
fn a_jump_fades_a_cut_off_audio_clip_down() {
    let (engine, control) = engine_with_show();
    apply(&control, vec![super::webcam()]);
    let mut show = match query(&control, Query::StateGet) {
        Ok(QueryResult::StateGet { shows, .. }) => shows[0].clone(),
        other => panic!("not state.get: {other:?}"),
    };
    show.bookmarks.clear();
    let clip =
        |id: &str, track: &str, start_ms: u32, length_ms: u32| jstream_protocol::AudioTrack {
            track_id: track.into(),
            name: String::new(),
            clips: vec![jstream_protocol::AudioClip {
                clip_id: id.into(),
                source_id: jstream_protocol::SourceId::new("webcam"),
                start_ms,
                length_ms,
                fade_in_ms: 0,
                fade_out_ms: 0,
                gain_db: jstream_protocol::Finite32::new(0.0).expect("finite"),
            }],
        };
    show.audio = vec![clip("early", "a1", 0, 1000)];
    apply(&control, vec![Edit::ShowSet { show }]);
    act(&control, play()).expect("play");
    step(&control, 200);
    assert_eq!(heard(&engine, "webcam"), 0.0, "up");
    act(&control, Action::ShowSeek { at_ms: 2000 }).expect("seek");
    step(&control, 150);
    let fading = heard(&engine, "webcam");
    assert!(fading > -100.0 && fading < 0.0, "fading: {fading}");
    step(&control, 200);
    assert_eq!(
        heard(&engine, "webcam"),
        -100.0,
        "down once the fade is done"
    );
    // Stopped, the show hands the fader back to its own level over the short fade.
    act(&control, Action::ShowStop).expect("stop");
    step(&control, 150);
    let back = heard(&engine, "webcam");
    assert!(back > -100.0 && back < 0.0, "going back: {back}");
    step(&control, 200);
    assert_eq!(heard(&engine, "webcam"), 0.0, "its own fader");
}

/// Two clips of one source on two tracks: when the first ends, the second keeps the
/// source up (heard), and its fader at the second clip's level (#1763 B1).
#[test]
fn a_clip_ending_leaves_its_source_up_while_another_track_holds_it() {
    let (engine, control) = engine_with_show();
    apply(&control, vec![super::webcam()]);
    let mut show = match query(&control, Query::StateGet) {
        Ok(QueryResult::StateGet { shows, .. }) => shows[0].clone(),
        other => panic!("not state.get: {other:?}"),
    };
    show.bookmarks.clear();
    let track =
        |track: &str, start_ms: u32, length_ms: u32, gain_db: f32| jstream_protocol::AudioTrack {
            track_id: track.into(),
            name: String::new(),
            clips: vec![jstream_protocol::AudioClip {
                clip_id: format!("{track}-clip"),
                source_id: jstream_protocol::SourceId::new("webcam"),
                start_ms,
                length_ms,
                fade_in_ms: 0,
                fade_out_ms: 0,
                gain_db: jstream_protocol::Finite32::new(gain_db).expect("finite"),
            }],
        };
    show.audio = vec![track("a1", 0, 500, 0.0), track("a2", 200, 1000, -10.0)];
    apply(&control, vec![Edit::ShowSet { show }]);
    let shown = |engine: &Engine| {
        engine.drawn(|state| {
            crate::activity::activity_gains(state, Some(&SceneId::new("a")))
                .get(&jstream_protocol::SourceId::new("webcam"))
                .copied()
                .unwrap_or(0.0)
        })
    };
    act(&control, play()).expect("play");
    step(&control, 300);
    assert_eq!(shown(&engine), 1.0, "up");
    step(&control, 400);
    assert_eq!(shown(&engine), 1.0, "still up on the second track");
    assert_eq!(
        heard(&engine, "webcam"),
        -10.0,
        "at the second clip's level"
    );
}

/// At a loop's last clip the wrap runs back to the clip at the loop's start, so that
/// clip is the upcoming scene, not the one after the loop.
#[test]
fn at_a_loops_last_clip_its_first_clip_is_upcoming() {
    let (engine, control) = engine_with_show();
    let mut show = ShowInfo::new("s");
    show.program = vec![
        clip("ca", "a", 1000),
        clip("cb", "b", 1000),
        clip("cc", "c", 1000),
    ];
    show.loops = vec![jstream_protocol::ShowLoop {
        loop_id: "l".into(),
        name: String::new(),
        start_ms: 0,
        end_ms: 2000,
        preset_id: None,
        leave: jstream_protocol::LoopLeave::default(),
    }];
    apply(&control, vec![Edit::ShowSet { show }]);
    act(&control, play()).expect("play");
    step(&control, 100);
    assert_eq!(engine.upcoming().as_deref(), Some("b"));
    step(&control, 1000);
    assert_eq!(program(&control).as_deref(), Some("b"));
    assert_eq!(
        engine.upcoming().as_deref(),
        Some("a"),
        "the loop runs back to a"
    );
}
