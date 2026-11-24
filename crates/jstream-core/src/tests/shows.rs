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
        Some("a"),
        "b starts on Continue"
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
    assert_eq!(program(&control).as_deref(), Some("a"));

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
