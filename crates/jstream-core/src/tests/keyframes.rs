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

/// Each moving child's ID, its x at both ends and the track's duration.
fn moving(engine: &Engine) -> Vec<(String, f32, f32, f32)> {
    engine
        .render_hub()
        .load()
        .shot()
        .moving()
        .into_iter()
        .map(|(item, track)| match track {
            Track::Pair {
                from, to, timing, ..
            } => (
                item.to_string(),
                from.geometry.center.x.get(),
                to.geometry.center.x.get(),
                timing.duration_ms,
            ),
            other => panic!("not a pair: {other:?}"),
        })
        .collect()
}

/// A Move within a family moves a group drawn as one image (placed in a box, so not
/// flattened) as one item, and its children inside its layer from their values on the
/// outgoing side to those on the incoming side, with the group item's progress
/// (ADR-0049 §3): whether both sides override a child, only one does, or one side is
/// the parent, which draws the group as it is. A switch mid-Move starts the children
/// from where they are.
#[test]
fn a_move_within_a_family_moves_an_unflattened_groups_children() {
    use jstream_media::Timebase;
    use jstream_platform::fake::FakeClock;
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(std::sync::Arc::new(clock.clone()));
    let control = connected(&engine);
    apply(
        &control,
        vec![
            create("a"),
            colour("red"),
            add("a", "a1", "red"),
            geometry("a1", None, 100.0),
            variant("k", "a"),
            variant("k2", "a"),
        ],
    );
    assert!(engine.settle_sources(), "the sources were built in time");
    family_with_group(&engine);
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
            // The group in a box of its own: it moves as one image.
            geometry("ag", None, 960.0),
            add("g", "g2", "red"),
            geometry("g2", None, 400.0),
            geometry("g1", Some("k"), 800.0),
            geometry("g2", Some("k"), 600.0),
            geometry("g1", Some("k2"), 200.0),
        ],
    );
    switch(&control, "a", None);

    // From the parent, which draws the group as it is, to a keyframe that overrides
    // both children.
    switch(&control, "k", Some("move"));
    assert_eq!(
        moving(&engine),
        [
            ("g1".into(), 300.0, 800.0, 400.0),
            ("g2".into(), 400.0, 600.0, 400.0)
        ]
    );
    clock.advance(400_000);
    engine.replan();
    assert!(moving(&engine).is_empty(), "the Move ended");

    // Between two keyframes: g1 from one override to the other, and g2, which only
    // `k` overrides, from `k`'s value back to the group's.
    switch(&control, "k2", Some("move"));
    assert_eq!(
        moving(&engine),
        [
            ("g1".into(), 800.0, 200.0, 400.0),
            ("g2".into(), 600.0, 400.0, 400.0)
        ]
    );
    let tracks = engine
        .render_hub()
        .load()
        .shot()
        .tracks()
        .expect("a transition runs");
    assert!(
        tracks
            .iter()
            .all(|track| matches!(track, Track::Pair { .. })),
        "the group pairs as one item: {tracks:?}"
    );

    // Back to `k` half-way: the children start from where they are.
    clock.advance(200_000);
    switch(&control, "k", Some("move"));
    assert_eq!(
        moving(&engine),
        [
            ("g1".into(), 500.0, 800.0, 400.0),
            ("g2".into(), 500.0, 600.0, 400.0)
        ]
    );
}

/// The `move` preset, 400 ms and linear.
fn move_preset() -> Edit {
    Edit::TransitionCreate {
        preset_id: TransitionPresetId::new("move"),
        name: "Move".into(),
        kind: "move".into(),
        duration_ms: 400,
        easing: Easing::Linear,
        settings: Settings::default(),
    }
}

/// A `sharpen` on `owner`'s video chain, as filter `id`.
fn sharpen(owner: jstream_protocol::FilterOwner, id: &str) -> Edit {
    Edit::FilterAdd {
        owner,
        chain: jstream_protocol::FilterChain::Video,
        filter_id: jstream_protocol::FilterId::new(id),
        kind: "sharpen".into(),
        settings: Settings::default(),
        enabled: None,
        index: None,
    }
}

/// A group in a nested group moves inside it too (ADR-0049 §3): a Move between two
/// members of a family moves the outer group's children inside its layer, and the
/// inner group's children inside the inner group's, with the outer group item's
/// progress.
#[test]
fn a_move_within_a_family_moves_a_nested_groups_children() {
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            // `g` in a box, so it moves as one image, holding the group `h` over the
            // whole of it, which holds `h1`.
            geometry("ag", None, 960.0),
            Edit::SceneCreate {
                scene_id: SceneId::new("h"),
                name: "H".into(),
                role: Some(jstream_protocol::SceneRole::Group),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("sh"),
                name: "H".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("h"),
                },
                settings: Settings::default(),
                duplicated_from: None,
            },
            add("h", "h1", "red"),
            geometry("h1", None, 200.0),
            add("g", "gh", "sh"),
            geometry("g1", Some("k"), 800.0),
            geometry("h1", Some("k"), 700.0),
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let moves: Vec<(String, f32, f32)> = moving(&engine)
        .into_iter()
        .map(|(item, from, to, duration)| {
            assert_eq!(duration, 400.0, "{item} follows the group item");
            (item, from, to)
        })
        .collect();
    assert_eq!(
        moves,
        [
            ("g1".into(), 300.0, 800.0),
            ("gh".into(), 960.0, 960.0),
            ("h1".into(), 200.0, 700.0),
        ],
        "g's children inside g, then h's inside h"
    );
}

/// A group drawn through its own scene chain and its item's effects, with a child that
/// has effects of its own, moves its children inside its layer and is drawn through
/// both again (ADR-0049 §3, ADR-0065). The layers each side planned for the group are
/// not shown, so their chains and boxes are left out of the shot and don't run each
/// frame: only the moving layer's chain and boxes are counted.
#[test]
fn a_move_within_a_family_moves_a_chained_groups_children_and_runs_only_whats_shown() {
    use jstream_protocol::FilterOwner;
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            sharpen(
                FilterOwner::Scene {
                    scene_id: SceneId::new("g"),
                },
                "grade",
            ),
            sharpen(
                FilterOwner::Item {
                    item_id: ItemId::new("ag"),
                },
                "outer",
            ),
            sharpen(
                FilterOwner::Item {
                    item_id: ItemId::new("g1"),
                },
                "inner",
            ),
            geometry("g1", Some("k"), 800.0),
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub();
    let shot = hub.load();
    let shot = shot.shot();
    assert_eq!(moving(&engine), [("g1".into(), 300.0, 800.0, 400.0)]);
    let tracks = shot.tracks().expect("a transition runs");
    assert!(
        tracks
            .iter()
            .all(|track| matches!(track, Track::Pair { .. })),
        "the group pairs as one item: {tracks:?}"
    );
    let view = crate::snapshot::group_view(&SceneId::new("g"), &SceneId::new("k"));
    assert!(
        shot.scene_chain(&SceneId::new("g")).is_none(),
        "the old side's"
    );
    assert!(shot.scene_chain(&view).is_none(), "the new side's");
    let ag = ItemId::new("ag");
    assert!(shot.item_chain(&SceneId::new("a"), &ag).is_none());
    assert!(shot.item_chain(&SceneId::new("k"), &ag).is_none());
    let cost = shot.chain_cost();
    assert_eq!(
        (cost.scene_chains, cost.item_effects),
        (1, 2),
        "the moving layer's chain, its box, and g1's: {cost:?}"
    );
}

/// A child only one side of a Move within a family draws appears or disappears inside
/// the group's layer with the group item's timing, not its own (ADR-0049 §3).
#[test]
fn a_child_appears_and_disappears_inside_a_group_with_its_timing() {
    use jstream_protocol::ItemTransition;
    use jstream_render::transition::Timing;
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    let delayed = |item: &str, delay_ms: u32| Edit::ItemSetTransition {
        item_id: ItemId::new(item),
        transition: ItemTransition {
            delay_ms,
            ..ItemTransition::default()
        },
    };
    apply(
        &control,
        vec![
            move_preset(),
            geometry("ag", None, 960.0),
            add("g", "g2", "red"),
            geometry("g2", None, 400.0),
            delayed("ag", 100),
            delayed("g2", 50),
            geometry("g1", Some("k"), 800.0),
            Edit::ItemSetVisible {
                item_id: ItemId::new("g2"),
                scene_id: Some(SceneId::new("k")),
                visible: false,
            },
        ],
    );
    let group = Timing {
        delay_ms: 100.0,
        duration_ms: 400.0,
        easing: Easing::Linear,
    };
    let inside = |engine: &Engine| -> Vec<(String, &'static str, Timing)> {
        engine
            .render_hub()
            .load()
            .shot()
            .moving()
            .into_iter()
            .map(|(item, track)| {
                let (kind, timing) = match track {
                    Track::Pair { timing, .. } => ("pair", timing),
                    Track::Appear { timing, .. } => ("appear", timing),
                    Track::Disappear { timing, .. } => ("disappear", timing),
                };
                (item.to_string(), kind, timing)
            })
            .collect()
    };
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    assert_eq!(
        inside(&engine),
        [
            ("g2".into(), "disappear", group),
            ("g1".into(), "pair", group)
        ],
        "a leaving child is drawn below the arriving ones"
    );
    // Cut back to the parent and on to `k`, then Move from `k` to the parent.
    switch(&control, "a", None);
    switch(&control, "k", None);
    switch(&control, "a", Some("move"));
    assert_eq!(
        inside(&engine),
        [("g1".into(), "pair", group), ("g2".into(), "appear", group)]
    );
}
