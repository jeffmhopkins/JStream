//! Unit tests for edit application, validation and inverses, and the undo property test.

use crate::Graph;
use jstream_protocol::{
    Crop, Edit, EntityKind, ItemId, ItemTransform, ProtocolError, Rev, SceneId, SourceId,
    SourceKind, Vec2,
};

fn scene_create(id: &str) -> Edit {
    Edit::SceneCreate {
        scene_id: SceneId::new(id),
        name: format!("Scene {id}"),
    }
}

fn builtin(id: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: format!("Source {id}"),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
    }
}

fn scene_source(id: &str, scene: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: format!("Nested {scene}"),
        kind: SourceKind::Scene {
            scene_id: SceneId::new(scene),
        },
    }
}

fn add(scene: &str, item: &str, source: &str) -> Edit {
    Edit::ItemAdd {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        transform: None,
    }
}

/// Applies `edits` and checks the result, then that the inverse restores the state.
fn apply_ok(graph: &mut Graph, edits: Vec<Edit>) -> crate::Applied {
    let before = graph.clone();
    let applied = graph.apply(&edits, Some(before.rev())).expect("applies");
    assert_eq!(applied.rev, Rev(before.rev().0 + 1));
    assert_eq!(graph.rev(), applied.rev);
    let mut undone = graph.clone();
    undone
        .apply(&applied.inverse.edits, Some(applied.rev))
        .expect("inverse applies");
    assert_eq!(undone.state(), before.state(), "inverse of {edits:?}");
    applied
}

/// Applies `edits`, expects `error`, and checks nothing changed.
fn apply_err(graph: &mut Graph, edits: Vec<Edit>) -> ProtocolError {
    let before = graph.clone();
    let error = graph.apply(&edits, None).expect_err("is rejected");
    assert_eq!(graph, &before, "a failed batch changed the graph");
    error
}

fn invalid_reason(error: ProtocolError) -> (String, String) {
    match error {
        ProtocolError::InvalidArgument { field, reason } => (field, reason),
        other => panic!("expected invalid_argument, got {other:?}"),
    }
}

/// Scenes a, b, c; builtin sources cam and mic; scene sources for each scene.
fn fixture() -> Graph {
    let mut graph = Graph::new();
    apply_ok(
        &mut graph,
        vec![
            scene_create("a"),
            scene_create("b"),
            scene_create("c"),
            builtin("cam"),
            builtin("mic"),
            scene_source("sa", "a"),
            scene_source("sb", "b"),
            scene_source("sc", "c"),
        ],
    );
    graph
}

#[test]
fn starts_empty_at_rev_zero() {
    let graph = Graph::new();
    assert_eq!(graph.rev(), Rev(0));
    assert!(graph.state().scenes().is_empty());
    assert_eq!(graph.state().sources().len(), 0);
}

#[test]
fn a_batch_bumps_rev_by_one() {
    let mut graph = fixture();
    assert_eq!(graph.rev(), Rev(1));
    let applied = apply_ok(
        &mut graph,
        vec![add("a", "i1", "cam"), add("a", "i2", "cam")],
    );
    assert_eq!(applied.rev, Rev(2));
}

#[test]
fn stale_expected_rev_is_rejected() {
    let mut graph = fixture();
    let error = graph.apply(&[scene_create("d")], Some(Rev(0)));
    assert_eq!(
        error,
        Err(ProtocolError::RevConflict {
            expected: Rev(0),
            current: Rev(1)
        })
    );
    assert!(graph.state().scene(&SceneId::new("d")).is_none());
    assert_eq!(graph.rev(), Rev(1));
}

#[test]
fn empty_batch_is_rejected() {
    let mut graph = fixture();
    assert_eq!(apply_err(&mut graph, vec![]), ProtocolError::EmptyBatch);
}

#[test]
fn a_failing_edit_rolls_back_the_whole_batch() {
    let mut graph = fixture();
    let error = apply_err(
        &mut graph,
        vec![
            scene_create("d"),
            add("d", "i1", "cam"),
            add("zz", "i2", "cam"),
        ],
    );
    assert_eq!(
        error,
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "zz".into()
        }
    );
}

#[test]
fn later_edits_see_earlier_ones() {
    let mut graph = Graph::new();
    apply_ok(
        &mut graph,
        vec![scene_create("s"), builtin("cam"), add("s", "i", "cam")],
    );
    assert_eq!(graph.state().scenes()[0].items().len(), 1);
}

#[test]
fn duplicate_ids_are_rejected() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "i1", "cam")]);
    let cases = [
        (scene_create("a"), EntityKind::Scene, "a"),
        (builtin("cam"), EntityKind::Source, "cam"),
        (add("b", "i1", "mic"), EntityKind::Item, "i1"),
        (
            Edit::SourceDuplicate {
                source_id: SourceId::new("cam"),
                new_source_id: SourceId::new("mic"),
                name: "x".into(),
            },
            EntityKind::Source,
            "mic",
        ),
    ];
    for (edit, entity, id) in cases {
        assert_eq!(
            apply_err(&mut graph, vec![edit]),
            ProtocolError::AlreadyExists {
                entity,
                id: id.into()
            }
        );
    }
}

#[test]
fn missing_objects_are_not_found() {
    let mut graph = fixture();
    let cases = [
        (
            Edit::SceneRename {
                scene_id: SceneId::new("x"),
                name: "n".into(),
            },
            EntityKind::Scene,
        ),
        (add("a", "i", "x"), EntityKind::Source),
        (
            Edit::ItemRemove {
                item_id: ItemId::new("x"),
            },
            EntityKind::Item,
        ),
        (
            Edit::SourceDelete {
                source_id: SourceId::new("x"),
            },
            EntityKind::Source,
        ),
        (scene_source("s", "x"), EntityKind::Scene),
    ];
    for (edit, entity) in cases {
        assert_eq!(
            apply_err(&mut graph, vec![edit]),
            ProtocolError::NotFound {
                entity,
                id: "x".into()
            }
        );
    }
}

#[test]
fn invalid_arguments_name_the_field() {
    let mut graph = fixture();
    let cases = [
        (
            Edit::SceneCreate {
                scene_id: SceneId::new(""),
                name: "n".into(),
            },
            "edits.0.scene_id",
        ),
        (
            Edit::SceneRename {
                scene_id: SceneId::new("a"),
                name: "  ".into(),
            },
            "edits.0.name",
        ),
        (add("a", "", "cam"), "edits.0.item_id"),
        (
            Edit::SceneReorder {
                scene_id: SceneId::new("a"),
                index: 3,
            },
            "edits.0.index",
        ),
    ];
    for (edit, field) in cases {
        assert_eq!(invalid_reason(apply_err(&mut graph, vec![edit])).0, field);
    }
}

#[test]
fn one_source_is_shared_by_every_placement() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "i1", "cam"),
            add("a", "i2", "cam"),
            add("b", "i3", "cam"),
        ],
    );
    let cam = SourceId::new("cam");
    assert_eq!(graph.state().placements(&cam).count(), 3);
    // Removing a placement keeps the source.
    apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    assert!(graph.state().source(&cam).is_some());
    assert_eq!(graph.state().placements(&cam).count(), 2);
}

#[test]
fn duplicate_makes_a_separate_source() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![Edit::SourceDuplicate {
            source_id: SourceId::new("cam"),
            new_source_id: SourceId::new("cam2"),
            name: "Camera copy".into(),
        }],
    );
    let state = graph.state();
    let copy = state.source(&SourceId::new("cam2")).expect("copied");
    assert_eq!(copy.name(), "Camera copy");
    assert_eq!(
        Some(copy.kind()),
        state.source(&SourceId::new("cam")).map(|s| s.kind())
    );
}

#[test]
fn a_placed_source_cannot_be_deleted() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "i1", "cam")]);
    let delete = Edit::SourceDelete {
        source_id: SourceId::new("cam"),
    };
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![delete.clone()]));
    assert_eq!(field, "edits.0.source_id");
    assert!(reason.contains("i1"), "{reason}");
    // Removing the placement first, in the same batch, works and undoes exactly.
    apply_ok(
        &mut graph,
        vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            delete,
        ],
    );
}

#[test]
fn a_scene_shown_by_a_scene_source_cannot_be_deleted() {
    let mut graph = fixture();
    let delete = Edit::SceneDelete {
        scene_id: SceneId::new("b"),
    };
    let (_, reason) = invalid_reason(apply_err(&mut graph, vec![delete.clone()]));
    assert!(reason.contains("sb"), "{reason}");
    apply_ok(
        &mut graph,
        vec![
            Edit::SourceDelete {
                source_id: SourceId::new("sb"),
            },
            delete,
        ],
    );
}

#[test]
fn a_scene_cannot_contain_itself() {
    let mut graph = fixture();
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![add("a", "i", "sa")]));
    assert_eq!(field, "edits.0.source_id");
    assert!(reason.ends_with("a -> a"), "{reason}");
}

#[test]
fn two_cycles_are_rejected() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "ab", "sb")]);
    let (_, reason) = invalid_reason(apply_err(&mut graph, vec![add("b", "ba", "sa")]));
    assert!(reason.ends_with("b -> a -> b"), "{reason}");
}

#[test]
fn three_cycles_are_rejected_even_within_one_batch() {
    let mut graph = fixture();
    let (field, reason) = invalid_reason(apply_err(
        &mut graph,
        vec![
            add("a", "ab", "sb"),
            add("b", "bc", "sc"),
            add("c", "ca", "sa"),
        ],
    ));
    assert_eq!(field, "edits.2.source_id");
    assert!(reason.ends_with("c -> a -> b -> c"), "{reason}");
}

#[test]
fn legal_nesting_is_accepted() {
    let mut graph = fixture();
    // A diamond: a shows b and c, b shows c, and c is placed twice in a.
    apply_ok(
        &mut graph,
        vec![
            add("a", "ab", "sb"),
            add("a", "ac", "sc"),
            add("b", "bc", "sc"),
            add("a", "ac2", "sc"),
            add("c", "cam", "cam"),
        ],
    );
    assert_eq!(
        graph
            .state()
            .scene(&SceneId::new("a"))
            .map(|s| s.items().len()),
        Some(3)
    );
}

fn ids(graph: &Graph, scene: &str) -> Vec<String> {
    graph
        .state()
        .scene(&SceneId::new(scene))
        .map(|s| s.items().iter().map(|i| i.id().to_string()).collect())
        .unwrap_or_default()
}

#[test]
fn reorder_moves_one_and_keeps_the_rest_in_order() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "i0", "cam"),
            add("a", "i1", "cam"),
            add("a", "i2", "cam"),
        ],
    );
    apply_ok(
        &mut graph,
        vec![Edit::ItemReorder {
            item_id: ItemId::new("i2"),
            index: 0,
        }],
    );
    assert_eq!(ids(&graph, "a"), ["i2", "i0", "i1"]);
    apply_ok(
        &mut graph,
        vec![Edit::SceneReorder {
            scene_id: SceneId::new("c"),
            index: 0,
        }],
    );
    let order: Vec<_> = graph
        .state()
        .scenes()
        .iter()
        .map(|s| s.id().to_string())
        .collect();
    assert_eq!(order, ["c", "a", "b"]);
}

#[test]
fn removing_a_middle_hidden_cropped_item_undoes_exactly() {
    let mut graph = fixture();
    let t = ItemTransform {
        position: Vec2::new(10.0, 20.0).expect("finite"),
        ..ItemTransform::IDENTITY
    };
    apply_ok(
        &mut graph,
        vec![
            add("a", "i0", "cam"),
            add("a", "i1", "mic"),
            add("a", "i2", "cam"),
            Edit::ItemSetTransform {
                item_id: ItemId::new("i1"),
                transform: t,
            },
            Edit::ItemSetVisible {
                item_id: ItemId::new("i1"),
                visible: false,
            },
            Edit::ItemSetCrop {
                item_id: ItemId::new("i1"),
                crop: Crop {
                    left: 4,
                    ..Crop::NONE
                },
            },
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    let names: Vec<_> = applied.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(
        names,
        [
            "item.add",
            "item.reorder",
            "item.set_crop",
            "item.set_visible"
        ]
    );
}

#[test]
fn deleting_a_middle_scene_undoes_exactly() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("b", "i0", "cam"),
            add("b", "i1", "mic"),
            Edit::ItemSetVisible {
                item_id: ItemId::new("i0"),
                visible: false,
            },
            Edit::SourceDelete {
                source_id: SourceId::new("sb"),
            },
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        }],
    );
    let names: Vec<_> = applied.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(
        names,
        [
            "scene.create",
            "scene.reorder",
            "item.add",
            "item.set_visible",
            "item.add"
        ]
    );
    // The sources the scene's items placed are still there.
    assert!(graph.state().source(&SourceId::new("cam")).is_some());
}

#[test]
fn inverse_of_a_batch_runs_in_reverse_order() {
    let mut graph = fixture();
    let applied = apply_ok(
        &mut graph,
        vec![
            add("a", "i", "cam"),
            Edit::SceneRename {
                scene_id: SceneId::new("a"),
                name: "Renamed".into(),
            },
        ],
    );
    let names: Vec<_> = applied.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(names, ["scene.rename", "item.remove"]);
    assert_eq!(applied.inverse.expected_rev, None);
}

mod property {
    //! Apply a random batch, then its inverse: the state must be what it was. Then apply
    //! the inverse's inverse (redo): the state must be what the batch produced.
    //!
    //! A hand-rolled, seeded generator keeps the test deterministic and dependency-free.

    use super::{add, builtin, scene_create, scene_source};
    use crate::Graph;
    use jstream_protocol::{Crop, Edit, ItemId, ItemTransform, Rev, SceneId, SourceId, Vec2};
    use std::collections::BTreeSet;

    /// xorshift64*: small, fast, and good enough to pick edits.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % (n.max(1) as u64)).expect("fits")
        }

        fn pick<T: Clone>(&mut self, from: &[T]) -> Option<T> {
            (!from.is_empty()).then(|| from[self.below(from.len())].clone())
        }
    }

    /// A random edit, mostly valid against `graph`'s state. Small ID pools make
    /// collisions, missing IDs and cycles common too.
    fn random_edit(rng: &mut Rng, graph: &Graph) -> Edit {
        let state = graph.state();
        let scenes: Vec<String> = state.scenes().iter().map(|s| s.id().to_string()).collect();
        let items: Vec<String> = state
            .scenes()
            .iter()
            .flat_map(|s| s.items().iter().map(|i| i.id().to_string()))
            .collect();
        let sources: Vec<String> = state.sources().map(|s| s.id().to_string()).collect();
        let pool = |rng: &mut Rng, prefix: &str| format!("{prefix}{}", rng.below(8));
        let scene = |rng: &mut Rng| rng.pick(&scenes).unwrap_or_else(|| pool(rng, "s"));
        let item = |rng: &mut Rng| rng.pick(&items).unwrap_or_else(|| pool(rng, "i"));
        let source = |rng: &mut Rng| rng.pick(&sources).unwrap_or_else(|| pool(rng, "src"));
        let index = |rng: &mut Rng| u32::try_from(rng.below(6)).expect("fits");
        let f = |rng: &mut Rng| f32::from(u16::try_from(rng.below(2000)).expect("fits")) - 1000.0;
        match rng.below(14) {
            0 => scene_create(&pool(rng, "s")),
            1 => Edit::SceneRename {
                scene_id: SceneId::new(scene(rng)),
                name: pool(rng, "name "),
            },
            2 => Edit::SceneDelete {
                scene_id: SceneId::new(scene(rng)),
            },
            3 => Edit::SceneReorder {
                scene_id: SceneId::new(scene(rng)),
                index: index(rng),
            },
            4 | 5 => add(&scene(rng), &pool(rng, "i"), &source(rng)),
            6 => Edit::ItemRemove {
                item_id: ItemId::new(item(rng)),
            },
            7 => Edit::ItemReorder {
                item_id: ItemId::new(item(rng)),
                index: index(rng),
            },
            8 => Edit::ItemSetTransform {
                item_id: ItemId::new(item(rng)),
                transform: ItemTransform {
                    position: Vec2::new(f(rng), f(rng)).expect("finite"),
                    ..ItemTransform::IDENTITY
                },
            },
            9 => Edit::ItemSetVisible {
                item_id: ItemId::new(item(rng)),
                visible: rng.below(2) == 0,
            },
            10 => Edit::ItemSetCrop {
                item_id: ItemId::new(item(rng)),
                crop: Crop {
                    left: u32::try_from(rng.below(3)).expect("fits"),
                    bottom: u32::try_from(rng.below(3)).expect("fits"),
                    ..Crop::NONE
                },
            },
            11 => {
                if rng.below(2) == 0 {
                    builtin(&pool(rng, "src"))
                } else {
                    scene_source(&pool(rng, "src"), &scene(rng))
                }
            }
            12 => Edit::SourceDuplicate {
                source_id: SourceId::new(source(rng)),
                new_source_id: SourceId::new(pool(rng, "src")),
                name: "copy".into(),
            },
            _ => Edit::SourceDelete {
                source_id: SourceId::new(source(rng)),
            },
        }
    }

    /// A batch of 1–5 edits, built against a scratch copy so later edits see earlier ones.
    /// About one batch in eight gets a random edit that may well be invalid.
    fn random_batch(rng: &mut Rng, graph: &Graph) -> Vec<Edit> {
        let mut scratch = graph.clone();
        let mut edits = Vec::new();
        for _ in 0..=rng.below(5) {
            let edit = random_edit(rng, &scratch);
            let ok = scratch.apply(std::slice::from_ref(&edit), None).is_ok();
            if ok || rng.below(8) == 0 {
                edits.push(edit);
            }
        }
        edits
    }

    #[test]
    fn inverse_restores_the_state_exactly() {
        let mut applied_names = BTreeSet::new();
        let mut successes = 0;
        let mut failures = 0;
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut graph = Graph::new();
            for _ in 0..150 {
                let edits = random_batch(&mut rng, &graph);
                let before = graph.clone();
                match graph.apply(&edits, Some(before.rev())) {
                    Ok(applied) => {
                        successes += 1;
                        applied_names.extend(edits.iter().map(Edit::name));
                        assert_eq!(applied.rev, Rev(before.rev().0 + 1));
                        let after = graph.clone();

                        let undo = graph
                            .apply(&applied.inverse.edits, Some(applied.rev))
                            .unwrap_or_else(|e| panic!("inverse of {edits:?} failed: {e}"));
                        assert_eq!(graph.state(), before.state(), "undo of {edits:?}");

                        graph
                            .apply(&undo.inverse.edits, Some(undo.rev))
                            .unwrap_or_else(|e| panic!("redo of {edits:?} failed: {e}"));
                        assert_eq!(graph.state(), after.state(), "redo of {edits:?}");
                        assert_eq!(graph.rev(), Rev(before.rev().0 + 3));
                    }
                    Err(_) => {
                        failures += 1;
                        assert_eq!(graph, before, "a failed batch changed the graph");
                    }
                }
            }
        }
        // The generator must actually exercise every edit and both outcomes.
        let all: BTreeSet<&str> = [
            "scene.create",
            "scene.rename",
            "scene.delete",
            "scene.reorder",
            "item.add",
            "item.remove",
            "item.reorder",
            "item.set_transform",
            "item.set_visible",
            "item.set_crop",
            "source.create",
            "source.duplicate",
            "source.delete",
        ]
        .into();
        assert_eq!(applied_names, all);
        assert!(
            successes > 1000 && failures > 100,
            "{successes} ok, {failures} failed"
        );
    }
}
