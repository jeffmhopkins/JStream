//! Timelines (ADR-0114) through `InProcessControl`: `timeline.set` stores timed cues in
//! a scene, `timeline.play` fires them on the animation clock as the actions they hold,
//! `timeline.seek` runs only the state cues before where it starts, and deleting a
//! timeline stops it and takes the hotkeys bound to it.

use super::{add, block_on, code, connected, create, drain, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Batch, Call, CallResult, Chord, Edit, Event, HistoryCall, HistoryOp, ItemId, Query,
    QueryResult, SceneId, Settings, SourceId, SourceKind, TimelineChange, TimelineCue, TimelineId,
    TimelineInfo, TimelineLoop,
};
use serde_json::json;
use std::time::{Duration, Instant};

fn colour(id: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: Settings(
            [
                ("color", json!("#ff0000ff")),
                ("width", json!(100)),
                ("height", json!(100)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        ),
        duplicated_from: None,
    }
}

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

fn cue(at_ms: u32, action: Action) -> TimelineCue {
    TimelineCue { at_ms, action }
}

fn switch(scene: &str) -> Action {
    Action::SceneSwitch {
        scene_id: SceneId::new(scene),
        preset_id: None,
    }
}

fn show(item: &str) -> Action {
    Action::ItemShow {
        item_id: ItemId::new(item),
        scene_id: None,
    }
}

fn value(name: &str, v: i64) -> Action {
    Action::ValuesSet {
        name: name.into(),
        value: Some(json!(v)),
    }
}

fn timeline(id: &str, duration_ms: u32, looping: TimelineLoop, cues: Vec<TimelineCue>) -> Edit {
    Edit::TimelineSet {
        scene_id: SceneId::new("a"),
        timeline: TimelineInfo {
            timeline_id: TimelineId::new(id),
            name: String::new(),
            duration_ms,
            looping,
            cues,
        },
    }
}

fn play(id: &str) -> Action {
    Action::TimelinePlay {
        timeline_id: TimelineId::new(id),
    }
}

/// Scenes `a` and `b`, with colour item `i` in `a`, hidden.
fn engine_with_scenes() -> (Engine, crate::InProcessControl) {
    let engine = Engine::new();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            create("a"),
            create("b"),
            colour("red"),
            add("a", "i", "red"),
            Edit::ItemSetVisible {
                item_id: ItemId::new("i"),
                scene_id: None,
                visible: false,
            },
        ],
    );
    (engine, control)
}

fn program(control: &dyn Control) -> Option<SceneId> {
    match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { program_scene, .. }) => program_scene,
        other => panic!("not studio.get: {other:?}"),
    }
}

fn visible(control: &dyn Control, item: &str) -> bool {
    let Ok(QueryResult::StateGet { scenes, .. }) = query(control, Query::StateGet) else {
        panic!("not state.get");
    };
    scenes
        .iter()
        .flat_map(|scene| &scene.items)
        .find(|i| i.item_id.as_str() == item)
        .is_some_and(|i| i.visible)
}

/// Polls `done` until it holds, for up to five seconds.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

fn changes(events: &[Event]) -> Vec<TimelineChange> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::TimelineChanged { change, .. } => Some(*change),
            _ => None,
        })
        .collect()
}

fn fired(events: &[Event]) -> Vec<(u32, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::TimelineCueFired { cue, error, .. } => Some((*cue, error.is_none())),
            _ => None,
        })
        .collect()
}

/// A cue at 0 runs before `timeline.play` returns; a later one runs on the animation
/// clock from the live ticker; a `once` timeline then ends. Nothing reaches the undo
/// history.
#[test]
fn a_timeline_runs_its_cues_in_order_and_ends() {
    let (engine, control) = engine_with_scenes();
    let mut events = control.events();
    apply(
        &control,
        vec![timeline(
            "t",
            60,
            TimelineLoop::Once,
            vec![cue(0, switch("b")), cue(40, show("i"))],
        )],
    );
    drain(&mut events);
    assert_eq!(act(&control, play("t")), Ok(()));
    assert_eq!(program(&control), Some(SceneId::new("b")));
    assert!(eventually(|| visible(&control, "i")));
    let mut seen = Vec::new();
    assert!(eventually(|| {
        seen.extend(drain(&mut events));
        changes(&seen).contains(&TimelineChange::Ended)
    }));
    assert_eq!(
        changes(&seen),
        [TimelineChange::Started, TimelineChange::Ended]
    );
    assert_eq!(fired(&seen), [(0, true), (1, true)]);
    // The cues were actions, so undo reaches the batch that wrote the timeline.
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    let Ok(QueryResult::StateGet { scenes, .. }) = query(&control, Query::StateGet) else {
        panic!("not state.get");
    };
    assert!(scenes.iter().all(|scene| scene.timelines.is_empty()));
    drop(engine);
}

/// `timeline.seek` runs, at once, the last cue before the seek for each state it sets,
/// skips events such as a toggle, and plays on from there.
#[test]
fn a_seek_runs_only_the_last_state_cue_for_each_target() {
    let (_engine, control) = engine_with_scenes();
    let mut events = control.events();
    apply(
        &control,
        vec![timeline(
            "t",
            10_000,
            TimelineLoop::Once,
            vec![
                cue(0, switch("b")),
                cue(100, switch("a")),
                cue(200, value("count", 1)),
                cue(300, value("count", 2)),
                cue(
                    400,
                    Action::ItemToggle {
                        item_id: ItemId::new("i"),
                        scene_id: None,
                    },
                ),
                cue(9_000, show("i")),
            ],
        )],
    );
    act(&control, switch("b")).expect("switch");
    drain(&mut events);
    let seek = Action::TimelineSeek {
        timeline_id: TimelineId::new("t"),
        at_ms: 500,
    };
    assert_eq!(act(&control, seek), Ok(()));
    act(
        &control,
        Action::TimelineStop {
            timeline_id: TimelineId::new("t"),
        },
    )
    .expect("stop");
    let seen = drain(&mut events);
    assert_eq!(fired(&seen), [(1, true), (3, true)]);
    assert_eq!(
        changes(&seen),
        [TimelineChange::Sought, TimelineChange::Stopped]
    );
    assert_eq!(program(&control), Some(SceneId::new("a")));
    assert!(!visible(&control, "i"), "the toggle is an event, not run");
    let Ok(QueryResult::ValuesList { values }) =
        query(&control, Query::ValuesList { prefix: None })
    else {
        panic!("not values.list");
    };
    assert_eq!(values.first().map(|v| &v.value), Some(&json!(2)));
}

/// A looping timeline starts each pass again, reports it, and plays until stopped;
/// `studio.get` lists it while it plays.
#[test]
fn a_loop_repeats_until_stopped() {
    let (_engine, control) = engine_with_scenes();
    let mut events = control.events();
    apply(
        &control,
        vec![timeline(
            "t",
            30,
            TimelineLoop::Loop,
            vec![cue(10, value("beat", 1))],
        )],
    );
    act(&control, play("t")).expect("play");
    let mut seen = Vec::new();
    assert!(eventually(|| {
        seen.extend(drain(&mut events));
        changes(&seen)
            .iter()
            .filter(|c| **c == TimelineChange::Looped)
            .count()
            >= 2
    }));
    let Ok(QueryResult::StudioGet { timelines, .. }) = query(&control, Query::StudioGet) else {
        panic!("not studio.get");
    };
    assert_eq!(
        timelines
            .iter()
            .map(|t| t.timeline_id.as_str())
            .collect::<Vec<_>>(),
        ["t"]
    );
    act(
        &control,
        Action::TimelineStop {
            timeline_id: TimelineId::new("t"),
        },
    )
    .expect("stop");
    seen.extend(drain(&mut events));
    assert_eq!(changes(&seen).last(), Some(&TimelineChange::Stopped));
    assert!(fired(&seen).len() >= 2, "{seen:?}");
    let Ok(QueryResult::StudioGet { timelines, .. }) = query(&control, Query::StudioGet) else {
        panic!("not studio.get");
    };
    assert!(timelines.is_empty());
}

/// Deleting a playing timeline stops it, and takes the hotkeys bound to it, which undo
/// brings back with it.
#[test]
fn deleting_a_timeline_stops_it_and_takes_its_hotkeys() {
    let (_engine, control) = engine_with_scenes();
    let mut events = control.events();
    apply(
        &control,
        vec![
            timeline("t", 10_000, TimelineLoop::Once, vec![cue(9_000, show("i"))]),
            Edit::HotkeySet {
                chord: Chord::new("F5"),
                action: play("t"),
            },
        ],
    );
    act(&control, play("t")).expect("play");
    drain(&mut events);
    let deleted = block_on(
        control.call(Call::Batch(Batch::single(Edit::TimelineDelete {
            timeline_id: TimelineId::new("t"),
        }))),
    )
    .expect("deletes");
    let CallResult::Batch {
        removed_hotkeys, ..
    } = deleted.result
    else {
        panic!("not a batch result");
    };
    assert_eq!(removed_hotkeys.len(), 1);
    assert_eq!(changes(&drain(&mut events)), [TimelineChange::Removed]);
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    let Ok(QueryResult::StateGet {
        scenes, hotkeys, ..
    }) = query(&control, Query::StateGet)
    else {
        panic!("not state.get");
    };
    assert_eq!(hotkeys.len(), 1);
    assert_eq!(scenes[0].timelines.len(), 1);
    // It is not playing again: what plays is studio state, which undo doesn't touch.
    let Ok(QueryResult::StudioGet { timelines, .. }) = query(&control, Query::StudioGet) else {
        panic!("not studio.get");
    };
    assert!(timelines.is_empty());
}

/// A cue whose item was deleted stays, inert: it fires with the error the call would
/// get, and the timeline plays on.
#[test]
fn a_cue_naming_a_deleted_item_fails_and_the_timeline_goes_on() {
    let (_engine, control) = engine_with_scenes();
    let mut events = control.events();
    apply(
        &control,
        vec![
            timeline(
                "t",
                0,
                TimelineLoop::Once,
                vec![cue(0, show("i")), cue(0, value("after", 1))],
            ),
            Edit::ItemRemove {
                item_id: ItemId::new("i"),
            },
        ],
    );
    drain(&mut events);
    act(&control, play("t")).expect("play");
    let seen = drain(&mut events);
    assert_eq!(fired(&seen), [(0, false), (1, true)]);
    assert!(seen.iter().any(|event| matches!(
        event,
        Event::TimelineCueFired {
            error: Some(jstream_protocol::ProtocolError::NotFound { .. }),
            ..
        }
    )));
}

/// What `timeline.set` and the actions refuse.
#[test]
fn timelines_are_checked() {
    let (_engine, control) = engine_with_scenes();
    assert_eq!(act(&control, play("nope")), Err("not_found"));
    assert_eq!(
        act(
            &control,
            Action::SourceRestart {
                source_id: SourceId::new("nope")
            }
        ),
        Err("not_found")
    );
    assert_eq!(
        act(
            &control,
            Action::SourceRestart {
                source_id: SourceId::new("red")
            }
        ),
        Ok(())
    );
    let refused = |edit: Edit| {
        block_on(control.call(Call::Batch(Batch::single(edit))))
            .err()
            .and_then(|error| code(&error))
    };
    let output = cue(
        0,
        Action::OutputStart {
            output_id: jstream_protocol::OutputId::new("o"),
        },
    );
    assert_eq!(
        refused(timeline("t", 100, TimelineLoop::Once, vec![output])),
        Some("invalid_argument")
    );
    assert_eq!(
        refused(timeline(
            "t",
            100,
            TimelineLoop::Once,
            vec![cue(50, show("i")), cue(10, show("i"))]
        )),
        Some("invalid_argument")
    );
    apply(
        &control,
        vec![timeline("t", 100, TimelineLoop::Loop, vec![])],
    );
    // The same ID in another scene.
    let mut other = timeline("t", 100, TimelineLoop::Once, vec![]);
    if let Edit::TimelineSet { scene_id, .. } = &mut other {
        *scene_id = SceneId::new("b");
    }
    assert_eq!(refused(other), Some("already_exists"));
    let past = Action::TimelineSeek {
        timeline_id: TimelineId::new("t"),
        at_ms: 100,
    };
    assert_eq!(act(&control, past), Err("invalid_argument"));
}
