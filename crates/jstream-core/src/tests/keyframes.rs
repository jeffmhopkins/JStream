//! Keyframe scenes (K1, ADR-0049) through `InProcessControl`: a keyframe scene draws its
//! parent's items at their effective values and follows the parent's edits, a `move`
//! within a family pairs by item, and a batch reports the overrides it drops, which undo
//! restores.

use super::{add, block_on, connected, create, edits};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Batch, Call, CallResult, DroppedOverride, Easing, Edit, Finite32, GestureId,
    HistoryCall, HistoryOp, ItemGeometry, ItemId, ItemOverride, Query, QueryResult, SceneId,
    Settings, Size, SourceId, SourceKind, TransitionPresetId, Vec2,
};
use jstream_render::Placement;
use jstream_render::transition::Track;
use serde_json::json;

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

fn at(x: f32) -> ItemGeometry {
    ItemGeometry::try_new(
        Vec2::new(x, 300.0).expect("finite"),
        Size::try_new(100.0, 100.0).expect("positive"),
    )
    .expect("valid")
}

/// `item.set_geometry` of `item` to `x`, in `scene` (a keyframe scene) or on the item.
fn geometry(item: &str, scene: Option<&str>, x: f32) -> Edit {
    Edit::ItemSetGeometry {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        geometry: at(x),
    }
}

fn variant(scene: &str, of: &str) -> Edit {
    Edit::SceneCreateVariant {
        scene_id: SceneId::new(scene),
        variant_of: SceneId::new(of),
        name: scene.to_uppercase(),
    }
}

fn apply(control: &dyn Control, list: Vec<Edit>) -> Vec<DroppedOverride> {
    match block_on(control.call(Call::Batch(edits(list))))
        .expect("applies")
        .result
    {
        CallResult::Batch {
            dropped_overrides, ..
        } => dropped_overrides,
        other => panic!("not a batch result: {other:?}"),
    }
}

fn switch(control: &dyn Control, scene: &str, preset: Option<&str>) {
    let action = Action::SceneSwitch {
        scene_id: SceneId::new(scene),
        preset_id: preset.map(TransitionPresetId::new),
    };
    block_on(control.call(Call::Action { action })).expect("switch");
}

/// Scene `a` placing `red` twice, `a1` at x 100 and `a2` at x 500, with the keyframe
/// scene `k` of it, built and on program.
fn family() -> Engine {
    let engine = Engine::new();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            create("a"),
            colour("red"),
            add("a", "a1", "red"),
            add("a", "a2", "red"),
            geometry("a1", None, 100.0),
            geometry("a2", None, 500.0),
            variant("k", "a"),
        ],
    );
    assert!(engine.settle_sources(), "the sources were built in time");
    engine
}

fn x(placement: &Placement) -> f32 {
    match placement {
        Placement::Geometry(geometry) => geometry.center.x.get(),
        Placement::Legacy(_) => panic!("not a geometry placement"),
    }
}

/// What the program shot places for `scene`: each item's ID, x and opacity.
fn placed(engine: &Engine, scene: &str) -> Vec<(String, f32, f32)> {
    engine
        .render_hub()
        .load()
        .shot()
        .placed(&SceneId::new(scene))
        .expect("the shot draws the scene")
        .iter()
        .map(|(item, placement, opacity)| (item.to_string(), x(placement), *opacity))
        .collect()
}

#[test]
fn a_keyframe_scene_draws_its_parents_items_with_its_overrides() {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            Edit::ItemSetOpacity {
                item_id: ItemId::new("a1"),
                scene_id: Some(SceneId::new("k")),
                opacity: Finite32::new(0.5).expect("finite"),
            },
            geometry("a2", Some("k"), 900.0),
        ],
    );
    switch(&control, "k", None);
    assert_eq!(
        engine.render_hub().load().shot().program(),
        Some(&SceneId::new("k"))
    );
    assert_eq!(
        placed(&engine, "k"),
        [("a1".into(), 100.0, 0.5), ("a2".into(), 900.0, 1.0)]
    );
    // An edit to the parent shows through every field the keyframe doesn't override.
    apply(
        &control,
        vec![geometry("a1", None, 200.0), geometry("a2", None, 300.0)],
    );
    assert_eq!(
        placed(&engine, "k"),
        [("a1".into(), 200.0, 0.5), ("a2".into(), 900.0, 1.0)]
    );
}

#[test]
fn a_move_within_a_family_pairs_by_item() {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("move"),
                name: "Move".into(),
                kind: "move".into(),
                duration_ms: 400,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
            // `a1` is hidden in the keyframe: by source, `a2` would pair with `a1`.
            Edit::ItemSetVisible {
                item_id: ItemId::new("a1"),
                scene_id: Some(SceneId::new("k")),
                visible: false,
            },
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let tracks = engine
        .render_hub()
        .load()
        .shot()
        .tracks()
        .expect("a transition runs");
    let [
        Track::Disappear { from: leaving, .. },
        Track::Pair { from, to, .. },
    ] = tracks.as_slice()
    else {
        panic!("not one leaving and one pair: {tracks:?}");
    };
    assert_eq!(leaving.geometry.center.x.get(), 100.0, "a1 leaves");
    assert_eq!(
        (from.geometry.center.x.get(), to.geometry.center.x.get()),
        (500.0, 500.0),
        "a2 stays where it is"
    );
}

#[test]
fn a_batch_reports_the_overrides_it_drops_and_undo_restores_them() {
    let engine = family();
    let control = connected(&engine);
    let opacity = Finite32::new(0.25).expect("finite");
    apply(
        &control,
        vec![Edit::ItemSetOpacity {
            item_id: ItemId::new("a1"),
            scene_id: Some(SceneId::new("k")),
            opacity,
        }],
    );
    let dropped = apply(
        &control,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("a1"),
        }],
    );
    let o = ItemOverride {
        transform: None,
        item_id: ItemId::new("a1"),
        geometry: None,
        crop: None,
        visible: None,
        opacity: Some(opacity),
    };
    assert_eq!(
        dropped,
        [DroppedOverride {
            scene_id: SceneId::new("k"),
            dropped: o.clone(),
        }]
    );
    let overrides = |control: &dyn Control| {
        let call = Call::Query {
            query: Query::StateGet,
        };
        let CallResult::Query {
            result: QueryResult::StateGet { scenes, .. },
        } = block_on(control.call(call)).expect("state.get").result
        else {
            panic!("not state.get");
        };
        let k = scenes
            .into_iter()
            .find(|s| s.scene_id == SceneId::new("k"))
            .expect("k");
        assert_eq!(k.variant_of, Some(SceneId::new("a")));
        k.overrides
    };
    assert!(overrides(&control).is_empty());
    let undo = Call::History(HistoryCall::new(HistoryOp::Undo));
    block_on(control.call(undo)).expect("undo");
    assert_eq!(overrides(&control), [o]);
    // Clearing a field that isn't overridden changes nothing and is no undo step, so the
    // next undo takes back the opacity write.
    apply(
        &control,
        vec![Edit::ItemClearOverride {
            scene_id: SceneId::new("k"),
            item_id: ItemId::new("a1"),
            field: jstream_protocol::OverrideField::Crop,
        }],
    );
    let undo = Call::History(HistoryCall::new(HistoryOp::Undo));
    block_on(control.call(undo)).expect("undo");
    assert!(overrides(&control).is_empty());
}

/// A live value addressed to a keyframe scene is a parameter of that keyframe's (#846):
/// it shows only where the keyframe draws the item, beside the item's own live value,
/// which shows in the keyframe only through the fields the keyframe doesn't override.
/// Committing it writes the keyframe's override, and deleting the keyframe drops it.
#[test]
fn a_live_value_on_a_keyframe_scene_shows_where_the_keyframe_draws() {
    let engine = family();
    let control = connected(&engine);
    apply(&control, vec![geometry("a2", Some("k"), 900.0)]);
    let live_set = |gesture: &str, values: Vec<Edit>| {
        let action = Action::LiveSet {
            gesture_id: GestureId::new(gesture),
            values,
        };
        block_on(control.call(Call::Action { action })).expect("live.set");
    };
    let drawn = |scene: &str| -> Vec<(String, f32)> {
        let hub = engine.render_hub();
        hub.load()
            .shot()
            .drawn(&SceneId::new(scene), &hub.load_live())
            .expect("the shot draws the scene")
            .iter()
            .map(|(item, placement, _)| (item.to_string(), x(placement)))
            .collect()
    };
    switch(&control, "k", None);

    // The parent's live values show where the keyframe follows it: a1, not a2.
    live_set(
        "parent",
        vec![geometry("a1", None, 150.0), geometry("a2", None, 550.0)],
    );
    assert_eq!(drawn("k"), [("a1".into(), 150.0), ("a2".into(), 900.0)]);

    // The keyframe's own live values win in the keyframe, for either kind of field.
    live_set(
        "key",
        vec![
            geometry("a1", Some("k"), 170.0),
            geometry("a2", Some("k"), 950.0),
        ],
    );
    assert_eq!(drawn("k"), [("a1".into(), 170.0), ("a2".into(), 950.0)]);
    let call = Call::Query {
        query: Query::LiveList,
    };
    let CallResult::Query {
        result: QueryResult::LiveList { gestures },
    } = block_on(control.call(call)).expect("live.list").result
    else {
        panic!("not live.list");
    };
    assert_eq!(
        gestures.len(),
        2,
        "each holds its own parameters: {gestures:?}"
    );

    // On the parent the keyframe's live values don't show.
    switch(&control, "a", None);
    assert_eq!(drawn("a"), [("a1".into(), 150.0), ("a2".into(), 550.0)]);

    // Committing writes the keyframe's override; the parent's gesture stays open.
    let commit =
        Batch::single(geometry("a2", Some("k"), 950.0)).ending_gesture(GestureId::new("key"));
    block_on(control.call(Call::Batch(commit))).expect("commit");
    switch(&control, "k", None);
    assert_eq!(drawn("k"), [("a1".into(), 150.0), ("a2".into(), 950.0)]);

    // A keyframe that goes takes the live values addressed to it.
    live_set("key2", vec![geometry("a1", Some("k"), 180.0)]);
    switch(&control, "a", None);
    apply(
        &control,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("k"),
        }],
    );
    let live = engine.render_hub().load_live();
    assert_eq!(
        live.item(&ItemId::new("a1"))
            .map(|(placement, _)| placement.map(|p| x(&p))),
        Some(Some(150.0)),
        "the parent's own value stays"
    );
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    switch(&control, "k", None);
    assert_eq!(drawn("k"), [("a1".into(), 150.0), ("a2".into(), 950.0)]);
}

/// `family` with the group `g` placed in `a` over the whole canvas by `ag` (source
/// `sg`), holding `g1` (red) at x 300.
fn family_with_group(engine: &Engine) {
    let control = connected(engine);
    apply(
        &control,
        vec![
            Edit::SceneCreate {
                scene_id: SceneId::new("g"),
                name: "G".into(),
                role: Some(jstream_protocol::SceneRole::Group),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("sg"),
                name: "G".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("g"),
                },
                settings: Settings::default(),
                duplicated_from: None,
            },
            add("g", "g1", "red"),
            geometry("g1", None, 300.0),
            add("a", "ag", "sg"),
        ],
    );
}

/// A keyframe scene draws a group in its parent with its overrides of the group's
/// children, through a view of the group, while the parent draws the group as it is
/// (#846). The view follows the group's own edits to fields it doesn't override.
#[test]
fn a_keyframe_scene_draws_a_groups_children_with_its_overrides() {
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    apply(&control, vec![geometry("g1", Some("k"), 800.0)]);
    let view = crate::snapshot::group_view(&SceneId::new("g"), &SceneId::new("k"));
    switch(&control, "k", None);
    assert_eq!(placed(&engine, view.as_str()), [("g1".into(), 800.0, 1.0)]);
    let k = engine
        .render_hub()
        .load()
        .shot()
        .placed(&SceneId::new("k"))
        .expect("k is drawn");
    assert!(
        k.iter().any(|(item, ..)| item.as_str() == "ag"),
        "k places the group"
    );
    // An opacity the keyframe doesn't override follows the group.
    apply(
        &control,
        vec![Edit::ItemSetOpacity {
            item_id: ItemId::new("g1"),
            scene_id: None,
            opacity: Finite32::new(0.5).expect("finite"),
        }],
    );
    assert_eq!(placed(&engine, view.as_str()), [("g1".into(), 800.0, 0.5)]);
    // A live value addressed to the keyframe shows in the view.
    let action = Action::LiveSet {
        gesture_id: GestureId::new("drag"),
        values: vec![geometry("g1", Some("k"), 850.0)],
    };
    block_on(control.call(Call::Action { action })).expect("live.set");
    let hub = engine.render_hub();
    let drawn = hub
        .load()
        .shot()
        .drawn(&view, &hub.load_live())
        .expect("the view is drawn");
    assert_eq!(x(&drawn[0].1), 850.0);
    let action = Action::LiveCancel {
        gesture_id: GestureId::new("drag"),
    };
    block_on(control.call(Call::Action { action })).expect("live.cancel");
    switch(&control, "a", None);
    assert_eq!(placed(&engine, "g"), [("g1".into(), 300.0, 0.5)]);
    // Clearing the last override of the group's children draws the group itself.
    apply(
        &control,
        vec![Edit::ItemClearOverride {
            scene_id: SceneId::new("k"),
            item_id: ItemId::new("g1"),
            field: jstream_protocol::OverrideField::Geometry,
        }],
    );
    switch(&control, "k", None);
    assert_eq!(placed(&engine, "g"), [("g1".into(), 300.0, 0.5)]);
}

/// A Move between a parent and its keyframe scene pairs a group's child by item and
/// moves it from the parent's value to the keyframe's, the group being drawn as its
/// items (ADR-0049 §3).
#[test]
fn a_move_within_a_family_moves_a_groups_children() {
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    apply(
        &control,
        vec![
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("move"),
                name: "Move".into(),
                kind: "move".into(),
                duration_ms: 400,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
            geometry("g1", Some("k"), 800.0),
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let tracks = engine
        .render_hub()
        .load()
        .shot()
        .tracks()
        .expect("a transition runs");
    let moves: Vec<(f32, f32)> = tracks
        .iter()
        .map(|track| match track {
            Track::Pair { from, to, .. } => {
                (from.geometry.center.x.get(), to.geometry.center.x.get())
            }
            other => panic!("not a pair: {other:?}"),
        })
        .collect();
    assert_eq!(moves, [(100.0, 100.0), (500.0, 500.0), (300.0, 800.0)]);
}
