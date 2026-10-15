//! Per-client undo and redo (ADR-0032): `history.undo` and `history.redo` through
//! `InProcessControl`.

use super::{
    add, batch, block_on, code, connected, create, drain, edits, output_call, query, rtmp_output,
    scene_ids, switch_call, webcam,
};
use crate::Engine;
use jstream_control::{Control, ControlError};
use jstream_protocol::{
    Batch, Call, CallResult, ClientId, Edit, Event, HistoryCall, HistoryDirection, HistoryOp,
    IdempotencyKey, ItemId, ItemTransform, OutputId, ProtocolError, Query, QueryResult, Rev,
    SceneId, Vec2,
};

/// The answer to a history call: `(rev, step_rev, undo_depth, redo_depth)`.
fn history(control: &dyn Control, call: HistoryCall) -> Result<(Rev, Rev, u32, u32), ControlError> {
    match block_on(control.call(Call::History(call)))?.result {
        CallResult::History {
            rev,
            step_rev,
            undo_depth,
            redo_depth,
        } => Ok((rev, step_rev, undo_depth, redo_depth)),
        other => panic!("not a history result: {other:?}"),
    }
}

fn undo(control: &dyn Control) -> Result<(Rev, Rev, u32, u32), ControlError> {
    history(control, HistoryCall::new(HistoryOp::Undo))
}

fn redo(control: &dyn Control) -> Result<(Rev, Rev, u32, u32), ControlError> {
    history(control, HistoryCall::new(HistoryOp::Redo))
}

fn one(edit: Edit) -> Batch {
    Batch::single(edit)
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

fn names(control: &dyn Control) -> Vec<String> {
    scene_ids(control).1
}

#[test]
fn undo_and_redo_move_one_step_each_way() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let one_rev = batch(&control, one(create("a"))).expect("a");
    batch(&control, edits(vec![create("b"), create("c")])).expect("b and c");
    drain(&mut events);

    // Undoing the second batch removes both of its scenes, as one rev.
    assert_eq!(undo(&control).expect("undo"), (Rev(3), Rev(2), 1, 1));
    assert_eq!(names(&control), ["a"]);
    let client_id = control.client_id().expect("hello");
    match drain(&mut events).as_slice() {
        [
            Event::StateChanged {
                rev: Rev(3),
                client_id: by,
                request_id: Some(_),
                cause: Some(HistoryDirection::Undo),
            },
        ] => assert_eq!(by, &client_id),
        other => panic!("expected one state.changed caused by undo: {other:?}"),
    }

    assert_eq!(undo(&control).expect("undo"), (Rev(4), one_rev, 0, 2));
    assert!(names(&control).is_empty());
    let err = undo(&control).expect_err("nothing left");
    assert_eq!(
        err.protocol(),
        Some(&ProtocolError::HistoryEmpty {
            direction: HistoryDirection::Undo
        })
    );

    assert_eq!(redo(&control).expect("redo"), (Rev(5), one_rev, 1, 1));
    assert_eq!(redo(&control).expect("redo"), (Rev(6), Rev(2), 2, 0));
    assert_eq!(names(&control), ["a", "b", "c"]);
    // Removing and restoring the only scene also moves program (ADR-0030).
    assert!(matches!(
        drain(&mut events).as_slice(),
        [
            Event::StateChanged {
                cause: Some(HistoryDirection::Undo),
                ..
            },
            Event::StudioChanged {
                program_scene: None,
                ..
            },
            Event::StateChanged {
                cause: Some(HistoryDirection::Redo),
                ..
            },
            Event::StudioChanged {
                program_scene: Some(_),
                ..
            },
            Event::StateChanged {
                cause: Some(HistoryDirection::Redo),
                ..
            },
        ]
    ));
    let err = redo(&control).expect_err("nothing to redo");
    assert_eq!(
        err.protocol(),
        Some(&ProtocolError::HistoryEmpty {
            direction: HistoryDirection::Redo
        })
    );
}

fn program(control: &dyn Control) -> Option<SceneId> {
    match query(control, Query::StudioGet).expect("studio.get") {
        QueryResult::StudioGet { program_scene } => program_scene,
        other => panic!("not a studio result: {other:?}"),
    }
}

/// Undo and redo keep program valid like a batch does, and never put a restored scene
/// back on program (ADR-0030).
#[test]
fn program_follows_undo_and_redo() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    batch(&control, edits(vec![create("a"), create("b")])).expect("a and b");
    block_on(control.call(switch_call("b"))).expect("switch");
    batch(
        &control,
        one(Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        }),
    )
    .expect("delete");
    assert_eq!(program(&control), Some(SceneId::new("a")));
    drain(&mut events);

    // Undoing the delete brings b back without putting it on program.
    undo(&control).expect("undo the delete");
    assert_eq!(names(&control), ["a", "b"]);
    assert_eq!(program(&control), Some(SceneId::new("a")));
    assert!(matches!(
        drain(&mut events).as_slice(),
        [Event::StateChanged {
            cause: Some(HistoryDirection::Undo),
            ..
        }]
    ));

    // Undoing the creating batch empties program, after the undo's own `state.changed`.
    let undone = block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo))))
        .expect("undo the creation");
    assert_eq!(program(&control), None);
    let client_id = control.client_id().expect("hello");
    assert_eq!(
        drain(&mut events).as_slice(),
        [
            Event::StateChanged {
                rev: Rev(4),
                client_id: client_id.clone(),
                request_id: Some(undone.id.clone()),
                cause: Some(HistoryDirection::Undo),
            },
            Event::StudioChanged {
                program_scene: None,
                client_id: client_id.clone(),
                request_id: Some(undone.id),
            },
        ]
    );

    // Redoing it puts the first scene on program again.
    let redone = block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Redo))))
        .expect("redo the creation");
    assert_eq!(program(&control), Some(SceneId::new("a")));
    assert_eq!(
        drain(&mut events)[1..],
        [Event::StudioChanged {
            program_scene: Some(SceneId::new("a")),
            client_id,
            request_id: Some(redone.id),
        }]
    );
}

#[test]
fn a_new_batch_clears_what_could_be_redone() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, one(create("a"))).expect("a");
    undo(&control).expect("undo");
    batch(&control, one(create("b"))).expect("b");
    assert_eq!(
        code(&redo(&control).expect_err("cleared")),
        Some("history_empty")
    );
    assert_eq!(
        undo(&control).expect("undo b").2,
        0,
        "only b's step is left"
    );
}

#[test]
fn each_client_undoes_only_its_own_steps() {
    let engine = Engine::new();
    let user = connected(&engine);
    let agent = connected(&engine);
    batch(&user, one(create("mine"))).expect("user");
    batch(&agent, one(create("theirs"))).expect("agent");

    undo(&user).expect("the user's undo");
    assert_eq!(names(&user), ["theirs"], "the agent's scene stays");
    // The agent's history is untouched by the user's undo.
    undo(&agent).expect("the agent's undo");
    assert!(names(&agent).is_empty());
    assert_eq!(
        code(&undo(&agent).expect_err("empty")),
        Some("history_empty")
    );
}

#[test]
fn an_undo_never_overwrites_another_clients_later_change() {
    let engine = Engine::new();
    let user = connected(&engine);
    let agent = connected(&engine);
    batch(
        &user,
        edits(vec![create("main"), webcam(), add("main", "cam", "webcam")]),
    )
    .expect("setup");
    batch(&user, one(moved_to("cam", 10.0))).expect("user moves");
    let theirs = batch(&agent, one(moved_to("cam", 20.0))).expect("agent moves");

    let err = undo(&user).expect_err("the agent moved it since");
    let agent_id = agent.client_id().expect("hello");
    assert_eq!(
        err.protocol(),
        Some(&ProtocolError::UndoConflict {
            step_rev: Rev(2),
            client_id: agent_id,
            rev: theirs,
        })
    );
    assert_eq!(x_of(&user, "cam"), 20.0, "nothing was applied");
    assert_eq!(engine.rev(), theirs);

    // The conflicting step was dropped: the next undo reaches the one before it, which
    // the agent's later change also touched (it created the item the agent moved).
    let err = undo(&user).expect_err("the setup created what the agent moved");
    assert_eq!(code(&err), Some("undo_conflict"));
    assert_eq!(
        code(&undo(&user).expect_err("empty")),
        Some("history_empty")
    );
}

/// Undoing a `scene.create` deletes the items the scene holds now. When the step that
/// added an item was dropped for a conflict, another client's later change to that
/// item must still block the undo (review B1 on #176).
#[test]
fn undoing_a_scene_never_deletes_another_clients_work_on_its_items() {
    let engine = Engine::new();
    let user = connected(&engine);
    let agent = connected(&engine);
    batch(&user, one(webcam())).expect("source");
    batch(&user, one(create("main"))).expect("scene");
    batch(&user, one(add("main", "cam", "webcam"))).expect("item");
    let theirs = batch(&agent, one(moved_to("cam", 20.0))).expect("agent moves it");

    assert_eq!(
        code(&undo(&user).expect_err("the agent moved the item")),
        Some("undo_conflict")
    );
    let err = undo(&user).expect_err("deleting the scene would delete the agent's item");
    assert_eq!(
        err.protocol(),
        Some(&ProtocolError::UndoConflict {
            step_rev: Rev(2),
            client_id: agent.client_id().expect("hello"),
            rev: theirs,
        })
    );
    assert_eq!(x_of(&user, "cam"), 20.0, "nothing was applied");
    assert_eq!(engine.rev(), theirs);
}

#[test]
fn another_clients_unrelated_change_does_not_block_an_undo() {
    let engine = Engine::new();
    let user = connected(&engine);
    let agent = connected(&engine);
    batch(
        &user,
        edits(vec![
            create("main"),
            webcam(),
            add("main", "cam", "webcam"),
            add("main", "cam2", "webcam"),
        ]),
    )
    .expect("setup");
    batch(&user, one(moved_to("cam", 10.0))).expect("user moves cam");
    batch(&agent, one(moved_to("cam2", 30.0))).expect("agent moves cam2");
    batch(
        &agent,
        one(Edit::ItemSetVisible {
            item_id: ItemId::new("cam"),
            scene_id: None,
            visible: false,
        }),
    )
    .expect("agent hides cam");
    batch(&agent, one(create("other"))).expect("agent adds a scene");

    undo(&user).expect("only the transform of cam is the user's step");
    assert_eq!(x_of(&user, "cam"), 0.0);
    assert_eq!(x_of(&user, "cam2"), 30.0);
    redo(&user).expect("redo");
    assert_eq!(x_of(&user, "cam"), 10.0);
}

#[test]
fn a_redo_checks_changes_made_since_the_undo() {
    let engine = Engine::new();
    let user = connected(&engine);
    let agent = connected(&engine);
    batch(&user, one(create("main"))).expect("user");
    undo(&user).expect("undo");
    batch(&agent, one(create("main"))).expect("the agent reuses the id");
    let err = redo(&user).expect_err("the agent created it since");
    assert_eq!(code(&err), Some("undo_conflict"));
    assert_eq!(
        code(&redo(&user).expect_err("dropped")),
        Some("history_empty")
    );
}

#[test]
fn expected_rev_is_checked_first_and_keeps_the_step() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, one(create("a"))).expect("a");
    let err = history(
        &control,
        HistoryCall::new(HistoryOp::Undo).expecting(Rev(0)),
    )
    .expect_err("stale");
    assert_eq!(
        err.protocol(),
        Some(&ProtocolError::RevConflict {
            expected: Rev(0),
            current: Rev(1)
        })
    );
    history(
        &control,
        HistoryCall::new(HistoryOp::Undo).expecting(Rev(1)),
    )
    .expect("current");
    assert!(names(&control).is_empty());
}

#[test]
fn a_retried_undo_does_not_undo_a_second_step() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, one(create("a"))).expect("a");
    batch(&control, one(create("b"))).expect("b");
    let call = HistoryCall::new(HistoryOp::Undo).with_idempotency_key(IdempotencyKey::new("u"));
    let first = history(&control, call.clone()).expect("undo");
    assert_eq!(history(&control, call).expect("retry"), first);
    assert_eq!(names(&control), ["a"]);
    assert_eq!(engine.rev(), Rev(3));
}

#[test]
fn history_keeps_the_newest_steps_up_to_its_limit() {
    let engine = Engine::new().with_history_limit(2);
    let control = connected(&engine);
    for scene in ["a", "b", "c"] {
        batch(&control, one(create(scene))).expect("create");
    }
    undo(&control).expect("c");
    assert_eq!(undo(&control).expect("b").2, 0);
    assert_eq!(
        code(&undo(&control).expect_err("a was dropped")),
        Some("history_empty")
    );
    assert_eq!(names(&control), ["a"]);
}

#[test]
fn a_running_output_blocks_its_undo_but_keeps_the_step() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        one(Edit::OutputCreate {
            output_id: OutputId::new("yt"),
            kind: rtmp_output("rtmp://a.example/live"),
        }),
    )
    .expect("create");
    block_on(control.call(output_call("yt", true))).expect("start");
    let err = undo(&control).expect_err("it runs");
    assert_eq!(code(&err), Some("invalid_argument"));
    block_on(control.call(output_call("yt", false))).expect("stop");
    undo(&control).expect("stopped, so it can be deleted");
}

#[test]
fn a_session_that_ends_takes_its_history_with_it() {
    let engine = Engine::new();
    let control = connected(&engine);
    let client: ClientId = control.client_id().expect("hello");
    batch(&control, one(create("a"))).expect("a");
    assert_eq!(engine.history_depths(&client), (1, 0));
    drop(control);
    assert_eq!(engine.history_depths(&client), (0, 0));
}

#[test]
fn undo_and_redo_are_audited() {
    let audit = std::sync::Arc::new(crate::AuditMemory::new());
    let engine = Engine::new().with_audit(audit.clone());
    let control = connected(&engine);
    batch(&control, one(create("a"))).expect("a");
    let mut keyed = HistoryCall::new(HistoryOp::Undo);
    keyed.idempotency_key = Some(IdempotencyKey::new("u-1"));
    history(&control, keyed.clone()).expect("undo");
    history(&control, keyed).expect("retry");
    redo(&control).expect("redo");
    let error = redo(&control).expect_err("nothing to redo");
    assert_eq!(code(&error), Some("history_empty"));

    let summary: Vec<_> = audit
        .entries()
        .iter()
        .map(|e| {
            (
                e.call,
                e.edits.len(),
                (e.rev_before.0, e.rev_after.0),
                e.error,
                e.replayed,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("batch", 1, (0, 1), None, false),
            ("history.undo", 0, (1, 2), None, false),
            ("history.undo", 0, (2, 2), None, true),
            ("history.redo", 0, (2, 3), None, false),
            ("history.redo", 0, (3, 3), Some("history_empty"), false),
        ]
    );
}

/// Folders, labels and metadata (ADR-0024 §3 and §4) go through the engine like any
/// edit: `state.get` reports them, and undo takes them back one step at a time.
#[test]
fn folders_and_labels_are_undoable_collection_state() {
    use jstream_protocol::{FolderId, LabelId, SceneMeta};
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, one(create("main"))).expect("main");
    batch(
        &control,
        edits(vec![
            Edit::FolderCreate {
                folder_id: FolderId::new("shows"),
                name: "Shows".into(),
                parent: None,
            },
            Edit::SceneSetFolder {
                scene_id: SceneId::new("main"),
                folder_id: Some(FolderId::new("shows")),
            },
            Edit::LabelCreate {
                label_id: LabelId::new("sponsor"),
                name: "Sponsor".into(),
                color: "#ff8800".into(),
                icon: None,
            },
        ]),
    )
    .expect("organise");
    batch(
        &control,
        one(Edit::SceneSetMeta {
            scene_id: SceneId::new("main"),
            meta: SceneMeta {
                favourite: true,
                ..SceneMeta::default()
            },
        }),
    )
    .expect("favourite");
    let read = |control: &dyn Control| match query(control, Query::StateGet).expect("state") {
        QueryResult::StateGet {
            scenes,
            folders,
            labels,
            ..
        } => (
            scenes[0].folder.clone(),
            scenes[0].meta.favourite,
            folders.len(),
            labels.len(),
        ),
        other => panic!("not a state: {other:?}"),
    };
    assert_eq!(read(&control), (Some(FolderId::new("shows")), true, 1, 1));
    undo(&control).expect("undo the favourite");
    assert_eq!(read(&control), (Some(FolderId::new("shows")), false, 1, 1));
    undo(&control).expect("undo the folder and label");
    assert_eq!(read(&control), (None, false, 0, 0));
    redo(&control).expect("redo");
    assert_eq!(read(&control), (Some(FolderId::new("shows")), false, 1, 1));
}
