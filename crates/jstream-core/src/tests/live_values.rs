//! Live values (ADR-0032): `live.set`, `live.cancel`, `live.list`, committing with
//! `ends_gesture`, and the handover from the render parameter channel to the committed
//! snapshot, through `InProcessControl`.

use super::{add, batch, block_on, code, connected, create, drain, edits, query, webcam};
use crate::runtime::{Collector, RenderReader};
use crate::{AuditMemory, Engine};
use jstream_control::{Control, ControlError, EventStream};
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, Crop, Edit, Event, GestureId, GestureInfo,
    HistoryCall, HistoryOp, ItemId, ItemTransform, LiveOutcome, Query, QueryResult, SceneId, Vec2,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn gesture(id: &str) -> GestureId {
    GestureId::new(id)
}

fn moved_to(item: &str, x: f32) -> Edit {
    Edit::ItemSetTransform {
        item_id: ItemId::new(item),
        scene_id: None,
        transform: ItemTransform {
            position: Vec2::new(x, 0.0).expect("finite"),
            ..ItemTransform::IDENTITY
        },
    }
}

fn cropped(item: &str, left: u32) -> Edit {
    Edit::ItemSetCrop {
        item_id: ItemId::new(item),
        scene_id: None,
        crop: Crop { left, ..Crop::NONE },
    }
}

fn live_set(control: &dyn Control, id: &str, values: Vec<Edit>) -> Result<(), ControlError> {
    let action = Action::LiveSet {
        gesture_id: gesture(id),
        values,
    };
    match block_on(control.call(Call::Action { action }))?.result {
        CallResult::Action {} => Ok(()),
        other => panic!("not an action result: {other:?}"),
    }
}

fn live_cancel(control: &dyn Control, id: &str) -> Result<(), ControlError> {
    let action = Action::LiveCancel {
        gesture_id: gesture(id),
    };
    block_on(control.call(Call::Action { action })).map(|_| ())
}

fn live_list(control: &dyn Control) -> Vec<GestureInfo> {
    match query(control, Query::LiveList).expect("live.list") {
        QueryResult::LiveList { gestures } => gestures,
        other => panic!("not a live list: {other:?}"),
    }
}

/// An item's collection `x`, as `item.list` reads it.
fn x_of(control: &dyn Control, item: &str) -> f32 {
    match query(
        control,
        Query::ItemList {
            scene_id: SceneId::new("main"),
        },
    )
    .expect("item.list")
    {
        QueryResult::ItemList { items, .. } => items
            .iter()
            .find(|i| i.item_id.as_str() == item)
            .map(|i| i.transform.position.x.get())
            .expect("the item exists"),
        other => panic!("not an item list: {other:?}"),
    }
}

/// A scene `main` placing the webcam as items `a` and `b`.
fn scene(control: &dyn Control) {
    batch(
        control,
        edits(vec![
            create("main"),
            webcam(),
            add("main", "a", "webcam"),
            add("main", "b", "webcam"),
        ]),
    )
    .expect("scene");
}

/// The engine with a render reader standing in for the render thread, so the
/// handover has a reader to wait on.
fn reading(engine: &Engine) -> RenderReader {
    let mut reader = RenderReader::new(engine.render_hub(), Collector::inline());
    reader.start();
    reader
}

/// Whether the parameter channel holds a live value for `item`.
fn on_channel(engine: &Engine, item: &str) -> bool {
    engine
        .render_hub()
        .load_live()
        .item(&ItemId::new(item))
        .is_some()
}

/// Polls `events` until `done` holds for what arrived, or a few seconds pass.
fn wait_for(events: &mut EventStream, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        seen.extend(drain(events));
        if done(&seen) || Instant::now() > deadline {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn invalid_field(error: &ControlError) -> String {
    match error.protocol() {
        Some(jstream_protocol::ProtocolError::InvalidArgument { field, .. }) => field.clone(),
        other => panic!("not invalid_argument: {other:?}"),
    }
}

#[test]
fn a_live_value_overrides_output_without_touching_the_collection() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);
    let mut events = control.events();
    let rev = engine.rev();

    live_set(&control, "drag", vec![moved_to("a", 40.0), cropped("b", 3)]).expect("live.set");
    assert_eq!(engine.rev(), rev, "a live value is not collection state");
    assert_eq!(x_of(&control, "a"), 0.0, "collection reads never show it");
    assert!(on_channel(&engine, "a") && on_channel(&engine, "b"));
    assert!(!on_channel(&engine, "c"));
    assert_eq!(
        live_list(&control),
        [GestureInfo {
            gesture_id: gesture("drag"),
            client_id: control.client_id().expect("hello"),
            values: vec![moved_to("a", 40.0), cropped("b", 3)],
        }]
    );
    let seen = drain(&mut events);
    assert_eq!(
        seen,
        [Event::LiveChanged {
            gesture_id: gesture("drag"),
            client_id: control.client_id().expect("hello"),
            values: vec![moved_to("a", 40.0), cropped("b", 3)],
        }],
        "the first change goes out at once"
    );

    // A newer value for the same parameter replaces the gesture's previous one.
    live_set(&control, "drag", vec![moved_to("a", 80.0)]).expect("again");
    assert_eq!(
        live_list(&control)[0].values,
        [moved_to("a", 80.0), cropped("b", 3)]
    );
}

#[test]
fn a_commit_is_one_rev_one_undo_step_and_ends_the_gesture() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);
    let before = engine.rev();
    for x in [10.0, 20.0, 30.0] {
        live_set(&control, "drag", vec![moved_to("a", x)]).expect("live.set");
    }
    let mut events = control.events();
    let rev = batch(
        &control,
        Batch::single(moved_to("a", 30.0)).ending_gesture(gesture("drag")),
    )
    .expect("commit");
    assert_eq!(rev.0, before.0 + 1, "the whole drag is one rev");
    assert_eq!(x_of(&control, "a"), 30.0);
    assert!(live_list(&control).is_empty());
    // No reader is adopting snapshots, so the values are released at once.
    assert!(!on_channel(&engine, "a"));

    let client_id = control.client_id().expect("hello");
    let seen = wait_for(&mut events, |seen| {
        seen.iter().any(|e| matches!(e, Event::LiveEnded { .. }))
    });
    // The newest value that had not gone out yet comes first, then the commit's
    // `state.changed`, then `live.ended`.
    let tail: Vec<_> = seen
        .iter()
        .filter(|e| !matches!(e, Event::LiveChanged { .. }))
        .cloned()
        .collect();
    assert!(matches!(
        &tail[..],
        [
            Event::StateChanged { rev: changed, .. },
            Event::LiveEnded {
                outcome: LiveOutcome::Committed,
                rev: Some(ended),
                client_id: owner,
                ..
            },
        ] if *changed == rev && *ended == rev && *owner == client_id
    ));

    // Undo goes back to the value from before the gesture, in one step.
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    assert_eq!(x_of(&control, "a"), 0.0);
}

#[test]
fn committed_values_stay_on_the_channel_until_the_render_thread_catches_up() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);
    // The webcam needs a frame for its items to be drawn, so the commit changes what the
    // render snapshot draws.
    engine
        .set_source_frame(
            &jstream_protocol::SourceId::new("webcam"),
            1,
            1,
            &[1, 2, 3, 255],
        )
        .expect("frame");
    let mut reader = reading(&engine);
    live_set(&control, "drag", vec![moved_to("a", 50.0)]).expect("live.set");
    reader.adopt();
    assert!(reader.held().1.item(&ItemId::new("a")).is_some());

    batch(
        &control,
        Batch::single(moved_to("a", 50.0)).ending_gesture(gesture("drag")),
    )
    .expect("commit");
    // The reader still holds the snapshot from before the commit: the live value must
    // stay, or the next frame would show the old placement.
    std::thread::sleep(Duration::from_millis(30));
    assert!(on_channel(&engine, "a"), "held until the reader catches up");

    reader.adopt();
    let deadline = Instant::now() + Duration::from_secs(5);
    while on_channel(&engine, "a") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !on_channel(&engine, "a"),
        "released once it adopted the commit"
    );
    reader.adopt();
    assert!(reader.held().1.item(&ItemId::new("a")).is_none());
}

#[test]
fn a_failed_commit_leaves_the_gesture_open() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);
    live_set(&control, "drag", vec![moved_to("a", 5.0)]).expect("live.set");
    let rev = engine.rev();

    let stale = Batch::single(moved_to("a", 5.0))
        .expecting(jstream_protocol::Rev(0))
        .ending_gesture(gesture("drag"));
    let error = batch(&control, stale).expect_err("stale");
    assert_eq!(code(&error), Some("rev_conflict"));
    assert_eq!(live_list(&control).len(), 1, "still open");
    assert!(on_channel(&engine, "a"));

    let unknown = Batch::single(moved_to("a", 5.0)).ending_gesture(gesture("nope"));
    let error = batch(&control, unknown).expect_err("not open");
    assert_eq!(invalid_field(&error), "ends_gesture");
    assert_eq!(engine.rev(), rev, "nothing applied");

    // Another client can't commit it either: gestures are per client.
    let other = connected(&engine);
    let theirs = Batch::single(moved_to("a", 5.0)).ending_gesture(gesture("drag"));
    assert_eq!(
        invalid_field(&batch(&other, theirs).expect_err("not theirs")),
        "ends_gesture"
    );
}

#[test]
fn cancel_reverts_and_is_audited_once_per_gesture() {
    let audit = Arc::new(AuditMemory::new());
    let engine = Engine::new().with_audit(audit.clone());
    let control = connected(&engine);
    scene(&control);
    let mut events = control.events();
    live_set(&control, "dialog", vec![cropped("a", 2)]).expect("live.set");
    live_set(
        &control,
        "dialog",
        vec![cropped("a", 4), moved_to("b", 9.0)],
    )
    .expect("again");
    live_cancel(&control, "dialog").expect("cancel");
    assert!(live_list(&control).is_empty());
    assert!(!on_channel(&engine, "a") && !on_channel(&engine, "b"));
    live_cancel(&control, "dialog").expect("a second cancel is a no-op");

    let seen = drain(&mut events);
    assert!(
        matches!(
            seen.last(),
            Some(Event::LiveEnded {
                outcome: LiveOutcome::Cancelled,
                rev: None,
                ..
            })
        ),
        "{seen:?}"
    );
    // The values not sent yet go out before the gesture ends.
    let Some(Event::LiveChanged { values, .. }) = seen.get(seen.len() - 2) else {
        panic!("no last live.changed: {seen:?}");
    };
    assert_eq!(values, &[moved_to("b", 9.0), cropped("a", 4)]);

    let entries = audit.entries();
    let live: Vec<_> = entries
        .iter()
        .filter(|e| e.call.starts_with("live."))
        .collect();
    assert_eq!(live.len(), 1, "one entry per gesture, none per live.set");
    assert_eq!(live[0].call, "live.cancel");
    assert_eq!(live[0].gesture_id, Some(gesture("dialog")));
    assert_eq!(live[0].outcome, Some(LiveOutcome::Cancelled));
    assert_eq!(live[0].edits, ["item.set_transform", "item.set_crop"]);
    assert_eq!(live[0].rev_before, live[0].rev_after);
    assert!(live[0].request_id.is_some());
}

#[test]
fn a_session_that_ends_expires_its_gestures() {
    let audit = Arc::new(AuditMemory::new());
    let engine = Engine::new().with_audit(audit.clone());
    let watcher = connected(&engine);
    scene(&watcher);
    let mut events = watcher.events();
    let agent = connected(&engine);
    let agent_id = agent.client_id().expect("hello");
    live_set(&agent, "drag", vec![moved_to("a", 7.0)]).expect("live.set");
    drop(agent);

    assert!(live_list(&watcher).is_empty());
    assert!(!on_channel(&engine, "a"));
    let seen = drain(&mut events);
    assert_eq!(
        seen.last(),
        Some(&Event::LiveEnded {
            gesture_id: gesture("drag"),
            client_id: agent_id.clone(),
            outcome: LiveOutcome::Expired,
            rev: None,
        })
    );
    let entries = audit.entries();
    let expired = entries.last().expect("an entry");
    assert_eq!(expired.call, "live.ended");
    assert_eq!(expired.client_id, agent_id);
    assert_eq!(expired.outcome, Some(LiveOutcome::Expired));
    assert_eq!(expired.request_id, None, "no request ended it");
}

#[test]
fn values_are_validated_like_a_batch_and_all_or_nothing() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);

    let error = live_set(&control, "g", vec![moved_to("a", 1.0), create("x")]).expect_err("edit");
    assert_eq!(invalid_field(&error), "values.1.type");
    let error = live_set(&control, "g", vec![]).expect_err("empty");
    assert_eq!(invalid_field(&error), "values");
    let error = live_set(&control, "g", vec![moved_to("a", 1.0); 257]).expect_err("too many");
    assert_eq!(invalid_field(&error), "values");
    let error = live_set(
        &control,
        "g",
        vec![moved_to("a", 1.0), moved_to("ghost", 1.0)],
    )
    .expect_err("no such item");
    assert_eq!(code(&error), Some("not_found"));
    batch(&control, jstream_protocol::Batch::single(create("other"))).expect("other");
    let elsewhere = Edit::ItemSetTransform {
        item_id: ItemId::new("a"),
        scene_id: Some(SceneId::new("other")),
        transform: ItemTransform::IDENTITY,
    };
    let error = live_set(&control, "g", vec![moved_to("b", 1.0), elsewhere]).expect_err("scene");
    assert!(
        invalid_field(&error).starts_with("values.1."),
        "named as the value: {error:?}"
    );
    assert!(live_list(&control).is_empty(), "nothing was applied");
    assert!(!on_channel(&engine, "a") && !on_channel(&engine, "b"));

    for n in 0..16 {
        live_set(&control, &format!("g{n}"), vec![moved_to("a", 1.0)]).expect("open");
    }
    let error = live_set(&control, "g16", vec![moved_to("b", 1.0)]).expect_err("17th");
    assert_eq!(invalid_field(&error), "gesture_id");
    live_set(&control, "g3", vec![moved_to("b", 1.0)]).expect("an open one still takes values");
}

#[test]
fn the_last_writer_takes_a_parameter_over() {
    let engine = Engine::new();
    let alice = connected(&engine);
    scene(&alice);
    let bob = connected(&engine);
    live_set(&alice, "drag", vec![moved_to("a", 1.0), cropped("a", 1)]).expect("alice");
    live_set(&bob, "drag", vec![moved_to("a", 2.0)]).expect("bob takes the placement");

    let gestures = live_list(&alice);
    let held = |client: &ClientId| {
        gestures
            .iter()
            .find(|g| &g.client_id == client)
            .map(|g| g.values.clone())
            .expect("open")
    };
    assert_eq!(held(&alice.client_id().expect("hello")), [cropped("a", 1)]);
    assert_eq!(held(&bob.client_id().expect("hello")), [moved_to("a", 2.0)]);

    // A batch on a held parameter changes the collection; the live value still wins on
    // output until the gesture ends.
    batch(&alice, Batch::single(moved_to("a", 3.0))).expect("plain batch");
    assert_eq!(x_of(&alice, "a"), 3.0);
    assert!(on_channel(&engine, "a"));
}

#[test]
fn deleting_an_item_drops_its_live_values() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);
    live_set(
        &control,
        "drag",
        vec![moved_to("a", 1.0), moved_to("b", 1.0)],
    )
    .expect("live.set");
    batch(
        &control,
        Batch::single(Edit::ItemRemove {
            item_id: ItemId::new("a"),
        }),
    )
    .expect("remove a");
    assert!(!on_channel(&engine, "a"));
    assert!(on_channel(&engine, "b"));
    assert_eq!(live_list(&control)[0].values, [moved_to("b", 1.0)]);
}

#[test]
fn live_changed_is_coalesced_and_the_newest_values_follow() {
    let engine = Engine::new();
    let control = connected(&engine);
    scene(&control);
    let mut events = control.events();
    for x in 1..=20 {
        live_set(&control, "drag", vec![moved_to("a", x as f32)]).expect("live.set");
    }
    let seen = wait_for(&mut events, |seen| {
        seen.iter().any(
            |e| matches!(e, Event::LiveChanged { values, .. } if values == &[moved_to("a", 20.0)]),
        )
    });
    let changed: Vec<_> = seen
        .iter()
        .filter(|e| matches!(e, Event::LiveChanged { .. }))
        .collect();
    assert!(
        changed.len() < 20,
        "twenty quick sets are fewer events: {}",
        changed.len()
    );
    assert!(
        matches!(changed.last(), Some(Event::LiveChanged { values, .. }) if values == &[moved_to("a", 20.0)]),
        "the newest value always goes out"
    );
}
