//! Keyframe scenes (ADR-0024 §2, ADR-0040 §1, ADR-0049): creating one, its overrides
//! and their inverses, what it draws, the edits it refuses, the overrides an item's
//! removal drops, deleting a parent, and the collection file.

use super::{
    add, apply_err, apply_ok, blocker, blockers, fixture, invalid_reason, plan, scene_create,
    scene_source,
};
use crate::{Graph, Placement};
use jstream_protocol::{
    Crop, DeleteTarget, DroppedOverride, Edit, EntityKind, Finite32, FolderId, ItemGeometry,
    ItemId, ItemOverride, ItemTransform, OverrideField, ProtocolError, SceneId, SceneRole, Size,
    SourceId, Vec2,
};

fn variant(id: &str, of: &str) -> Edit {
    Edit::SceneCreateVariant {
        scene_id: SceneId::new(id),
        variant_of: SceneId::new(of),
        name: format!("Keyframe {id}"),
    }
}

fn geometry(x: f32) -> ItemGeometry {
    ItemGeometry::try_new(
        Vec2::new(x, 100.0).expect("finite"),
        Size::try_new(320.0, 180.0).expect("size"),
    )
    .expect("geometry")
}

fn set_geometry(item: &str, scene: Option<&str>, x: f32) -> Edit {
    Edit::ItemSetGeometry {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        geometry: geometry(x),
    }
}

fn set_visible(item: &str, scene: Option<&str>, visible: bool) -> Edit {
    Edit::ItemSetVisible {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        visible,
    }
}

fn set_opacity(item: &str, scene: Option<&str>, opacity: f32) -> Edit {
    Edit::ItemSetOpacity {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        opacity: Finite32::new(opacity).expect("finite"),
    }
}

fn clear(scene: &str, item: &str, field: OverrideField) -> Edit {
    Edit::ItemClearOverride {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        field,
    }
}

/// The fixture with items `i1` (cam) and `i2` (mic) in scene `a`, at x 0 and 50, and
/// keyframe scenes `k` and `k2` of `a`.
fn family() -> Graph {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "i1", "cam"),
            add("a", "i2", "mic"),
            set_geometry("i1", None, 0.0),
            set_geometry("i2", None, 50.0),
            variant("k", "a"),
            variant("k2", "a"),
        ],
    );
    graph
}

/// What scene `scene` draws: each item's ID, geometry x (or `None` for a legacy
/// placement), visibility and opacity.
fn shown(graph: &Graph, scene: &str) -> Vec<(String, Option<f32>, bool, f32)> {
    let state = graph.state();
    let scene = state.scene(&SceneId::new(scene)).expect("scene");
    state
        .shown_items(scene)
        .iter()
        .map(|item| {
            let x = match item.placement() {
                Placement::Geometry(g) => Some(g.center.x.get()),
                _ => None,
            };
            (
                item.id().as_str().to_owned(),
                x,
                item.visible(),
                item.opacity().get(),
            )
        })
        .collect()
}

fn overrides(graph: &Graph, scene: &str) -> Vec<ItemOverride> {
    graph
        .state()
        .scene(&SceneId::new(scene))
        .expect("scene")
        .to_info()
        .overrides
}

#[test]
fn a_keyframe_scene_shows_its_parents_items_and_follows_them_live() {
    let mut graph = family();
    let info = graph
        .state()
        .scene(&SceneId::new("k"))
        .expect("k")
        .to_info();
    assert_eq!(info.variant_of, Some(SceneId::new("a")));
    assert!(info.items.is_empty(), "no items of its own");
    assert_eq!(shown(&graph, "k"), shown(&graph, "a"));
    // An override applies only in its keyframe scene.
    apply_ok(&mut graph, vec![set_geometry("i1", Some("k"), 400.0)]);
    assert_eq!(shown(&graph, "k")[0].1, Some(400.0));
    assert_eq!(shown(&graph, "a")[0].1, Some(0.0));
    assert_eq!(shown(&graph, "k2")[0].1, Some(0.0));
    // A parent edit shows in every keyframe scene that doesn't override that field.
    apply_ok(
        &mut graph,
        vec![
            set_geometry("i1", None, 10.0),
            set_visible("i1", None, false),
        ],
    );
    assert_eq!(
        shown(&graph, "k")[0],
        ("i1".into(), Some(400.0), false, 1.0)
    );
    assert_eq!(
        shown(&graph, "k2")[0],
        ("i1".into(), Some(10.0), false, 1.0)
    );
    assert_eq!(
        graph.state().family_of(&SceneId::new("k")),
        &SceneId::new("a")
    );
    assert_eq!(
        graph.state().family_of(&SceneId::new("a")),
        &SceneId::new("a")
    );
}

#[test]
fn writes_and_clears_have_the_inverses_adr_0049_gives() {
    let mut graph = family();
    // A field that was absent is undone by clearing it.
    let applied = apply_ok(&mut graph, vec![set_opacity("i2", Some("k"), 0.5)]);
    assert_eq!(
        applied.inverse.edits,
        [clear("k", "i2", OverrideField::Opacity)]
    );
    // A field that was present is undone by writing the old value.
    let applied = apply_ok(&mut graph, vec![set_opacity("i2", Some("k"), 0.25)]);
    assert_eq!(applied.inverse.edits, [set_opacity("i2", Some("k"), 0.5)]);
    // Clearing a present field is undone by writing it back.
    let applied = apply_ok(&mut graph, vec![clear("k", "i2", OverrideField::Opacity)]);
    assert_eq!(applied.inverse.edits, [set_opacity("i2", Some("k"), 0.25)]);
    assert!(
        overrides(&graph, "k").is_empty(),
        "no empty override is kept"
    );
    // Clearing an absent field changes nothing and has no inverse.
    let before = graph.state().clone();
    let applied = graph
        .apply(&[clear("k", "i2", OverrideField::Crop)], None)
        .expect("applies");
    assert!(applied.inverse.edits.is_empty());
    assert_eq!(graph.state(), &before);
    // Every field, as `state.get` reports them.
    let crop = Crop {
        left: 4,
        top: 0,
        right: 0,
        bottom: 2,
    };
    apply_ok(
        &mut graph,
        vec![
            set_geometry("i1", Some("k"), 5.0),
            Edit::ItemSetCrop {
                item_id: ItemId::new("i1"),
                scene_id: Some(SceneId::new("k")),
                crop,
            },
            set_visible("i1", Some("k"), false),
            set_opacity("i1", Some("k"), 0.75),
        ],
    );
    assert_eq!(
        overrides(&graph, "k"),
        [ItemOverride {
            transform: None,
            item_id: ItemId::new("i1"),
            geometry: Some(geometry(5.0)),
            crop: Some(crop),
            visible: Some(false),
            opacity: Some(Finite32::new(0.75).expect("finite")),
        }]
    );
    // "Reset keyframe": a batch of clears, undone as a batch.
    apply_ok(
        &mut graph,
        [
            OverrideField::Geometry,
            OverrideField::Crop,
            OverrideField::Visible,
            OverrideField::Opacity,
        ]
        .map(|field| clear("k", "i1", field))
        .to_vec(),
    );
    assert_eq!(shown(&graph, "k"), shown(&graph, "a"));
}

#[test]
fn a_keyframe_scene_refuses_what_it_cant_hold() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![
            Edit::FolderCreate {
                folder_id: FolderId::new("f"),
                name: "F".into(),
                parent: None,
            },
            scene_create("g"),
            Edit::SceneSetRole {
                scene_id: SceneId::new("g"),
                role: SceneRole::Group,
            },
            scene_source("sg", "g"),
            add("g", "gi", "cam"),
            add("a", "ig", "sg"),
            // A scene placed in the parent that isn't a group.
            scene_create("n"),
            scene_source("sn", "n"),
            add("n", "ni", "cam"),
            add("a", "in", "sn"),
        ],
    );
    let refused = |graph: &mut Graph, edit: Edit| invalid_reason(apply_err(graph, vec![edit]));
    // A keyframe of a keyframe is made on the parent.
    assert_eq!(
        refused(&mut graph, variant("kk", "k")).0,
        "edits.0.variant_of"
    );
    // No items of its own.
    assert_eq!(
        refused(&mut graph, add("k", "i9", "cam")).0,
        "edits.0.scene_id"
    );
    let into = Edit::ItemMove {
        item_id: ItemId::new("i1"),
        scene_id: SceneId::new("k"),
        index: 0,
    };
    assert_eq!(refused(&mut graph, into).0, "edits.0.scene_id");
    // It sits under its parent, never in a folder.
    let folder = Edit::SceneSetFolder {
        scene_id: SceneId::new("k"),
        folder_id: Some(FolderId::new("f")),
    };
    assert_eq!(refused(&mut graph, folder).0, "edits.0.scene_id");
    // Only the item's own scene and a keyframe scene whose tree holds it can be named.
    assert_eq!(
        refused(&mut graph, set_visible("i1", Some("b"), false)).0,
        "edits.0.scene_id"
    );
    // A group's items can be overridden (#846), but not those of a scene that isn't a
    // group, placed in the parent.
    apply_ok(&mut graph, vec![set_visible("gi", Some("k"), false)]);
    let (field, reason) = refused(&mut graph, set_visible("ni", Some("k"), false));
    assert_eq!(field, "edits.0.scene_id");
    assert!(reason.contains("group placed in it"), "{reason}");
    // `item.clear_override` names a keyframe scene and an item in its parent's tree.
    assert_eq!(
        refused(&mut graph, clear("b", "i1", OverrideField::Visible)).0,
        "edits.0.scene_id"
    );
    assert_eq!(
        refused(&mut graph, clear("k", "ni", OverrideField::Visible)).0,
        "edits.0.item_id"
    );
    assert!(matches!(
        apply_err(&mut graph, vec![variant("k3", "nope")]),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            ..
        }
    ));
    // An opacity outside [0, 1] is refused as the item's own is.
    assert_eq!(
        refused(&mut graph, set_opacity("i1", Some("k"), 1.5)).0,
        "edits.0.opacity"
    );
}

#[test]
fn a_keyframe_scene_cant_be_placed_in_its_parent() {
    let mut graph = family();
    apply_ok(&mut graph, vec![scene_source("sk", "k")]);
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![add("a", "ik", "sk")]));
    assert_eq!(field, "edits.0.source_id");
    assert!(reason.contains("cycle"), "{reason}");
    // Elsewhere it is placed like any scene.
    apply_ok(&mut graph, vec![add("b", "ik", "sk")]);
}

#[test]
fn removing_or_moving_an_item_out_drops_its_overrides_and_undo_restores_them() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![
            set_geometry("i1", Some("k"), 400.0),
            set_visible("i1", Some("k2"), false),
            set_opacity("i2", Some("k"), 0.5),
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    let dropped = |scene: &str, o: ItemOverride| DroppedOverride {
        scene_id: SceneId::new(scene),
        dropped: o,
    };
    let none = |item: &str| ItemOverride {
        transform: None,
        item_id: ItemId::new(item),
        geometry: None,
        crop: None,
        visible: None,
        opacity: None,
    };
    assert_eq!(
        applied.dropped_overrides,
        [
            dropped(
                "k",
                ItemOverride {
                    geometry: Some(geometry(400.0)),
                    ..none("i1")
                }
            ),
            dropped(
                "k2",
                ItemOverride {
                    visible: Some(false),
                    ..none("i1")
                }
            ),
        ]
    );
    assert_eq!(
        overrides(&graph, "k"),
        [ItemOverride {
            opacity: Some(Finite32::new(0.5).expect("finite")),
            ..none("i2")
        }]
    );
    // A move to another scene drops them too; a reorder in the parent doesn't.
    let reorder = Edit::ItemMove {
        item_id: ItemId::new("i2"),
        scene_id: SceneId::new("a"),
        index: 0,
    };
    assert!(
        apply_ok(&mut graph, vec![reorder])
            .dropped_overrides
            .is_empty()
    );
    let out = Edit::ItemMove {
        item_id: ItemId::new("i2"),
        scene_id: SceneId::new("b"),
        index: 0,
    };
    let applied = apply_ok(&mut graph, vec![out]);
    assert_eq!(applied.dropped_overrides.len(), 1);
    assert!(overrides(&graph, "k").is_empty());
}

#[test]
fn a_parent_is_deleted_only_with_its_keyframe_scenes() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![
            set_geometry("i1", Some("k"), 400.0),
            scene_source("sk", "k"),
            add("b", "ik", "sk"),
        ],
    );
    let error = apply_err(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("a"),
        }],
    );
    let found = blockers(error);
    assert!(found.contains(&blocker(EntityKind::Scene, "k", "variant_of")));
    assert!(found.contains(&blocker(EntityKind::Scene, "k2", "variant_of")));
    // `delete.plan` takes the keyframe scenes, and what shows them, with it.
    let edits = plan(
        &graph,
        DeleteTarget::Scene {
            scene_id: SceneId::new("a"),
        },
    );
    let deletes: Vec<&Edit> = edits
        .iter()
        .filter(|e| matches!(e, Edit::SceneDelete { .. }))
        .collect();
    assert_eq!(deletes.len(), 3, "{edits:?}");
    assert_eq!(
        edits.last(),
        Some(&Edit::SceneDelete {
            scene_id: SceneId::new("a"),
        })
    );
    apply_ok(&mut graph, edits);
    assert!(graph.state().scene(&SceneId::new("k")).is_none());
}

#[test]
fn deleting_a_keyframe_scene_undoes_with_its_overrides() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![
            set_geometry("i1", Some("k"), 400.0),
            set_opacity("i2", Some("k"), 0.5),
            Edit::SceneSetTransitions {
                scene_id: SceneId::new("k"),
                transition_in: Some(jstream_protocol::TransitionPresetId::new(
                    jstream_protocol::INITIAL_PRESET,
                )),
                transition_out: None,
            },
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("k"),
        }],
    );
    assert_eq!(applied.inverse.edits[0], variant("k", "a"));
}

/// `family` with group `g` placed in `a` by item `ig` (source `sg`), holding `gi` (cam)
/// and group `h`, placed in `g` by `ih` (source `sh`), holding `hi` (mic).
fn nested_groups() -> Graph {
    let mut graph = family();
    let group = |id: &str| Edit::SceneSetRole {
        scene_id: SceneId::new(id),
        role: SceneRole::Group,
    };
    apply_ok(
        &mut graph,
        vec![
            scene_create("g"),
            group("g"),
            scene_source("sg", "g"),
            scene_create("h"),
            group("h"),
            scene_source("sh", "h"),
            add("g", "gi", "cam"),
            add("h", "hi", "mic"),
            add("g", "ih", "sh"),
            add("a", "ig", "sg"),
        ],
    );
    graph
}

/// Overrides of a group's children at any depth (ADR-0024 §2, #846): written and shown
/// through the group, kept while any path reaches the child (ADR-0040 §2), and dropped,
/// with undo restoring them, by each edit that takes the child out of the tree (§1).
#[test]
fn a_keyframe_overrides_a_groups_children_at_any_depth() {
    let mut graph = nested_groups();
    apply_ok(
        &mut graph,
        vec![
            set_visible("gi", Some("k"), false),
            set_geometry("hi", Some("k"), 700.0),
        ],
    );
    let state = graph.state();
    let k = state.scene(&SceneId::new("k")).expect("k");
    let h = state.scene(&SceneId::new("h")).expect("h");
    let through: Vec<_> = crate::State::items_through(h, k)
        .iter()
        .map(|item| match item.placement() {
            Placement::Geometry(g) => g.center.x.get(),
            _ => f32::NAN,
        })
        .collect();
    assert_eq!(through, [700.0], "h's child as k shows it");
    assert!(state.overrides_within(k, &SceneId::new("g")));
    assert!(state.overrides_within(k, &SceneId::new("h")));
    assert!(!state.overrides_within(
        state.scene(&SceneId::new("k2")).expect("k2"),
        &SceneId::new("g")
    ));
    assert_eq!(overrides(&graph, "k").len(), 2);

    // Grouping keeps overrides: a child moved from the group into the parent stays in
    // the tree, and so does a group placed twice when one placement goes.
    let up = Edit::ItemMove {
        item_id: ItemId::new("gi"),
        scene_id: SceneId::new("a"),
        index: 0,
    };
    assert!(apply_ok(&mut graph, vec![up]).dropped_overrides.is_empty());
    apply_ok(&mut graph, vec![add("a", "ig2", "sg")]);
    let one = Edit::ItemRemove {
        item_id: ItemId::new("ig"),
    };
    assert!(apply_ok(&mut graph, vec![one]).dropped_overrides.is_empty());
    assert_eq!(overrides(&graph, "k").len(), 2);

    // A group that becomes a scene takes its children out of every tree.
    let before = overrides(&graph, "k");
    let role = Edit::SceneSetRole {
        scene_id: SceneId::new("h"),
        role: SceneRole::Scene,
    };
    let applied = apply_ok(&mut graph, vec![role]);
    assert_eq!(
        applied
            .dropped_overrides
            .iter()
            .map(|d| d.dropped.item_id.as_str())
            .collect::<Vec<_>>(),
        ["hi"]
    );
    apply_ok(&mut graph, applied.inverse.edits.clone());
    assert_eq!(overrides(&graph, "k"), before);

    // So does removing the last item that places the outer group, which reaches `hi`
    // through it.
    let last = Edit::ItemRemove {
        item_id: ItemId::new("ig2"),
    };
    let applied = apply_ok(&mut graph, vec![last]);
    assert_eq!(applied.dropped_overrides.len(), 1);
    assert_eq!(
        overrides(&graph, "k").len(),
        1,
        "gi is the parent's own now"
    );
    apply_ok(&mut graph, applied.inverse.edits.clone());
    assert_eq!(overrides(&graph, "k"), before);

    // Retargeting the item that places it does too.
    let retarget = Edit::ItemSetSource {
        item_id: ItemId::new("ig2"),
        source_id: SourceId::new("cam"),
    };
    let applied = apply_ok(&mut graph, vec![retarget]);
    assert_eq!(applied.dropped_overrides.len(), 1);
    apply_ok(&mut graph, applied.inverse.edits.clone());
    assert_eq!(overrides(&graph, "k"), before);
}

/// A keyframe's overrides of a group's children are saved and loaded.
#[test]
fn overrides_of_a_groups_children_save_and_load() {
    let mut graph = nested_groups();
    apply_ok(&mut graph, vec![set_geometry("hi", Some("k"), 700.0)]);
    let json = graph.to_collection_json();
    let loaded = super::graph()
        .with_collection_json(&json)
        .expect("the file loads");
    assert_eq!(loaded.state(), graph.state());
    assert_eq!(overrides(&loaded, "k").len(), 1);
}

#[test]
fn keyframe_scenes_save_and_load() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![
            set_geometry("i1", Some("k"), 400.0),
            set_visible("i2", Some("k2"), false),
            // The 0.3 form overrides the same field, and a keyframe scene's role is set
            // like any scene's.
            Edit::ItemSetTransform {
                item_id: ItemId::new("i2"),
                scene_id: Some(SceneId::new("k")),
                transform: ItemTransform::IDENTITY,
            },
            Edit::SceneSetRole {
                scene_id: SceneId::new("k2"),
                role: SceneRole::Group,
            },
            // A keyframe scene listed before its parent.
            Edit::SceneReorder {
                scene_id: SceneId::new("k2"),
                index: 0,
            },
        ],
    );
    let transform = overrides(&graph, "k")
        .into_iter()
        .find(|o| o.item_id == ItemId::new("i2"))
        .and_then(|o| o.transform);
    assert_eq!(transform, Some(ItemTransform::IDENTITY));
    let json = graph.to_collection_json();
    let loaded = super::graph()
        .with_collection_json(&json)
        .expect("the file loads");
    assert_eq!(loaded.state(), graph.state());
    assert_eq!(loaded.to_collection_json(), json);
}

/// A scene's background is one of its options, and checked; a keyframe scene shows its
/// parent's, so it can't have one of its own (ADR-0089 §4).
#[test]
fn a_background_is_checked_and_a_keyframe_has_none_of_its_own() {
    let mut graph = family();
    let background = |scene: &str, color: &str| Edit::SceneSetOptions {
        scene_id: SceneId::new(scene),
        options: jstream_protocol::SceneOptions {
            background: Some(color.into()),
            ..jstream_protocol::SceneOptions::default()
        },
    };
    apply_ok(&mut graph, vec![background("a", "#0F111580")]);
    let options = graph
        .state()
        .scene(&SceneId::new("a"))
        .map(|s| s.options().clone());
    assert_eq!(
        options.and_then(|o| o.background),
        Some("#0F111580".to_owned())
    );
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![background("a", "blue")]));
    assert!(field.ends_with("background"), "{field}");
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![background("k", "#000000")]));
    assert!(field.ends_with("background"), "{field}");
    assert!(reason.contains("parent"), "{reason}");
}
