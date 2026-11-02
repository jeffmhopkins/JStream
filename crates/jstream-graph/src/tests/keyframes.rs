//! Keyframe scenes (ADR-0024 §2, ADR-0040 §1, ADR-0049): creating one, its overrides
//! and their inverses, what it draws, the edits it refuses, the overrides an item's
//! removal drops, deleting a parent, and the collection file; and a keyframe's override
//! of an item's effects (ADR-0088).

use super::{
    add, apply_err, apply_ok, blocker, blockers, fixture, invalid_reason, plan, scene_create,
    scene_source,
};
use crate::{Graph, Placement};
use jstream_protocol::{
    Crop, DeleteTarget, DroppedOverride, Edit, EntityKind, FilterChain, FilterId, FilterInfo,
    FilterOwner, Finite32, FolderId, ItemGeometry, ItemId, ItemOverride, ItemTransform,
    OverrideField, ProtocolError, SceneId, SceneRole, Settings, Size, SourceId, Vec2,
};
use serde_json::{Value, json};

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
            effects: None,
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
        effects: None,
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

/// A keyframe's override of an item's effects (ADR-0088): one `tint` per entry, with
/// `amount` set and `mask` naming a scene, or the defaults.
fn effects(entries: &[(&str, Option<f64>, Option<&str>)]) -> Vec<FilterInfo> {
    entries
        .iter()
        .map(|(id, amount, mask)| {
            let mut settings = Settings::new();
            if let Some(amount) = amount {
                settings.insert("amount", json!(amount));
            }
            if let Some(mask) = mask {
                settings.insert("mask", json!({"type": "scene", "scene_id": mask}));
            }
            FilterInfo {
                filter_id: FilterId::new(*id),
                kind: "tint".into(),
                enabled: true,
                settings,
            }
        })
        .collect()
}

fn set_effects(scene: &str, item: &str, list: Vec<FilterInfo>) -> Edit {
    Edit::ItemSetEffects {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        effects: list,
    }
}

/// The IDs and `amount`s of the effects `scene` draws `item` with.
fn shown_effects(graph: &Graph, scene: &str, item: &str) -> Vec<(String, f64)> {
    let state = graph.state();
    let scene = state.scene(&SceneId::new(scene)).expect("scene");
    state
        .shown_items(scene)
        .iter()
        .filter(|i| i.id().as_str() == item)
        .flat_map(|i| i.effects().to_vec())
        .map(|f| {
            let amount = f.settings().get("amount").and_then(Value::as_f64);
            (f.id().as_str().to_owned(), amount.unwrap_or(f64::NAN))
        })
        .collect()
}

#[test]
fn a_keyframe_overrides_an_items_effects_whole_and_undoes_exactly() {
    let mut graph = family();
    let own = Edit::FilterAdd {
        owner: FilterOwner::Item {
            item_id: ItemId::new("i1"),
        },
        chain: FilterChain::Video,
        filter_id: FilterId::new("own"),
        kind: "tint".into(),
        settings: Settings::new(),
        enabled: None,
        index: None,
    };
    apply_ok(&mut graph, vec![own]);
    // Not overridden: the keyframe follows the item's own effects.
    assert_eq!(shown_effects(&graph, "k", "i1"), [("own".into(), 1.0)]);

    // The first write's inverse clears the field; its settings are completed.
    let applied = apply_ok(
        &mut graph,
        vec![set_effects("k", "i1", effects(&[("kf", Some(0.25), None)]))],
    );
    assert_eq!(
        applied.inverse.edits,
        [clear("k", "i1", OverrideField::Effects)]
    );
    assert_eq!(shown_effects(&graph, "k", "i1"), [("kf".into(), 0.25)]);
    assert_eq!(shown_effects(&graph, "a", "i1"), [("own".into(), 1.0)]);
    assert_eq!(shown_effects(&graph, "k2", "i1"), [("own".into(), 1.0)]);
    let stored = overrides(&graph, "k")[0]
        .effects
        .clone()
        .expect("overridden");
    assert_eq!(stored[0].settings.get("mask"), Some(&Value::Null));

    // A second write's inverse writes the old list back.
    let applied = apply_ok(
        &mut graph,
        vec![set_effects(
            "k",
            "i1",
            effects(&[("kf", Some(0.5), None), ("kf2", None, None)]),
        )],
    );
    assert_eq!(
        applied.inverse.edits,
        [set_effects("k", "i1", stored.clone())]
    );
    assert_eq!(
        shown_effects(&graph, "k", "i1"),
        [("kf".into(), 0.5), ("kf2".into(), 1.0)]
    );

    // An empty list is an override: no effects there.
    apply_ok(&mut graph, vec![set_effects("k", "i1", Vec::new())]);
    assert_eq!(shown_effects(&graph, "k", "i1"), []);
    assert_eq!(overrides(&graph, "k")[0].effects, Some(Vec::new()));

    // Clearing follows the item again, and undoes with the list it removed.
    let applied = apply_ok(&mut graph, vec![clear("k", "i1", OverrideField::Effects)]);
    assert_eq!(applied.inverse.edits, [set_effects("k", "i1", Vec::new())]);
    assert_eq!(shown_effects(&graph, "k", "i1"), [("own".into(), 1.0)]);
    assert!(overrides(&graph, "k").is_empty());
}

#[test]
fn set_effects_refuses_what_an_item_effect_would() {
    let mut graph = family();
    let field = |graph: &mut Graph, edit: Edit| invalid_reason(apply_err(graph, vec![edit])).0;
    // Only a keyframe scene: the item's own effects take `filter.*`.
    assert_eq!(
        field(&mut graph, set_effects("a", "i1", Vec::new())),
        "edits.0.scene_id"
    );
    assert_eq!(
        field(&mut graph, set_effects("b", "i1", Vec::new())),
        "edits.0.scene_id"
    );
    assert_eq!(
        apply_err(&mut graph, vec![set_effects("nope", "i1", Vec::new())]),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "nope".into(),
        }
    );
    let mut audio = effects(&[("g", None, None)]);
    audio[0].kind = "gain".into();
    assert_eq!(
        field(&mut graph, set_effects("k", "i1", audio)),
        "edits.0.effects.0.kind"
    );
    let mut missing = effects(&[("ok", None, None), ("m", None, None)]);
    missing[1].kind = "nope".into();
    assert_eq!(
        field(&mut graph, set_effects("k", "i1", missing)),
        "edits.0.effects.1.kind"
    );
    assert_eq!(
        field(
            &mut graph,
            set_effects("k", "i1", effects(&[("x", None, None), ("x", None, None)]))
        ),
        "edits.0.effects.1.filter_id"
    );
    assert_eq!(
        field(
            &mut graph,
            set_effects("k", "i1", effects(&[("x", Some(2.0), None)]))
        ),
        "edits.0.effects.0.settings.amount"
    );
    let nine: Vec<(String, Option<f64>, Option<&str>)> =
        (0..9).map(|n| (format!("f{n}"), None, None)).collect();
    let nine: Vec<(&str, Option<f64>, Option<&str>)> = nine
        .iter()
        .map(|(id, a, m)| (id.as_str(), *a, *m))
        .collect();
    assert_eq!(
        field(&mut graph, set_effects("k", "i1", effects(&nine))),
        "edits.0.effects"
    );
    // A reference that would close a cycle through the keyframe scene.
    assert_eq!(
        field(
            &mut graph,
            set_effects("k", "i1", effects(&[("x", None, Some("k"))]))
        ),
        "edits.0.effects.0.settings.mask"
    );
    // Filter edits still refuse a keyframe scene as their owner (ADR-0065 §3).
    let on_keyframe = Edit::FilterAdd {
        owner: FilterOwner::Scene {
            scene_id: SceneId::new("k"),
        },
        chain: FilterChain::Video,
        filter_id: FilterId::new("x"),
        kind: "tint".into(),
        settings: Settings::new(),
        enabled: None,
        index: None,
    };
    assert_eq!(field(&mut graph, on_keyframe), "edits.0.owner");
}

#[test]
fn an_overridden_effects_reference_blocks_a_delete_that_the_plan_clears() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![set_effects(
            "k",
            "i1",
            effects(&[("kf", Some(0.5), Some("b"))]),
        )],
    );
    let delete_b = Edit::SceneDelete {
        scene_id: SceneId::new("b"),
    };
    assert_eq!(
        blockers(apply_err(&mut graph, vec![delete_b])),
        [
            blocker(EntityKind::Source, "sb", "kind.scene_id"),
            blocker(EntityKind::Scene, "k", "overrides.i1.effects.kf.mask"),
        ]
    );
    let edits = plan(
        &graph,
        DeleteTarget::Scene {
            scene_id: SceneId::new("b"),
        },
    );
    let cleared = effects(&[("kf", Some(0.5), None)]);
    let mut cleared_mask = cleared.clone();
    cleared_mask[0].settings.insert("mask", Value::Null);
    assert!(
        edits.contains(&set_effects("k", "i1", cleared_mask)),
        "{edits:?}"
    );
    apply_ok(&mut graph, edits);
    assert_eq!(shown_effects(&graph, "k", "i1"), [("kf".into(), 0.5)]);
}

#[test]
fn an_items_removal_drops_its_overridden_effects_and_undo_restores_them() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![set_effects("k", "i1", effects(&[("kf", None, None)]))],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    let dropped = &applied.dropped_overrides;
    assert_eq!(dropped.len(), 1);
    assert_eq!(
        dropped[0].dropped.effects.as_ref().map(Vec::len),
        Some(1),
        "the dropped override carries its effects"
    );
    // Deleting the keyframe scene also undoes with its overridden effects.
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![set_effects("k", "i1", effects(&[("kf", None, None)]))],
    );
    apply_ok(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("k"),
        }],
    );
}

#[test]
fn overridden_effects_save_and_load() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![
            set_effects("k", "i1", effects(&[("kf", Some(0.5), Some("b"))])),
            set_effects("k", "i2", Vec::new()),
            set_opacity("i2", Some("k"), 0.5),
        ],
    );
    let json = graph.to_collection_json();
    let loaded = super::graph()
        .with_collection_json(&json)
        .expect("the file loads");
    assert_eq!(loaded.state(), graph.state());
    assert_eq!(loaded.to_collection_json(), json);
}

#[test]
fn an_overridden_effect_of_a_missing_kind_loads_and_saves_as_it_was() {
    let mut graph = family();
    apply_ok(
        &mut graph,
        vec![set_effects(
            "k",
            "i1",
            effects(&[("kf", Some(0.5), None), ("x", None, None)]),
        )],
    );
    let json = graph.to_collection_json();
    let without = super::graph().without_kinds(["tint"]);
    // Every other `tint` would be refused too, so only the override holds one.
    let loaded = without
        .with_collection_json(&json)
        .expect("the file loads without the kind");
    assert_eq!(loaded.to_collection_json(), json);
    assert_eq!(
        overrides(&loaded, "k")[0].effects.as_ref().map(Vec::len),
        Some(2)
    );
}

fn keyframe_owner(scene: &str, item: &str) -> FilterOwner {
    FilterOwner::Keyframe {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
    }
}

fn tint_on(owner: FilterOwner, id: &str) -> Edit {
    Edit::FilterAdd {
        owner,
        chain: FilterChain::Video,
        filter_id: FilterId::new(id),
        kind: "tint".into(),
        settings: Settings::new(),
        enabled: None,
        index: None,
    }
}

/// The `keyframe` owner edits one effect of a keyframe's override (ADR-0088): the first
/// edit copies the item's own effects and undoes by clearing the override, later ones
/// undo as on any owner, and the item's own effects never change.
#[test]
fn filter_edits_on_the_keyframe_owner_copy_the_items_effects_first() {
    let mut graph = family();
    let item = FilterOwner::Item {
        item_id: ItemId::new("i1"),
    };
    apply_ok(&mut graph, vec![tint_on(item, "own")]);
    let kf = keyframe_owner("k", "i1");
    let applied = apply_ok(
        &mut graph,
        vec![Edit::FilterSetSettings {
            owner: kf.clone(),
            filter_id: FilterId::new("own"),
            patch: Settings([("amount".to_owned(), json!(0.5))].into_iter().collect()),
        }],
    );
    assert_eq!(
        applied.inverse.edits,
        [clear("k", "i1", OverrideField::Effects)]
    );
    assert_eq!(shown_effects(&graph, "k", "i1"), [("own".into(), 0.5)]);
    assert_eq!(shown_effects(&graph, "a", "i1"), [("own".into(), 1.0)]);

    // Later edits have the usual inverses, on the same owner.
    let applied = apply_ok(&mut graph, vec![tint_on(kf.clone(), "more")]);
    assert_eq!(
        applied.inverse.edits,
        [Edit::FilterRemove {
            owner: kf.clone(),
            filter_id: FilterId::new("more"),
        }]
    );
    apply_ok(
        &mut graph,
        vec![
            Edit::FilterReorder {
                owner: kf.clone(),
                filter_id: FilterId::new("more"),
                index: 0,
            },
            Edit::FilterSetEnabled {
                owner: kf.clone(),
                filter_id: FilterId::new("own"),
                enabled: false,
            },
            Edit::FilterRemove {
                owner: kf.clone(),
                filter_id: FilterId::new("more"),
            },
        ],
    );
    assert_eq!(shown_effects(&graph, "k", "i1"), [("own".into(), 0.5)]);
    assert!(
        !graph
            .state()
            .scene(&SceneId::new("k"))
            .and_then(|k| graph.state().shown_items(k).into_iter().next())
            .is_some_and(|i| i.effects()[0].enabled()),
        "switched off in the keyframe"
    );
    assert_eq!(shown_effects(&graph, "a", "i1"), [("own".into(), 1.0)]);
}

#[test]
fn the_keyframe_owner_refuses_what_isnt_a_keyframes_item() {
    let mut graph = family();
    apply_ok(&mut graph, vec![add("b", "ib", "cam")]);
    let field = |graph: &mut Graph, edit: Edit| invalid_reason(apply_err(graph, vec![edit])).0;
    assert_eq!(
        field(&mut graph, tint_on(keyframe_owner("a", "i1"), "x")),
        "edits.0.owner",
        "not a keyframe scene"
    );
    assert_eq!(
        field(&mut graph, tint_on(keyframe_owner("k", "ib"), "x")),
        "edits.0.owner",
        "not in the keyframe's tree"
    );
    assert_eq!(
        apply_err(&mut graph, vec![tint_on(keyframe_owner("k", "nope"), "x")]),
        ProtocolError::NotFound {
            entity: EntityKind::Item,
            id: "nope".into(),
        }
    );
    let mut audio = tint_on(keyframe_owner("k", "i1"), "x");
    if let Edit::FilterAdd { chain, kind, .. } = &mut audio {
        *chain = FilterChain::Audio;
        *kind = "gain".into();
    }
    assert_eq!(field(&mut graph, audio), "edits.0.chain");
    // A reference through the keyframe owner is an edge of the keyframe scene.
    let mut cycle = tint_on(keyframe_owner("k", "i1"), "x");
    if let Edit::FilterAdd { settings, .. } = &mut cycle {
        settings.insert("mask", json!({"type": "scene", "scene_id": "k"}));
    }
    assert_eq!(field(&mut graph, cycle), "edits.0.settings.mask");
}

/// An undo of a keyframe effects step conflicts with another client's later filter edit
/// on that keyframe's effects (ADR-0032, ADR-0088 §4): both touch the item's one effects
/// property, so the undo can't silently drop it.
#[test]
fn a_keyframe_effects_undo_conflicts_with_a_later_filter_edit_there() {
    let mut graph = family();
    let item = FilterOwner::Item {
        item_id: ItemId::new("i1"),
    };
    apply_ok(&mut graph, vec![tint_on(item, "own")]);
    let kf = keyframe_owner("k", "i1");
    // A's edit copies the item's effects; its inverse clears the override.
    let a = apply_ok(
        &mut graph,
        vec![Edit::FilterSetSettings {
            owner: kf.clone(),
            filter_id: FilterId::new("own"),
            patch: Settings([("amount".to_owned(), json!(0.5))].into_iter().collect()),
        }],
    );
    // B adds a filter to the copy.
    let b = apply_ok(&mut graph, vec![tint_on(kf.clone(), "more")]);
    assert!(a.touched.conflicts_with(&b.touched), "A's step and B's");
    let undo = apply_ok(&mut graph, a.inverse.edits.clone());
    assert!(undo.touched.conflicts_with(&b.touched), "A's undo and B's");

    // A whole write conflicts with a later filter edit, and with a later write.
    let c = apply_ok(&mut graph, vec![set_effects("k", "i1", Vec::new())]);
    let d = apply_ok(&mut graph, vec![tint_on(kf.clone(), "late")]);
    assert!(c.touched.conflicts_with(&d.touched));
    // Every filter edit on the keyframe owner touches it, not only the copying one.
    for edit in [
        Edit::FilterSetEnabled {
            owner: kf.clone(),
            filter_id: FilterId::new("late"),
            enabled: false,
        },
        Edit::FilterReorder {
            owner: kf.clone(),
            filter_id: FilterId::new("late"),
            index: 0,
        },
        Edit::FilterRemove {
            owner: kf.clone(),
            filter_id: FilterId::new("late"),
        },
    ] {
        let e = apply_ok(&mut graph, vec![edit]);
        assert!(c.touched.conflicts_with(&e.touched));
    }
    // The item's own effects are another property: no conflict there.
    let own = apply_ok(
        &mut graph,
        vec![tint_on(
            FilterOwner::Item {
                item_id: ItemId::new("i1"),
            },
            "own2",
        )],
    );
    assert!(!c.touched.conflicts_with(&own.touched));
}
