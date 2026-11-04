//! Timelines (ADR-0115): `timeline.set` and `timeline.delete`, their checks and
//! inverses, the global ID, a deleted scene's timelines, and the hotkeys bound to one.

use super::{add, apply_err, apply_ok, fixture, invalid_reason};
use crate::Graph;
use jstream_protocol::{
    Action, Chord, Edit, EntityKind, ItemId, MAX_TIMELINES, ProtocolError, SceneId, SourceId,
    TimelineCue, TimelineId, TimelineInfo, TimelineLoop,
};

fn info(id: &str, duration_ms: u32) -> TimelineInfo {
    TimelineInfo {
        timeline_id: TimelineId::new(id),
        name: String::new(),
        duration_ms,
        looping: TimelineLoop::Once,
        cues: vec![TimelineCue::new(
            0,
            &Action::ItemShow {
                item_id: ItemId::new("gone"),
                scene_id: None,
            },
        )],
    }
}

fn set(scene: &str, timeline: TimelineInfo) -> Edit {
    Edit::TimelineSet {
        scene_id: SceneId::new(scene),
        timeline,
    }
}

fn delete(id: &str) -> Edit {
    Edit::TimelineDelete {
        timeline_id: TimelineId::new(id),
    }
}

fn held(graph: &Graph, id: &str) -> Option<(String, TimelineInfo)> {
    graph
        .state()
        .timeline(&TimelineId::new(id))
        .map(|(scene, t)| (scene.id().as_str().to_owned(), t.clone()))
}

fn bind(chord: &str, id: &str) -> Edit {
    Edit::HotkeySet {
        chord: Chord::new(chord),
        action: Action::TimelinePlay {
            timeline_id: TimelineId::new(id),
        },
    }
}

#[test]
fn a_timeline_is_set_changed_and_deleted_with_exact_inverses() {
    let mut graph = fixture();
    // A cue may name an item that doesn't exist: it stays inert.
    let applied = apply_ok(&mut graph, vec![set("a", info("t", 1000))]);
    assert_eq!(applied.inverse.edits, vec![delete("t")]);
    assert_eq!(held(&graph, "t"), Some(("a".into(), info("t", 1000))));
    let read = graph.state().scene(&SceneId::new("a")).expect("a");
    assert_eq!(read.timelines().len(), 1);

    let applied = apply_ok(&mut graph, vec![set("a", info("t", 2000))]);
    assert_eq!(applied.inverse.edits, vec![set("a", info("t", 1000))]);

    let applied = apply_ok(&mut graph, vec![delete("t")]);
    assert_eq!(applied.inverse.edits, vec![set("a", info("t", 2000))]);
    assert_eq!(held(&graph, "t"), None);
}

#[test]
fn an_id_is_unique_across_the_collection() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![set("a", info("t", 1000))]);
    assert_eq!(
        apply_err(&mut graph, vec![set("b", info("t", 1000))]),
        ProtocolError::AlreadyExists {
            entity: EntityKind::Timeline,
            id: "t".into(),
        }
    );
    assert_eq!(
        apply_err(&mut graph, vec![delete("nope")]),
        ProtocolError::NotFound {
            entity: EntityKind::Timeline,
            id: "nope".into(),
        }
    );
}

#[test]
fn a_timeline_is_checked() {
    let mut graph = fixture();
    let mut bad = info("t", 1000);
    bad.cues[0].at_ms = 1001;
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", bad)]));
    assert_eq!(field, "edits.0.timeline.cues.0.at_ms");
    let mut bad = info("t", 1000);
    bad.cues[0] = TimelineCue::new(
        0,
        &Action::OutputStop {
            output_id: jstream_protocol::OutputId::new("o"),
        },
    );
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", bad)]));
    assert_eq!(field, "edits.0.timeline.cues.0.action.type");

    let full: Vec<Edit> = (0..MAX_TIMELINES)
        .map(|n| set("a", info(&format!("t{n}"), 1000)))
        .collect();
    apply_ok(&mut graph, full);
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", info("more", 1000))]));
    assert_eq!(field, "edits.0.timeline");
    // Changing one already there is not a 33rd.
    apply_ok(&mut graph, vec![set("a", info("t0", 2000))]);
}

#[test]
fn deleting_a_scene_takes_its_timelines_and_undo_restores_them() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![set("b", info("t1", 1000)), set("b", info("t2", 500))],
    );
    // apply_ok checks the inverse puts both back exactly.
    apply_ok(
        &mut graph,
        vec![
            Edit::SourceDelete {
                source_id: SourceId::new("sb"),
            },
            Edit::SceneDelete {
                scene_id: SceneId::new("b"),
            },
        ],
    );
    assert_eq!(held(&graph, "t1"), None);
    assert_eq!(held(&graph, "t2"), None);
}

#[test]
fn deleting_a_timeline_takes_its_hotkeys_and_undo_restores_them() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![set("a", info("t", 1000)), bind("F6", "t")]);
    // A binding naming a timeline that doesn't exist is refused.
    assert!(matches!(
        apply_err(&mut graph, vec![bind("F7", "nope")]),
        ProtocolError::NotFound { .. }
    ));
    let applied = apply_ok(&mut graph, vec![delete("t")]);
    assert_eq!(applied.removed_hotkeys.len(), 1);
    assert_eq!(graph.state().hotkeys().count(), 0);
}

#[test]
fn removing_a_cues_item_leaves_the_timeline_as_it_is() {
    let mut graph = fixture();
    let mut shows = info("t", 1000);
    shows.cues[0] = TimelineCue::new(
        0,
        &Action::ItemShow {
            item_id: ItemId::new("i"),
            scene_id: None,
        },
    );
    apply_ok(
        &mut graph,
        vec![add("a", "i", "cam"), set("a", shows.clone())],
    );
    apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i"),
        }],
    );
    assert_eq!(held(&graph, "t"), Some(("a".into(), shows)));
}
