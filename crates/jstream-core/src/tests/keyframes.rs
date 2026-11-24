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
        via: Vec::new(),
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
        Placement::Legacy(_) | Placement::Layer(_) => panic!("not a geometry placement"),
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
        motion: Default::default(),
        timing: None,
        transform: None,
        item_id: ItemId::new("a1"),
        geometry: None,
        crop: None,
        visible: None,
        opacity: Some(opacity),
        effects: None,
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
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    apply(&control, chained_group());
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
    let cost = shot.chain_cost([]);
    assert_eq!(
        (cost.scene_chains, cost.item_effects),
        (1, 2),
        "the moving layer's chain, its box, and g1's: {cost:?}"
    );
}

/// What a scene mix shows is planned in full though the Move above dropped it from the
/// program's tracks (#961): a mix showing the group's scene `g` gets `g` with its chain
/// and `g1`'s box, and a mix showing `k` gets `k` with `ag`'s box, while the Move
/// still runs.
#[test]
fn a_scene_mix_plans_again_what_the_move_dropped() {
    use jstream_protocol::{MixInput, MixResolution, VideoMix, VideoMixId};
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    let mix = |id: &str, scene: &str| Edit::VideoMixCreate {
        mix_id: VideoMixId::new(id),
        name: id.into(),
        mix: VideoMix {
            input: MixInput::Scene {
                scene_id: SceneId::new(scene),
            },
            resolution: MixResolution {
                width: 64,
                height: 36,
            },
            fps_divisor: 1,
        },
    };
    let mut list = chained_group();
    list.extend([mix("gm", "g"), mix("km", "k")]);
    apply(&control, list);
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub();
    let shot = hub.load();
    let shot = shot.shot();
    assert!(shot.tracks().is_some(), "a transition runs");
    assert_eq!(moving(&engine), [("g1".into(), 300.0, 800.0, 400.0)]);
    let (g, k) = (SceneId::new("g"), SceneId::new("k"));
    assert_eq!(shot.root_of(Some(&VideoMixId::new("gm"))), Some(&g));
    assert_eq!(shot.root_of(Some(&VideoMixId::new("km"))), Some(&k));
    assert!(shot.scene_chain(&g).is_some(), "gm's scene runs its chain");
    assert!(
        shot.item_chain(&g, &ItemId::new("g1")).is_some(),
        "gm's scene runs g1's effects"
    );
    assert!(
        shot.item_chain(&k, &ItemId::new("ag")).is_some(),
        "km's scene runs ag's effects"
    );
}

/// The edits that give the group `g` a scene chain, its item `ag` and its child `g1`
/// effects, and `g1` another place in `k`, with the Move preset.
fn chained_group() -> Vec<Edit> {
    use jstream_protocol::FilterOwner;
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
    ]
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
                    Track::Through { enter, .. } => ("through", enter),
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

/// A Move between two members of a family pairs an item only where its effects look the
/// same on both sides (ADR-0088, ADR-0081): `a1`, whose effects keyframe `k` overrides,
/// disappears and appears; `a2`, which follows the item's own on both sides, pairs.
#[test]
fn a_move_within_a_family_pairs_an_item_only_with_the_same_effects() {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            Edit::ItemSetEffects {
                scene_id: SceneId::new("k"),
                item_id: ItemId::new("a1"),
                effects: vec![jstream_protocol::FilterInfo {
                    filter_id: jstream_protocol::FilterId::new("sharp"),
                    kind: "sharpen".into(),
                    enabled: true,
                    settings: Settings::default(),
                }],
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
    let kinds: Vec<&str> = tracks
        .iter()
        .map(|track| match track {
            Track::Pair { .. } => "pair",
            Track::Appear { .. } => "appear",
            Track::Disappear { .. } => "disappear",
            Track::Through { .. } => "through",
        })
        .collect();
    let pairs = kinds.iter().filter(|k| **k == "pair").count();
    assert_eq!(pairs, 1, "only a2 pairs: {kinds:?}");
    assert!(
        kinds.contains(&"appear") && kinds.contains(&"disappear"),
        "{kinds:?}"
    );
}

/// A Move between two keyframes whose effects differ only in their settings pairs the
/// item and blends each number with its progress (ADR-0127): a `blur` radius and a
/// `color_correction` value glide instead of the item cross-fading.
#[test]
fn a_move_blends_effect_settings_that_differ() {
    use jstream_protocol::{FilterChain, FilterId, FilterInfo, FilterOwner};
    let engine = family();
    let control = connected(&engine);
    let effect = |id: &str, kind: &str, settings: serde_json::Value| FilterInfo {
        filter_id: FilterId::new(id),
        kind: kind.into(),
        enabled: true,
        settings: serde_json::from_value(settings).expect("settings"),
    };
    let own = |id: &str, kind: &str, settings: serde_json::Value| Edit::FilterAdd {
        owner: FilterOwner::Item {
            item_id: ItemId::new("a1"),
        },
        chain: FilterChain::Video,
        filter_id: FilterId::new(id),
        kind: kind.into(),
        settings: serde_json::from_value(settings).expect("settings"),
        enabled: None,
        index: None,
    };
    apply(
        &control,
        vec![
            move_preset(),
            own("soft", "blur", serde_json::json!({"radius": 2.0})),
            own(
                "grade",
                "color_correction",
                serde_json::json!({"brightness": 0.0}),
            ),
            Edit::ItemSetEffects {
                scene_id: SceneId::new("k"),
                item_id: ItemId::new("a1"),
                effects: vec![
                    effect("soft", "blur", serde_json::json!({"radius": 10.0})),
                    effect(
                        "grade",
                        "color_correction",
                        serde_json::json!({"brightness": 0.4}),
                    ),
                ],
            },
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub().load();
    let shot = hub.shot();
    let tracks = shot.tracks().expect("a transition runs");
    assert!(
        tracks
            .iter()
            .all(|track| matches!(track, Track::Pair { .. })),
        "every item pairs: {tracks:?}"
    );
    let number = |settings: &Settings, id: &str| {
        settings
            .get(id)
            .and_then(serde_json::Value::as_f64)
            .expect("a number")
    };
    // Linear over 400 ms: halfway at 200.
    let blended = shot.blended_effects(200.0).expect("a transition runs");
    let [(owner, settings)] = blended.as_slice() else {
        panic!("a1 alone blends: {blended:?}");
    };
    assert_eq!(
        *owner,
        crate::chain::ChainOwner::Item(SceneId::new("k"), ItemId::new("a1"))
    );
    assert!(
        (number(&settings[0], "radius") - 6.0).abs() < 1e-4,
        "{settings:?}"
    );
    assert!(
        (number(&settings[1], "brightness") - 0.2).abs() < 1e-4,
        "{settings:?}"
    );
    let ended = shot.blended_effects(400.0).expect("a transition runs");
    assert!((number(&ended[0].1[0], "radius") - 10.0).abs() < 1e-4);
}

/// While a Move blends an effect's settings (ADR-0127), the item's layer holds what it
/// draws at either look: a glow whose radius shrinks from 80 to 4 is padded for 80 for
/// the whole Move, though its own settings need 16 (ADR-0136).
#[test]
fn a_move_pads_the_layer_for_the_look_it_leaves() {
    use jstream_protocol::{FilterChain, FilterId, FilterInfo, FilterOwner};
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            Edit::FilterAdd {
                owner: FilterOwner::Item {
                    item_id: ItemId::new("a1"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("halo"),
                kind: "glow".into(),
                settings: serde_json::from_value(serde_json::json!({"radius": 80.0}))
                    .expect("settings"),
                enabled: None,
                index: None,
            },
            Edit::ItemSetEffects {
                scene_id: SceneId::new("k"),
                item_id: ItemId::new("a1"),
                effects: vec![FilterInfo {
                    filter_id: FilterId::new("halo"),
                    kind: "glow".into(),
                    enabled: true,
                    settings: serde_json::from_value(serde_json::json!({"radius": 4.0}))
                        .expect("settings"),
                }],
            },
        ],
    );
    switch(&control, "k", None);
    let pad = |engine: &Engine| {
        let hub = engine.render_hub().load();
        hub.shot()
            .item_chain(&SceneId::new("k"), &ItemId::new("a1"))
            .and_then(|chain| chain.pad)
    };
    assert_eq!(pad(&engine), Some((0, 16)), "the glow alone, at radius 4");
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    assert_eq!(pad(&engine), Some((0, 80)), "the Move leaves radius 80");
}

/// An undo of one client's step on a keyframe's effects is refused once another client
/// has changed those effects since (ADR-0032, ADR-0088 §4): the copy's undo would clear
/// the override, and the other client's effect with it.
#[test]
fn undoing_a_keyframe_effects_step_conflicts_with_a_later_edit_there() {
    use jstream_protocol::{FilterChain, FilterId, FilterOwner};
    let engine = family();
    let user = connected(&engine);
    let agent = connected(&engine);
    let sharpen = |owner: FilterOwner, id: &str| Edit::FilterAdd {
        owner,
        chain: FilterChain::Video,
        filter_id: FilterId::new(id),
        kind: "sharpen".into(),
        settings: Settings::default(),
        enabled: None,
        index: None,
    };
    let keyframe = FilterOwner::Keyframe {
        scene_id: SceneId::new("k"),
        item_id: ItemId::new("a1"),
    };
    apply(
        &user,
        vec![sharpen(
            FilterOwner::Item {
                item_id: ItemId::new("a1"),
            },
            "own",
        )],
    );
    // The user's edit copies a1's effects into k; the agent adds one to the copy.
    apply(
        &user,
        vec![Edit::FilterSetEnabled {
            owner: keyframe.clone(),
            filter_id: FilterId::new("own"),
            enabled: false,
        }],
    );
    apply(&agent, vec![sharpen(keyframe, "more")]);
    let err = block_on(user.call(Call::History(HistoryCall::new(HistoryOp::Undo))))
        .expect_err("the agent changed k's effects since");
    assert_eq!(super::code(&err), Some("undo_conflict"));
    let CallResult::Query {
        result: QueryResult::StateGet { scenes, .. },
    } = block_on(user.call(Call::Query {
        query: Query::StateGet,
    }))
    .expect("state.get")
    .result
    else {
        panic!("not state.get");
    };
    let k = scenes
        .into_iter()
        .find(|s| s.scene_id == SceneId::new("k"))
        .expect("k");
    let effects: Vec<String> = k
        .overrides
        .into_iter()
        .flat_map(|o| o.effects.unwrap_or_default())
        .map(|f| f.filter_id.to_string())
        .collect();
    assert_eq!(effects, ["own", "more"]);
}

/// A `scene` source's own video chain runs over the view a keyframe draws its group
/// through, and over the group itself where its parent draws it: one chain each
/// (ADR-0065 §4, #1018). A switch that retargets a running transition keeps the view
/// on the side it leaves.
#[test]
fn a_filtered_group_source_runs_over_each_view_and_keeps_it_on_a_retarget() {
    use crate::snapshot::GpuKey;
    let engine = family();
    family_with_group(&engine);
    let control = connected(&engine);
    apply(
        &control,
        vec![
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("fade"),
                name: "Fade".into(),
                kind: "fade".into(),
                // Long enough that the second switch below retargets it.
                duration_ms: 60_000,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
            sharpen(
                jstream_protocol::FilterOwner::Source {
                    source_id: SourceId::new("sg"),
                },
                "crisp",
            ),
            geometry("g1", Some("k"), 800.0),
        ],
    );
    let (sg, g) = (SourceId::new("sg"), SceneId::new("g"));
    let view = crate::snapshot::group_view(&g, &SceneId::new("k"));
    let input = |scene: &SceneId| {
        engine
            .render_hub()
            .load()
            .shot()
            .placed_chain(&sg, scene)
            .map(|chain| chain.input.clone())
    };
    switch(&control, "a", None);
    assert_eq!(input(&g), Some(GpuKey::Layer(g.clone(), 1)));
    assert_eq!(input(&view), None, "a draws the group itself");

    switch(&control, "k", Some("fade"));
    assert_eq!(input(&g), Some(GpuKey::Layer(g.clone(), 1)), "a, leaving");
    assert_eq!(
        input(&view),
        Some(GpuKey::Layer(view.clone(), 1)),
        "k, arriving"
    );

    // Back to `a` mid-fade: what is on screen, k's view among it, leaves.
    switch(&control, "a", Some("fade"));
    let shot = engine.render_hub().load();
    assert!(shot.shot().transition_tracks().is_some(), "a fade runs");
    assert_eq!(
        input(&view),
        Some(GpuKey::Layer(view.clone(), 1)),
        "k's view, leaving"
    );
    assert_eq!(input(&g), Some(GpuKey::Layer(g, 1)), "a, arriving");
}

// Motion modifiers (ADR-0099): drawn each frame on top of an item's values, a keyframe
// scene's override in place of the item's own, blended across a Move.

fn secs(s: f64) -> jstream_media::MediaTime {
    jstream_media::MediaTime::new((s * 1e6) as i64, jstream_media::Timebase::MICROS)
}

/// `item`'s motion: one drift along x at `rate` pixels a second, on `clock`.
fn drift_x(
    item: &str,
    scene: Option<&str>,
    rate: f32,
    clock: jstream_protocol::MotionClock,
) -> Edit {
    Edit::ItemSetMotion {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        motion: vec![jstream_protocol::MotionModifier {
            modifier_id: "drift".into(),
            property: jstream_protocol::MotionProperty::X,
            effect: None,
            source: None,
            kind: jstream_protocol::MotionKind::Drift {
                rate_per_s: Finite32::new(rate).expect("finite"),
                wrap: None,
            },
            clock,
            enabled: true,
            amount: None,
        }],
    }
}

/// Each item's x as the program pass of `scene` draws it at `at`, with its motion.
fn moved_x(engine: &Engine, scene: &str, at: jstream_media::MediaTime) -> Vec<(String, f32)> {
    engine
        .render_hub()
        .load()
        .shot()
        .moved_at(&SceneId::new(scene), at)
        .expect("the program draws the scene")
        .iter()
        .map(|(item, placement, _, _)| (item.to_string(), x(placement)))
        .collect()
}

#[test]
fn an_items_motion_moves_it_each_frame_and_a_keyframe_overrides_it() {
    use jstream_protocol::MotionClock::Engine as Clock;
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    assert!(!engine.render_hub().load().shot().changes_with_time());
    apply(&control, vec![drift_x("a1", None, 100.0, Clock)]);
    let shot = engine.render_hub().load();
    assert!(shot.shot().changes_with_time(), "a moving item redraws");
    // The animation time, not the edit's: two seconds in, 200 px along.
    assert_eq!(
        moved_x(&engine, "a", secs(2.0)),
        [("a1".into(), 300.0), ("a2".into(), 500.0)]
    );
    // The keyframe's own motion replaces the item's there, and only there.
    apply(&control, vec![drift_x("a1", Some("k"), -50.0, Clock)]);
    switch(&control, "k", None);
    assert_eq!(
        moved_x(&engine, "k", secs(2.0)),
        [("a1".into(), 0.0), ("a2".into(), 500.0)]
    );
}

#[test]
fn a_move_blends_both_sides_motion_by_its_progress() {
    use jstream_protocol::MotionClock::Engine as Clock;
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
            drift_x("a1", None, 100.0, Clock),
            drift_x("a1", Some("k"), 300.0, Clock),
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let shot = engine.render_hub().load();
    let x_of = |elapsed_ms: f32| {
        shot.shot()
            .tracks_moved_at(elapsed_ms, secs(1.0))
            .expect("a transition runs")
            .iter()
            .map(|end| end.map(|end| end.geometry.center.x.get()))
            .collect::<Vec<_>>()
    };
    // a1 moves 100 px a second on `a` and 300 on `k`: halfway, it adds 200.
    assert_eq!(x_of(0.0), [Some(200.0), Some(500.0)]);
    assert_eq!(x_of(200.0), [Some(300.0), Some(500.0)]);
    assert_eq!(x_of(400.0), [Some(400.0), Some(500.0)]);
}

#[test]
fn the_shown_clock_starts_when_the_item_is_first_drawn() {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![drift_x(
            "a1",
            None,
            100.0,
            jstream_protocol::MotionClock::Shown,
        )],
    );
    switch(&control, "a", None);
    let shown = *engine
        .render_hub()
        .load()
        .shot()
        .shown()
        .get(&ItemId::new("a1"))
        .expect("a1 is drawn");
    let one_second_later = secs(shown.as_secs_f64() + 1.0);
    // The clock is the engine's, so the time a second after `shown` is only as exact
    // as that sum in seconds.
    let (item, x) = moved_x(&engine, "a", one_second_later)[0].clone();
    assert_eq!(item, "a1");
    assert!((x - 200.0).abs() < 0.01, "{x}");
}

/// An item of a scene with a video chain, which a chain phase's pass draws, is drawn
/// too: its `shown` clock runs.
#[test]
fn the_shown_clock_runs_in_a_scene_with_a_chain() {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            drift_x("a1", None, 100.0, jstream_protocol::MotionClock::Shown),
            sharpen(
                jstream_protocol::FilterOwner::Scene {
                    scene_id: SceneId::new("a"),
                },
                "look",
            ),
        ],
    );
    switch(&control, "a", None);
    assert!(
        engine
            .render_hub()
            .load()
            .shot()
            .shown()
            .contains_key(&ItemId::new("a1")),
        "a1 is drawn"
    );
}

#[test]
fn the_shown_clock_runs_for_an_item_with_effects() {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            drift_x("a1", None, 100.0, jstream_protocol::MotionClock::Shown),
            sharpen(
                jstream_protocol::FilterOwner::Item {
                    item_id: ItemId::new("a1"),
                },
                "look",
            ),
        ],
    );
    switch(&control, "a", None);
    assert!(
        engine
            .render_hub()
            .load()
            .shot()
            .shown()
            .contains_key(&ItemId::new("a1")),
        "a1 is drawn"
    );
}

// Audio-reactive motion modifiers (ADR-0100): an item's opacity follows the program
// mix's level through the render thread's follower.

/// `item`'s motion: its opacity follows the program mix's level, dark at silence and
/// full at -6 dBFS, with no attack.
fn pulse(item: &str, scene: Option<&str>) -> Edit {
    let f = |v: f32| Finite32::new(v).expect("finite");
    Edit::ItemSetMotion {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        motion: vec![jstream_protocol::MotionModifier {
            modifier_id: "pulse".into(),
            property: jstream_protocol::MotionProperty::Opacity,
            effect: None,
            source: None,
            kind: jstream_protocol::MotionKind::Audio {
                audio: None,
                audio_source: None,
                measure: jstream_protocol::MotionMeasure::Level,
                low_hz: None,
                high_hz: None,
                floor_db: f(-60.0),
                ceiling_db: f(-6.0),
                attack_ms: 0,
                release_ms: 200,
                curve: jstream_protocol::MotionCurve::Linear,
                from: f(-1.0),
                to: Finite32::ZERO,
            },
            clock: jstream_protocol::MotionClock::Engine,
            enabled: true,
            amount: None,
        }],
    }
}

/// Each item's opacity as the program pass of `scene` draws it at `at`.
fn moved_opacity(engine: &Engine, scene: &str, at: jstream_media::MediaTime) -> Vec<f32> {
    engine
        .render_hub()
        .load()
        .shot()
        .moved_at(&SceneId::new(scene), at)
        .expect("the program draws the scene")
        .iter()
        .map(|(_, _, _, opacity)| *opacity)
        .collect()
}

#[test]
fn an_audio_modifier_follows_the_program_mix_and_reads_silence_until_heard() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    apply(&control, vec![pulse("a1", None)]);
    let snapshot = engine.render_hub().load();
    let shot = snapshot.shot();
    assert!(shot.changes_with_time(), "an audio modifier redraws");
    let a1 = ItemId::new("a1");
    assert_eq!(
        shot.audio().level(&a1, "pulse"),
        Some(0.0),
        "bound to the program mix"
    );
    // Before any tick follows it, it reads silence: its `from`, dark.
    assert_eq!(moved_opacity(&engine, "a", secs(1.0)), [0.0, 1.0]);

    // A full-scale sine on the program mix: an RMS of -3 dBFS, over the ceiling.
    let samples: Vec<f32> = (0..2048)
        .flat_map(|n| {
            let s = (std::f32::consts::TAU * 440.0 * n as f32 / 48_000.0).sin();
            [s, s]
        })
        .collect();
    engine.audio_scope().push(48_000, &samples);
    let mut follower = crate::reactive::Follower::default();
    follower.follow(shot.audio(), secs(1.0));
    follower.follow(shot.audio(), secs(1.016));
    assert_eq!(
        moved_opacity(&engine, "a", secs(1.016)),
        [1.0, 1.0],
        "full on"
    );

    // The keyframe's motion overrides the item's, and binds the same way.
    apply(&control, vec![pulse("a2", Some("k"))]);
    switch(&control, "k", None);
    let snapshot = engine.render_hub().load();
    assert!(
        snapshot
            .shot()
            .audio()
            .level(&ItemId::new("a2"), "pulse")
            .is_some()
    );
    assert!(
        snapshot.shot().audio().level(&a1, "pulse").is_some(),
        "k shows a1's own"
    );
}

/// ADR-0171 decision 3: a `data` modifier poses an item from a sender's live data: the
/// transport's `playing` moves it, a preview reading it as it is.
#[test]
fn a_data_modifier_poses_an_item_from_live_data() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    let f = |v: f32| Finite32::new(v).expect("finite");
    apply(
        &control,
        vec![Edit::ItemSetMotion {
            item_id: ItemId::new("a1"),
            scene_id: None,
            motion: vec![jstream_protocol::MotionModifier {
                modifier_id: "play".into(),
                property: jstream_protocol::MotionProperty::X,
                effect: None,
                source: None,
                kind: jstream_protocol::MotionKind::Data {
                    input: None,
                    key: "transport".into(),
                    field: "playing".into(),
                    range: [f(0.0), f(1.0)],
                    states: std::collections::BTreeMap::new(),
                    smoothing_ms: 0,
                    curve: jstream_protocol::MotionCurve::Linear,
                    from: f(0.0),
                    to: f(100.0),
                    color_from: None,
                    palette: std::collections::BTreeMap::new(),
                },
                clock: jstream_protocol::MotionClock::Engine,
                enabled: true,
                amount: None,
            }],
        }],
    );
    let snapshot = engine.render_hub().load();
    let shot = snapshot.shot();
    assert!(shot.changes_with_time(), "a data modifier redraws");
    let x = |at| {
        moved_x(&engine, "a", at)
            .into_iter()
            .find(|(item, _)| item == "a1")
            .map(|(_, x)| x)
            .expect("a1 is drawn")
    };
    let rest = x(secs(1.0));
    let data = engine.live_data();
    data.put("DAW", "transport", br#"{"playing": true}"#, 0)
        .expect("taken");
    assert!((x(secs(1.0)) - (rest + 100.0)).abs() < 0.01, "playing");
    let mut follower = crate::reactive::Follower::default();
    follower.follow(shot.audio(), secs(1.0));
    assert!((x(secs(1.0)) - (rest + 100.0)).abs() < 0.01, "followed");
    data.put("DAW", "transport", br#"{"playing": false}"#, 10)
        .expect("taken");
    follower.follow(shot.audio(), secs(1.1));
    assert!((x(secs(1.1)) - rest).abs() < 0.01, "stopped, at once");
}

/// ADR-0168: a `midi` modifier poses an item from a controller: a preview reads the
/// control as it is, and the program's follower smooths it, landing on its first tick.
#[test]
fn a_midi_modifier_poses_an_item_from_a_controller() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    let f = |v: f32| Finite32::new(v).expect("finite");
    apply(
        &control,
        vec![Edit::ItemSetMotion {
            item_id: ItemId::new("a1"),
            scene_id: None,
            motion: vec![jstream_protocol::MotionModifier {
                modifier_id: "fader".into(),
                property: jstream_protocol::MotionProperty::X,
                effect: None,
                source: None,
                kind: jstream_protocol::MotionKind::Midi {
                    control: jstream_protocol::MotionMidiControl::Cc,
                    controller: Some(7),
                    channel: Some(1),
                    input: None,
                    smoothing_ms: 100,
                    curve: jstream_protocol::MotionCurve::Linear,
                    from: f(0.0),
                    to: f(127.0),
                },
                clock: jstream_protocol::MotionClock::Engine,
                enabled: true,
                amount: None,
            }],
        }],
    );
    let snapshot = engine.render_hub().load();
    let shot = snapshot.shot();
    assert!(shot.changes_with_time(), "a MIDI modifier redraws");
    let a1 = ItemId::new("a1");
    let x = |at| {
        moved_x(&engine, "a", at)
            .into_iter()
            .find(|(item, _)| item == "a1")
            .map(|(_, x)| x)
            .expect("a1 is drawn")
    };
    let rest = x(secs(1.0));
    assert_eq!(
        shot.audio().level(&a1, "fader"),
        Some(0.0),
        "nothing sent yet"
    );

    let midi = engine.midi_inputs();
    // Its MIDI may move things (ADR-0123 §3).
    midi.set_bindable(["Faders"].map(String::from));
    let cc = |value| jstream_midi::Message::parse(&[0xB0, 7, value]).expect("a CC");
    midi.push("Faders", 0, &cc(127));
    // No follower yet (as in a preview): the control as it is, unsmoothed.
    assert!((x(secs(1.0)) - (rest + 127.0)).abs() < 0.01);
    let mut follower = crate::reactive::Follower::default();
    follower.follow(shot.audio(), secs(1.0));
    assert!(
        (x(secs(1.0)) - (rest + 127.0)).abs() < 0.01,
        "the first tick lands"
    );
    midi.push("Faders", 1, &cc(0));
    follower.follow(shot.audio(), secs(1.1));
    // One time constant on, about 37% of the way back is left.
    let eased = x(secs(1.1)) - rest;
    assert!((eased - 127.0 * (-1f32).exp()).abs() < 0.5, "{eased}");
    // Another channel moves nothing.
    midi.push(
        "Faders",
        2,
        &jstream_midi::Message::parse(&[0xB1, 7, 127]).expect("a CC"),
    );
    follower.follow(shot.audio(), secs(2.0));
    assert!((x(secs(2.0)) - rest).abs() < 0.5, "settled at the bottom");
}

/// ADR-0173: with no output running, the pictures the control side draws (a preview, a
/// `video.snapshot`) follow the program's motion controls themselves, so a held clock's
/// steps smooth a MIDI modifier as an output's ticks would.
#[test]
fn a_preview_with_no_output_smooths_a_midi_modifier_on_the_clock() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    let f = |v: f32| Finite32::new(v).expect("finite");
    apply(
        &control,
        vec![Edit::ItemSetMotion {
            item_id: ItemId::new("a1"),
            scene_id: None,
            motion: vec![jstream_protocol::MotionModifier {
                modifier_id: "fader".into(),
                property: jstream_protocol::MotionProperty::X,
                effect: None,
                source: None,
                kind: jstream_protocol::MotionKind::Midi {
                    control: jstream_protocol::MotionMidiControl::Cc,
                    controller: Some(7),
                    channel: Some(1),
                    input: None,
                    smoothing_ms: 100,
                    curve: jstream_protocol::MotionCurve::Linear,
                    from: f(0.0),
                    to: f(127.0),
                },
                clock: jstream_protocol::MotionClock::Engine,
                enabled: true,
                amount: None,
            }],
        }],
    );
    let act = |action| {
        block_on(control.call(jstream_protocol::Call::Action { action })).expect("clock action");
    };
    act(jstream_protocol::Action::ClockHold);
    let a1 = ItemId::new("a1");
    let level = || {
        let current = engine.preview_current();
        current
            .program
            .shot()
            .audio()
            .level(&a1, "fader")
            .expect("bound")
    };
    assert!(
        level().abs() < 1e-6,
        "the first picture lands on the control, at rest"
    );

    let midi = engine.midi_inputs();
    // Its MIDI may move things (ADR-0123 §3).
    midi.set_bindable(["Faders".to_owned()]);
    midi.push(
        "Faders",
        0,
        &jstream_midi::Message::parse(&[0xB0, 7, 127]).expect("a CC"),
    );
    assert!(
        level().abs() < 1e-6,
        "a picture at the same instant moves nothing"
    );
    act(jstream_protocol::Action::ClockStep { ms: 50 });
    let stepped = level();
    assert!(
        stepped > 0.2 && stepped < 0.6,
        "one 50 ms step of 100 ms smoothing goes part way, not to the top: {stepped}"
    );
    act(jstream_protocol::Action::ClockStep { ms: 1_000 });
    assert!(level() > 0.99, "settled at the top");
}

/// An item whose effects run over the canvas (ADR-0106) moves inside its layer, which
/// stays flat over the canvas: in a switch its tracks, which carry that layer, don't
/// drift with its motion (#1125 review B8, N16).
#[test]
fn a_canvas_space_items_motion_leaves_its_layer_over_the_canvas() {
    use jstream_protocol::MotionClock::Engine as Clock;
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Item {
                    item_id: ItemId::new("a1"),
                },
                chain: jstream_protocol::FilterChain::Video,
                filter_id: jstream_protocol::FilterId::new("sharp"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
            Edit::ItemSetEffectsSpace {
                item_id: ItemId::new("a1"),
                space: jstream_protocol::EffectsSpace::Canvas,
            },
            drift_x("a1", None, 100.0, Clock),
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let shot = engine.render_hub().load();
    let xs = |at: f64| {
        shot.shot()
            .tracks_moved_at(200.0, secs(at))
            .expect("a transition runs")
            .iter()
            .map(|end| end.map(|end| end.geometry.center.x.get()))
            .collect::<Vec<_>>()
    };
    assert!(xs(1.0).iter().any(Option::is_some), "{:?}", xs(1.0));
    assert_eq!(xs(1.0), xs(3.0), "the canvas layers stay where they are");
}

// Triggered motion (#1107): still until `item.trigger_motion` fires it, then from then.

fn trigger(
    control: &dyn Control,
    item: &str,
    modifier: Option<&str>,
) -> Result<(), jstream_protocol::ProtocolError> {
    let action = Action::ItemTriggerMotion {
        item_id: ItemId::new(item),
        modifier_id: modifier.map(Into::into),
    };
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|e| match e {
            jstream_control::ControlError::Protocol(wire) => wire.error,
            other => panic!("not a protocol error: {other}"),
        })
}

/// `a1`'s motion: `jolt`, a 500 ms impulse of 40 px along y at 2 Hz that doesn't decay,
/// and `drift`, 100 px a second along x, both on the `triggered` clock.
fn jolt_and_drift() -> Edit {
    use jstream_protocol::{MotionClock, MotionKind, MotionModifier, MotionProperty};
    let f = |v: f32| Finite32::new(v).expect("finite");
    Edit::ItemSetMotion {
        item_id: ItemId::new("a1"),
        scene_id: None,
        motion: vec![
            MotionModifier {
                modifier_id: "jolt".into(),
                property: MotionProperty::Y,
                effect: None,
                source: None,
                kind: MotionKind::Impulse {
                    amplitude: f(40.0),
                    frequency_hz: f(2.0),
                    decay_per_s: f(0.0),
                    duration_ms: 500,
                },
                clock: MotionClock::Triggered,
                enabled: true,
                amount: None,
            },
            MotionModifier {
                modifier_id: "drift".into(),
                property: MotionProperty::X,
                effect: None,
                source: None,
                kind: MotionKind::Drift {
                    rate_per_s: f(100.0),
                    wrap: None,
                },
                clock: MotionClock::Triggered,
                enabled: true,
                amount: None,
            },
        ],
    }
}

/// `a1`'s centre as the program draws it `after` seconds past when `id` was fired, or
/// at `after` seconds on the engine's clock if it never was.
fn a1_after(engine: &Engine, id: &str, after: f64) -> (f32, f32) {
    let hub = engine.render_hub().load();
    let shot = hub.shot();
    let origin = shot
        .fired_at(&ItemId::new("a1"), id)
        .map_or(0.0, |at| at.as_secs_f64());
    let moved = shot
        .moved_at(&SceneId::new("a"), secs(origin + after))
        .expect("the program draws a");
    let (_, placement, _, _) = moved
        .iter()
        .find(|(item, ..)| item.as_str() == "a1")
        .expect("a1");
    let jstream_render::Placement::Geometry(g) = placement else {
        panic!("a1 has a box");
    };
    (g.center.x.get(), g.center.y.get())
}

#[test]
fn a_triggered_modifier_is_still_until_fired_then_runs_from_the_fire() {
    let engine = family();
    let control = connected(&engine);
    apply(&control, vec![jolt_and_drift()]);
    switch(&control, "a", None);
    let (_, y0) = a1_after(&engine, "jolt", 0.0);
    assert_eq!(
        a1_after(&engine, "jolt", 5.0),
        (100.0, y0),
        "never fired: still"
    );

    // Firing one moves only that one, from the fire: a quarter of a 2 Hz swing is its
    // peak, and past its 500 ms it is still again.
    trigger(&control, "a1", Some("jolt")).expect("fires");
    let near = |(x, y): (f32, f32), (ex, ey): (f32, f32)| {
        assert!(
            (x - ex).abs() < 0.01 && (y - ey).abs() < 0.01,
            "({x}, {y}) is not ({ex}, {ey})"
        );
    };
    near(a1_after(&engine, "jolt", 0.125), (100.0, y0 + 40.0));
    near(a1_after(&engine, "jolt", 0.6), (100.0, y0));

    // Firing every one starts the drift too, and restarts the jolt.
    trigger(&control, "a1", None).expect("fires");
    near(a1_after(&engine, "drift", 0.125), (112.5, y0 + 40.0));
    near(a1_after(&engine, "drift", 2.0), (300.0, y0));
}

#[test]
fn a_fire_names_an_item_and_a_well_formed_modifier() {
    let engine = family();
    let control = connected(&engine);
    match trigger(&control, "nope", None) {
        Err(jstream_protocol::ProtocolError::NotFound { entity, id }) => {
            assert_eq!(
                (entity, id.as_str()),
                (jstream_protocol::EntityKind::Item, "nope")
            );
        }
        other => panic!("not a not_found: {other:?}"),
    }
    match trigger(&control, "a1", Some("has space")) {
        Err(jstream_protocol::ProtocolError::InvalidArgument { field, .. }) => {
            assert_eq!(field, "modifier_id")
        }
        other => panic!("not an invalid_argument: {other:?}"),
    }
    // A modifier the item doesn't hold is no error: a keyframe can override its list.
    trigger(&control, "a1", Some("elsewhere")).expect("fires nothing");
}

/// `item.set_timing` of `item` in keyframe `scene`: its delay, easing and duration in
/// moves into it (ADR-0129).
fn timing(
    scene: &str,
    item: &str,
    delay_ms: u32,
    easing: jstream_protocol::ItemEasing,
    duration_ms: Option<u32>,
) -> Edit {
    Edit::ItemSetTiming {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        timing: jstream_protocol::KeyframeTiming {
            delay_ms: Some(delay_ms),
            easing: Some(easing),
            duration_ms,
        },
    }
}

/// Each track's delay, duration and easing, in the shot's order.
fn timings(engine: &Engine) -> Vec<(f32, f32, Easing)> {
    let hub = engine.render_hub().load();
    hub.shot()
        .tracks()
        .expect("a transition runs")
        .iter()
        .map(|track| match track {
            Track::Pair { timing, .. } => (timing.delay_ms, timing.duration_ms, timing.easing),
            other => panic!("every item glides: {other:?}"),
        })
        .collect()
}

/// Two keyframes of one scene stagger its items in opposite orders (ADR-0129): a Move
/// into `k1` starts `a1` first and `a2` last, a Move into `k2` the other way round, and
/// each keyframe's easing and duration replace the item's and the preset's only in moves
/// into it. A Move back into the parent uses the items' own timing.
#[test]
fn two_keyframes_stagger_the_same_items_in_opposite_orders() {
    use jstream_protocol::ItemEasing;
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            variant("k2", "a"),
            geometry("a1", Some("k"), 300.0),
            geometry("a2", Some("k"), 700.0),
            geometry("a1", Some("k2"), 200.0),
            geometry("a2", Some("k2"), 600.0),
            timing("k", "a1", 0, ItemEasing::EaseIn, None),
            timing("k", "a2", 300, ItemEasing::EaseIn, Some(200)),
            timing("k2", "a1", 300, ItemEasing::Hold, Some(200)),
            timing("k2", "a2", 0, ItemEasing::Hold, None),
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    assert_eq!(
        timings(&engine),
        [(0.0, 400.0, Easing::EaseIn), (300.0, 200.0, Easing::EaseIn)]
    );
    switch(&control, "k2", Some("move"));
    assert_eq!(
        timings(&engine),
        [(300.0, 200.0, Easing::Hold), (0.0, 400.0, Easing::Hold)]
    );
    switch(&control, "a", Some("move"));
    assert_eq!(
        timings(&engine),
        [(0.0, 400.0, Easing::Linear), (0.0, 400.0, Easing::Linear)]
    );
}

/// `item.set_timing` is undone by the edit it returns, a timing with no field clears the
/// override, and the item's own scene or an out-of-range curve is refused.
#[test]
fn a_keyframe_timing_is_undone_cleared_and_checked() {
    use jstream_protocol::{Bezier, ItemEasing, KeyframeTiming};
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![timing("k", "a1", 120, ItemEasing::Hold, Some(50))],
    );
    let override_of =
        |control: &dyn Control| match super::query(control, jstream_protocol::Query::StateGet)
            .expect("state")
        {
            jstream_protocol::QueryResult::StateGet { scenes, .. } => scenes
                .into_iter()
                .find(|scene| scene.scene_id == SceneId::new("k"))
                .and_then(|scene| scene.overrides.into_iter().next())
                .and_then(|o| o.timing),
            other => panic!("not a state: {other:?}"),
        };
    let undo = |control: &dyn Control| {
        let call = jstream_protocol::HistoryCall::new(jstream_protocol::HistoryOp::Undo);
        block_on(control.call(Call::History(call))).expect("undoes");
    };
    let set = override_of(&control);
    assert_eq!(
        set.map(|t| (t.delay_ms, t.duration_ms)),
        Some((Some(120), Some(50)))
    );
    apply(
        &control,
        vec![Edit::ItemSetTiming {
            scene_id: SceneId::new("k"),
            item_id: ItemId::new("a1"),
            timing: KeyframeTiming::default(),
        }],
    );
    assert_eq!(override_of(&control), None, "an empty timing clears it");
    undo(&control);
    assert_eq!(override_of(&control), set, "undo writes it back");
    undo(&control);
    assert_eq!(override_of(&control), None, "undo clears what it set");
    let refused = |timing: KeyframeTiming, scene: &str| {
        block_on(control.call(Call::Batch(edits(vec![Edit::ItemSetTiming {
            scene_id: SceneId::new(scene),
            item_id: ItemId::new("a1"),
            timing,
        }]))))
        .err()
        .and_then(|error| error.protocol().cloned())
    };
    let own = refused(
        KeyframeTiming {
            delay_ms: Some(10),
            ..KeyframeTiming::default()
        },
        "a",
    );
    assert!(
        matches!(&own, Some(jstream_protocol::ProtocolError::InvalidArgument { field, .. }) if field.ends_with("scene_id")),
        "{own:?}"
    );
    let f = |v: f32| jstream_protocol::Finite32::new(v).expect("finite");
    let curve = refused(
        KeyframeTiming {
            easing: Some(ItemEasing::CubicBezier(Bezier {
                x1: f(1.5),
                y1: f(0.0),
                x2: f(0.5),
                y2: f(1.0),
            })),
            ..KeyframeTiming::default()
        },
        "k",
    );
    assert!(
        matches!(&curve, Some(jstream_protocol::ProtocolError::InvalidArgument { field, .. }) if field.ends_with("timing.easing.cubic_bezier.x1")),
        "{curve:?}"
    );
}

/// The framing camera for the default canvas, turned 20° about the canvas centre so it
/// looks down at it from above.
fn tilted_20() -> jstream_protocol::SceneCamera {
    let framing = jstream_protocol::SceneCamera::framing(jstream_protocol::Canvas::DEFAULT);
    let target = framing.target;
    let distance = framing.position.z.get() - target.z.get();
    let (sin, cos) = 20f32.to_radians().sin_cos();
    jstream_protocol::SceneCamera {
        position: jstream_protocol::Vec3 {
            x: target.x,
            y: Finite32::new(target.y.get() - distance * sin).expect("finite"),
            z: Finite32::new(target.z.get() + distance * cos).expect("finite"),
        },
        ..framing
    }
}

/// A Move from a 2D keyframe family's parent to its keyframe with a camera tilted 20°
/// glides (ADR-0128): every item pairs, and the tracks are seen through a camera moving
/// from the framing camera, which draws the 2D side as 2D does, to the tilted one.
#[test]
fn a_move_from_2d_to_a_tilted_keyframe_glides() {
    use jstream_protocol::{Canvas, SceneCamera};
    let engine = family();
    let control = connected(&engine);
    let tilted = tilted_20();
    apply(
        &control,
        vec![
            move_preset(),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some(tilted),
            },
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub().load();
    let shot = hub.shot();
    let tracks = shot.tracks().expect("a transition runs");
    assert_eq!(tracks.len(), 2);
    assert!(
        tracks
            .iter()
            .all(|track| matches!(track, Track::Pair { .. })),
        "every item glides: {tracks:?}"
    );
    let framing = SceneCamera::framing(Canvas::DEFAULT);
    let start = shot.track_cameras(0.0).expect("a transition runs");
    assert!(
        start.iter().all(|camera| *camera == Some(framing)),
        "{start:?}"
    );
    // Linear over 400 ms: halfway at 200.
    let half = jstream_render::transition::lerp_camera(&framing, &tilted, 0.5);
    let mid = shot.track_cameras(200.0).expect("a transition runs");
    assert!(mid.iter().all(|camera| *camera == Some(half)), "{mid:?}");
    let end = shot.track_cameras(400.0).expect("a transition runs");
    assert!(end.iter().all(|camera| *camera == Some(tilted)), "{end:?}");
    // And back: the Move out of 3D ends on the framing camera, then draws 2D.
    switch(&control, "a", Some("move"));
    let hub = engine.render_hub().load();
    let back = hub.shot();
    let tracks = back.tracks().expect("a transition runs");
    assert!(
        tracks
            .iter()
            .all(|track| matches!(track, Track::Pair { .. }))
    );
    let end = back.track_cameras(400.0).expect("a transition runs");
    assert!(end.iter().all(|camera| *camera == Some(framing)), "{end:?}");
}

/// A Move between 2D and 3D fades where the 2D side has an item the framing camera
/// can't draw as 2D does (ADR-0128): a tilted item with no perspective of its own.
#[test]
fn a_move_from_2d_with_a_tilted_item_into_3d_fades() {
    let engine = family();
    let control = connected(&engine);
    let mut tilted_item = at(100.0);
    tilted_item.tilt_deg = Finite32::new(30.0).expect("finite");
    apply(
        &control,
        vec![
            move_preset(),
            Edit::ItemSetGeometry {
                item_id: ItemId::new("a1"),
                scene_id: None,
                geometry: tilted_item,
            },
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some(tilted_20()),
            },
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub().load();
    let tracks = hub.shot().tracks().expect("a transition runs");
    assert!(
        !tracks
            .iter()
            .any(|track| matches!(track, Track::Pair { .. })),
        "it fades: {tracks:?}"
    );
}

/// A Move between 2D and 3D fades where the 2D side has a billboard (ADR-0151 decision
/// 2): flat in 2D, it would turn to face the framing camera and snap flat at the end.
#[test]
fn a_move_from_2d_with_a_billboard_into_3d_fades() {
    let engine = family();
    let control = connected(&engine);
    let mut billboard = at(100.0);
    billboard.billboard = true;
    apply(
        &control,
        vec![
            move_preset(),
            Edit::ItemSetGeometry {
                item_id: ItemId::new("a1"),
                scene_id: None,
                geometry: billboard,
            },
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some(tilted_20()),
            },
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub().load();
    let tracks = hub.shot().tracks().expect("a transition runs");
    assert!(
        !tracks
            .iter()
            .any(|track| matches!(track, Track::Pair { .. })),
        "it fades: {tracks:?}"
    );
}

/// An item off the canvas's centre with its own perspective at the framing camera's focal
/// length still fades (ADR-0128 §2): its perspective projects about its own centre, the
/// camera's about the canvas's, so gliding would make it jump on the first frame.
#[test]
fn a_move_from_2d_with_an_off_centre_item_at_the_framing_focal_length_fades() {
    use jstream_protocol::{Canvas, SceneCamera};
    let engine = family();
    let control = connected(&engine);
    let focal = SceneCamera::framing(Canvas::DEFAULT)
        .view(Canvas::DEFAULT)
        .expect("the framing camera has a view")
        .focal_px();
    let mut off_centre = at(100.0);
    off_centre.tilt_deg = Finite32::new(30.0).expect("finite");
    off_centre.perspective_px = Some(Finite32::new(focal).expect("finite"));
    apply(
        &control,
        vec![
            move_preset(),
            Edit::ItemSetGeometry {
                item_id: ItemId::new("a1"),
                scene_id: None,
                geometry: off_centre,
            },
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some(tilted_20()),
            },
        ],
    );
    switch(&control, "a", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub().load();
    let tracks = hub.shot().tracks().expect("a transition runs");
    assert!(
        !tracks
            .iter()
            .any(|track| matches!(track, Track::Pair { .. })),
        "it fades: {tracks:?}"
    );
}

// Motion that drives an effect's settings (ADR-0138).

/// `a1`'s motion: a sine of `amplitude` over one second on its `halo` glow's `param`.
fn pulse_halo(param: &str, amplitude: f32) -> Edit {
    Edit::ItemSetMotion {
        item_id: ItemId::new("a1"),
        scene_id: None,
        motion: vec![jstream_protocol::MotionModifier {
            modifier_id: "pulse".into(),
            property: jstream_protocol::MotionProperty::Effect,
            effect: Some(jstream_protocol::MotionEffect {
                filter_id: jstream_protocol::FilterId::new("halo"),
                param: param.into(),
            }),
            source: None,
            kind: jstream_protocol::MotionKind::Oscillate {
                shape: jstream_protocol::MotionShape::Sine,
                amplitude: Finite32::new(amplitude).expect("finite"),
                period_ms: 1000,
                phase: Finite32::ZERO,
            },
            clock: jstream_protocol::MotionClock::Engine,
            enabled: true,
            amount: None,
        }],
    }
}

/// `a1`'s `halo` glow, at radius 4.
fn halo() -> Edit {
    use jstream_protocol::{FilterChain, FilterId, FilterOwner};
    Edit::FilterAdd {
        owner: FilterOwner::Item {
            item_id: ItemId::new("a1"),
        },
        chain: FilterChain::Video,
        filter_id: FilterId::new("halo"),
        kind: "glow".into(),
        settings: serde_json::from_value(json!({"radius": 4.0})).expect("settings"),
        enabled: None,
        index: None,
    }
}

/// The settings of `a1`'s effects in scene `a` as drawn at `at`.
fn drawn_effects(engine: &Engine, at: jstream_media::MediaTime) -> Vec<Settings> {
    let owner = crate::chain::ChainOwner::Item(SceneId::new("a"), ItemId::new("a1"));
    engine
        .render_hub()
        .load()
        .shot()
        .drawn_settings(&owner, at)
        .expect("a1's effects run")
}

/// A modifier on an effect's setting moves it each frame, kept within the range its
/// kind gives it, leaves the item's box where it is, and pads the layer for the most
/// it reaches (ADR-0138).
#[test]
fn a_modifier_drives_an_effect_setting_within_its_range() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    apply(&control, vec![halo(), pulse_halo("radius", 20.0)]);
    let radius = |at: f64| {
        drawn_effects(&engine, secs(at))[0]
            .get("radius")
            .and_then(serde_json::Value::as_f64)
            .expect("a number")
    };
    // A quarter period in, the sine is at its top: 4 + 20.
    assert!((radius(0.25) - 24.0).abs() < 1e-3, "{}", radius(0.25));
    // Three quarters in, at its bottom: 4 - 20, kept at the glow's least radius, 0.
    assert_eq!(radius(0.75), 0.0);
    // On the period, it adds 0 and the chain draws its own settings.
    assert!((radius(1.0) - 4.0).abs() < 1e-3);
    // The box doesn't move.
    let x = moved_x(&engine, "a", secs(0.25));
    let still = {
        apply(
            &control,
            vec![Edit::ItemSetMotion {
                item_id: ItemId::new("a1"),
                scene_id: None,
                motion: Vec::new(),
            }],
        );
        moved_x(&engine, "a", secs(0.25))
    };
    assert_eq!(x, still);
    // The layer holds radius 24, rounded up to 32, not the 16 radius 4 needs.
    apply(&control, vec![pulse_halo("radius", 20.0)]);
    let hub = engine.render_hub().load();
    let pad = hub
        .shot()
        .item_chain(&SceneId::new("a"), &ItemId::new("a1"))
        .and_then(|chain| chain.pad);
    assert_eq!(pad, Some((0, 32)));
}

/// A modifier on a colour setting lightens or darkens its red, green and blue, and one
/// naming a setting the effect's kind doesn't have adds nothing (ADR-0138).
#[test]
fn a_modifier_moves_a_colour_and_one_on_a_missing_setting_adds_nothing() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    apply(&control, vec![halo(), pulse_halo("color", 0.5)]);
    // Three quarters in, the sine is at -0.5: white darkens to half.
    let drawn = drawn_effects(&engine, secs(0.75));
    let color = drawn[0].get("color").and_then(serde_json::Value::as_str);
    assert!(
        matches!(color, Some(c) if c.starts_with("#808080")),
        "{color:?}"
    );
    apply(&control, vec![pulse_halo("missing", 20.0)]);
    let drawn = drawn_effects(&engine, secs(0.25));
    assert_eq!(drawn[0].get("radius"), Some(&json!(4.0)));
    assert_eq!(drawn[0].get("missing"), None);
}

/// #1417 review B1: an edit that adds, then removes, only an effect modifier reaches the
/// renderer, though it changes nothing else of the shot.
#[test]
fn an_edit_to_an_effect_modifier_alone_is_drawn() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    apply(&control, vec![halo()]);
    let color = |engine: &Engine| {
        drawn_effects(engine, secs(0.75))[0]
            .get("color")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let white = color(&engine);
    apply(&control, vec![pulse_halo("color", 0.5)]);
    let dimmed = color(&engine);
    assert!(
        dimmed.as_deref().is_some_and(|c| c.starts_with("#808080")),
        "{dimmed:?}"
    );
    apply(
        &control,
        vec![Edit::ItemSetMotion {
            item_id: ItemId::new("a1"),
            scene_id: None,
            motion: Vec::new(),
        }],
    );
    assert_eq!(color(&engine), white, "the pulse stops with its modifier");
}

/// `scene.switch` to `scene` through the keyframes `via` (ADR-0166 decision 2).
fn switch_via(
    control: &dyn Control,
    scene: &str,
    via: &[&str],
    preset: Option<&str>,
) -> Result<(), jstream_protocol::ProtocolError> {
    let action = Action::SceneSwitch {
        scene_id: SceneId::new(scene),
        preset_id: preset.map(TransitionPresetId::new),
        via: via.iter().map(|id| SceneId::new(*id)).collect(),
    };
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|error| match error {
            jstream_control::ControlError::Protocol(wire) => wire.error,
            other => panic!("{other:?}"),
        })
}

/// The family of [`family`] with a second keyframe `k2`: `a1` is at x 100 in `a`, 500
/// in `k` and 1300 in `k2`, with a linear 400 ms Move preset, `a` on program.
fn three_stops() -> Engine {
    let engine = family();
    let control = connected(&engine);
    apply(
        &control,
        vec![
            move_preset(),
            variant("k2", "a"),
            geometry("a1", Some("k"), 500.0),
            geometry("a1", Some("k2"), 1300.0),
        ],
    );
    switch(&control, "a", None);
    engine
}

/// The x of the pair that starts at `from_x`, `ms` into the running transition.
fn pair_x(engine: &Engine, from_x: f32, ms: f32) -> f32 {
    let hub = engine.render_hub().load();
    let tracks = hub.shot().tracks().expect("a transition runs");
    let track = tracks
        .iter()
        .find(|track| {
            matches!(track, Track::Pair { from, .. } if from.geometry.center.x.get() == from_x)
        })
        .expect("the pair");
    track
        .sample(ms, [1920.0, 1080.0])
        .expect("drawn")
        .geometry
        .center
        .x
        .get()
}

/// ADR-0166 decision 2: a Move through a middle keyframe passes exactly through it at
/// its share of the path, and keeps its speed there instead of stopping or turning a
/// corner. Without `via` it goes straight.
#[test]
fn a_move_via_a_keyframe_passes_through_it_without_stopping() {
    let engine = three_stops();
    let control = connected(&engine);
    switch_via(&control, "k2", &["k"], Some("move")).expect("switch");
    // Linear over 400 ms, two legs: the middle stop at 200 ms.
    assert!((pair_x(&engine, 100.0, 200.0) - 500.0).abs() < 1e-3);
    assert!((pair_x(&engine, 100.0, 400.0) - 1300.0).abs() < 1e-3);
    // The speed either side of the stop is the same: (1300 - 100) / 2 per leg of
    // 200 ms, 3 px/ms.
    let before = (pair_x(&engine, 100.0, 200.0) - pair_x(&engine, 100.0, 199.0)) / 1.0;
    let after = (pair_x(&engine, 100.0, 201.0) - pair_x(&engine, 100.0, 200.0)) / 1.0;
    assert!((before - 3.0).abs() < 0.05, "{before}");
    assert!((after - 3.0).abs() < 0.05, "{after}");
    // The other item, at x 500 everywhere, stays still.
    assert!((pair_x(&engine, 500.0, 100.0) - 500.0).abs() < 1e-3);

    // The same switch without `via` goes straight through x 700 at halfway.
    switch(&control, "a", None);
    switch(&control, "k2", Some("move"));
    assert!((pair_x(&engine, 100.0, 200.0) - 700.0).abs() < 1e-3);
}

/// ADR-0166 decision 2: an item a stop hides passes that stop by, and a stop that isn't
/// a member of the family, the destination itself, a repeat or too many stops are
/// refused before anything switches.
#[test]
fn a_via_stop_that_hides_an_item_is_passed_by_and_bad_stops_are_refused() {
    use jstream_protocol::ProtocolError;
    let engine = three_stops();
    let control = connected(&engine);
    apply(
        &control,
        vec![Edit::ItemSetVisible {
            item_id: ItemId::new("a1"),
            scene_id: Some(SceneId::new("k")),
            visible: false,
        }],
    );
    switch_via(&control, "k2", &["k"], Some("move")).expect("switch");
    assert!((pair_x(&engine, 100.0, 200.0) - 700.0).abs() < 1e-3);

    switch(&control, "a", None);
    apply(&control, vec![create("other")]);
    let field = |result: Result<(), ProtocolError>| match result {
        Err(ProtocolError::InvalidArgument { field, .. }) => field,
        other => panic!("not invalid_argument: {other:?}"),
    };
    assert_eq!(field(switch_via(&control, "k2", &["other"], None)), "via.0");
    assert_eq!(
        field(switch_via(&control, "k2", &["k", "k2"], None)),
        "via.1"
    );
    assert_eq!(
        field(switch_via(&control, "k2", &["k", "k"], None)),
        "via.1"
    );
    let nine = ["k", "a"].repeat(5);
    assert_eq!(field(switch_via(&control, "k2", &nine[..9], None)), "via");
    assert!(matches!(
        switch_via(&control, "k2", &["missing"], None),
        Err(ProtocolError::NotFound { .. })
    ));
    assert_eq!(
        engine.render_hub().load().shot().program(),
        Some(&SceneId::new("a")),
        "a refused switch changes nothing"
    );
}

/// The sources the program plan of a Move from `a` to `k2` looks up, through `via`, 100
/// ms in: `blue`'s item is hidden in `a` and `k2` and shown only in `k`.
fn looked_up(via: &[&str]) -> Vec<String> {
    use crate::snapshot::{self, Frame, Look};
    use crate::transition::{self, From, Running};
    let hidden = |scene: Option<&str>, visible: bool| Edit::ItemSetVisible {
        item_id: ItemId::new("b1"),
        scene_id: scene.map(SceneId::new),
        visible,
    };
    let mut graph = jstream_graph::Graph::new().with_kinds(crate::kinds::builtin());
    let mut blue = colour("blue");
    if let Edit::SourceCreate { source_id, .. } = &mut blue {
        *source_id = SourceId::new("blue");
    }
    graph
        .apply(
            &[
                create("a"),
                colour("red"),
                blue,
                add("a", "a1", "red"),
                add("a", "b1", "blue"),
                hidden(None, false),
                move_preset(),
                variant("k", "a"),
                variant("k2", "a"),
                hidden(Some("k"), true),
            ],
            None,
        )
        .expect("edits");
    let (_, run) = transition::resolve(
        &graph,
        Some(&SceneId::new("a")),
        &SceneId::new("k2"),
        Some(&TransitionPresetId::new("move")),
    )
    .expect("resolves");
    let running = Running {
        from: From::Scene(SceneId::new("a")),
        to: SceneId::new("k2"),
        start: secs(0.0),
        run: run.expect("a move runs"),
        by_item: true,
        from_camera: None,
        via: via.iter().map(|id| SceneId::new(*id)).collect(),
    };
    let white: Vec<u8> = std::iter::repeat_n([255u8; 4], 4).flatten().collect();
    let frame = Frame::new(2, 2, &white, 1).expect("2x2 RGBA8");
    let mut asked = Vec::new();
    snapshot::plan_with(
        &graph,
        Some(&SceneId::new("k2")),
        Some((&running, secs(0.1))),
        &mut |source: &SourceId| {
            asked.push(source.as_str().to_owned());
            Some(Look::frame(frame.clone(), None))
        },
        &crate::filter_assets::FilterAssets::default(),
    );
    asked.sort();
    asked.dedup();
    asked
}

/// ADR-0166 decision 2: a Move's stops are read only for their values, so a source only
/// a stop shows isn't looked up: it isn't shown, heard, restarted or woken for the Move.
#[test]
fn a_via_stop_reaches_no_source_that_only_it_shows() {
    assert_eq!(looked_up(&[]), ["red"]);
    assert_eq!(looked_up(&["k"]), ["red"]);
}

/// A camera `degrees` around the framing camera's target, about the vertical axis, at
/// the framing camera's distance.
fn around(degrees: f32) -> jstream_protocol::SceneCamera {
    let framing = jstream_protocol::SceneCamera::framing(jstream_protocol::Canvas::DEFAULT);
    let target = framing.target;
    let distance = framing.position.z.get() - target.z.get();
    let (sin, cos) = degrees.to_radians().sin_cos();
    jstream_protocol::SceneCamera {
        position: jstream_protocol::Vec3 {
            x: Finite32::new(target.x.get() + distance * sin).expect("finite"),
            y: target.y,
            z: Finite32::new(target.z.get() + distance * cos).expect("finite"),
        },
        ..framing
    }
}

fn distance(camera: &jstream_protocol::SceneCamera) -> f32 {
    let d = |a: Finite32, b: Finite32| a.get() - b.get();
    let (x, y, z) = (
        d(camera.position.x, camera.target.x),
        d(camera.position.y, camera.target.y),
        d(camera.position.z, camera.target.z),
    );
    (x * x + y * y + z * z).sqrt()
}

/// ADR-0166 decisions 2 and 3: the camera passes through a middle keyframe's camera,
/// and an `orbit` Move keeps its distance from the target where a straight one cuts
/// across toward it.
#[test]
fn the_camera_passes_through_a_stop_and_an_orbit_keeps_its_distance() {
    let engine = three_stops();
    let control = connected(&engine);
    let orbit = Edit::TransitionCreate {
        preset_id: TransitionPresetId::new("orbit"),
        name: "Orbit".into(),
        kind: "move".into(),
        duration_ms: 400,
        easing: Easing::Linear,
        settings: Settings(
            [("camera_path".to_owned(), json!("orbit"))]
                .into_iter()
                .collect(),
        ),
    };
    let camera = |scene: &str, degrees: f32| Edit::SceneSetCamera {
        scene_id: SceneId::new(scene),
        camera: Some(around(degrees)),
    };
    apply(
        &control,
        vec![
            orbit,
            camera("a", 0.0),
            camera("k", 45.0),
            camera("k2", 90.0),
        ],
    );
    let cameras = |ms: f32| {
        let hub = engine.render_hub().load();
        let all = hub.shot().track_cameras(ms).expect("a transition runs");
        all[0].expect("3D")
    };
    let full = distance(&around(0.0));

    // Straight, through `k`: at the stop it is `k`'s camera.
    switch_via(&control, "k2", &["k"], Some("move")).expect("switch");
    let mid = cameras(200.0);
    let want = around(45.0);
    for (got, want) in [
        (mid.position.x, want.position.x),
        (mid.position.z, want.position.z),
    ] {
        assert!((got.get() - want.get()).abs() < 1e-2, "{mid:?}");
    }

    // Straight, with no stop: halfway on the chord, nearer the target.
    switch(&control, "a", None);
    switch(&control, "k2", Some("move"));
    assert!(distance(&cameras(200.0)) < full * 0.75);

    // Orbiting: the same distance all the way, and halfway is 45° round.
    switch(&control, "a", None);
    switch(&control, "k2", Some("orbit"));
    for ms in [100.0, 200.0, 300.0] {
        assert!((distance(&cameras(ms)) - full).abs() < 0.5, "{ms}");
    }
    let half = cameras(200.0);
    assert!(
        (half.position.x.get() - want.position.x.get()).abs() < 0.5,
        "{half:?}"
    );
    assert!(
        (half.position.z.get() - want.position.z.get()).abs() < 0.5,
        "{half:?}"
    );
}

/// ADR-0171 decision 5: a `data` modifier with a palette sets an effect's colour from
/// its field's text while the text is listed, preview and program alike, and the
/// effect's own colour shows otherwise.
#[test]
fn a_data_modifier_sets_an_effect_colour_from_a_palette() {
    let engine = family();
    let control = connected(&engine);
    switch(&control, "a", None);
    let f = |v: f32| Finite32::new(v).expect("finite");
    let tint = Edit::ItemSetMotion {
        item_id: ItemId::new("a1"),
        scene_id: None,
        motion: vec![jstream_protocol::MotionModifier {
            modifier_id: "clip".into(),
            property: jstream_protocol::MotionProperty::Effect,
            effect: Some(jstream_protocol::MotionEffect {
                filter_id: jstream_protocol::FilterId::new("halo"),
                param: "color".into(),
            }),
            source: None,
            kind: jstream_protocol::MotionKind::Data {
                input: None,
                key: "track/1".into(),
                field: "state".into(),
                range: [f(0.0), f(1.0)],
                states: std::collections::BTreeMap::new(),
                smoothing_ms: 0,
                curve: jstream_protocol::MotionCurve::Linear,
                from: f(0.0),
                to: f(1.0),
                color_from: Some(jstream_protocol::MotionColorFrom::Palette),
                palette: [("recording".to_owned(), "#ff2000".to_owned())]
                    .into_iter()
                    .collect(),
            },
            clock: jstream_protocol::MotionClock::Engine,
            enabled: true,
            amount: None,
        }],
    };
    apply(&control, vec![halo(), tint]);
    let color = |at: f64| drawn_effects(&engine, secs(at))[0].0.get("color").cloned();
    let own = color(1.0);
    assert_ne!(
        own,
        Some(json!("#ff2000")),
        "nothing sent yet: its own colour"
    );
    let data = engine.live_data();
    data.put("DAW", "track/1", br#"{"state": "recording"}"#, 0)
        .expect("taken");
    assert_eq!(color(1.0), Some(json!("#ff2000")), "recording: red");
    let shot = engine.render_hub().load();
    let mut follower = crate::reactive::Follower::default();
    follower.follow(shot.shot().audio(), secs(1.0));
    assert_eq!(
        color(1.0),
        Some(json!("#ff2000")),
        "followed, it lands at once"
    );
    data.put("DAW", "track/1", br#"{"state": "stopped"}"#, 10)
        .expect("taken");
    follower.follow(shot.shot().audio(), secs(1.1));
    assert_eq!(color(1.1), own, "not listed: its own colour again");
}

/// A builtin source `id` of `kind` with `settings`.
fn builtin(id: &str, kind: &str, settings: &[(&str, serde_json::Value)]) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin { kind: kind.into() },
        settings: Settings(
            settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        ),
        duplicated_from: None,
    }
}

/// The framing camera for the default canvas, moved `dx` across.
fn framing_across(dx: f32) -> jstream_protocol::SceneCamera {
    let mut camera = jstream_protocol::SceneCamera::framing(jstream_protocol::Canvas::DEFAULT);
    camera.position.x = Finite32::new(camera.position.x.get() + dx).expect("finite");
    camera.target.x = Finite32::new(camera.target.x.get() + dx).expect("finite");
    camera
}

/// A 3D scene `s` placing a scene-lit model, a point light and an environment light
/// whose sky is the scene's backdrop, seen through a camera 300 px left of framing, and
/// its keyframe scene `k`, which differs only in its camera, 300 px right.
fn model_family(engine: &Engine) -> crate::InProcessControl {
    let control = connected(engine);
    apply(
        &control,
        vec![
            create("s"),
            builtin("cube", "model", &[("shape", json!("cube"))]),
            builtin("lamp", "light", &[("type", json!("point"))]),
            builtin(
                "sky",
                "light",
                &[("type", json!("environment")), ("backdrop", json!(true))],
            ),
            add("s", "m", "cube"),
            add("s", "l", "lamp"),
            add("s", "e", "sky"),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("s"),
                camera: Some(framing_across(-300.0)),
            },
            variant("k", "s"),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some(framing_across(300.0)),
            },
            move_preset(),
        ],
    );
    assert!(engine.settle_sources(), "the sources were built in time");
    control
}

/// A Move between two keyframes of a 3D scene that holds a model, lights and a sky
/// backdrop, and differs only in its camera, moves the camera straight from one to the
/// other: every track, and the sky, is seen through the camera on that line.
#[test]
fn a_move_between_two_views_of_a_model_scene_keeps_to_the_line() {
    let engine = Engine::new();
    let control = model_family(&engine);
    let (from, to) = (framing_across(-300.0), framing_across(300.0));
    switch(&control, "s", None);
    switch(&control, "k", Some("move"));
    let hub = engine.render_hub().load();
    let shot = hub.shot();
    for (ms, k) in [(0.0, 0.0), (200.0, 0.5), (400.0, 1.0)] {
        let want = jstream_render::transition::lerp_camera(&from, &to, k);
        let cameras = shot.track_cameras(ms).expect("a transition runs");
        assert!(!cameras.is_empty(), "the model is a track");
        assert!(
            cameras.iter().all(|camera| *camera == Some(want)),
            "at {ms} ms: {cameras:?}, want {want:?}"
        );
        assert_eq!(
            shot.sky_cameras(ms),
            Some((Some(want), Some(want))),
            "at {ms} ms"
        );
    }
}

/// [`model_family`] with the scene drawn as one picture through its chain, `z`, a 2D
/// scene, on program, then a Fade onto `s` and, `fade_ms` later, a Move to `k`. The clock
/// is held (ADR-0172), so no plan lets the Fade go when it ends.
fn move_after_a_fade_onto_the_model_scene(fade_ms: u32) -> Engine {
    use jstream_media::Timebase;
    use jstream_platform::fake::FakeClock;
    let engine = Engine::new();
    engine.set_source_clock(std::sync::Arc::new(FakeClock::new(Timebase::MICROS)));
    let control = model_family(&engine);
    let call = |action: Action| {
        block_on(control.call(Call::Action { action })).expect("action");
    };
    apply(
        &control,
        vec![
            create("z"),
            sharpen(
                jstream_protocol::FilterOwner::Scene {
                    scene_id: SceneId::new("s"),
                },
                "look",
            ),
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("fade"),
                name: "Fade".into(),
                kind: "fade".into(),
                duration_ms: 400,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
        ],
    );
    call(Action::ClockHold);
    switch(&control, "z", None);
    switch(&control, "s", Some("fade"));
    call(Action::ClockStep { ms: fade_ms });
    switch(&control, "k", Some("move"));
    engine
}

/// #1598: a Move between two views of a 3D scene, started once the Fade onto it has
/// ended but before a plan let that go, moves straight from the scene's camera to the
/// keyframe's, item by item, as from the scene at rest: not from the framing camera, as
/// if what the ended Fade left on screen were 2D.
#[test]
fn a_move_after_an_ended_fade_starts_from_the_scenes_camera() {
    let engine = move_after_a_fade_onto_the_model_scene(600);
    let (from, to) = (framing_across(-300.0), framing_across(300.0));
    let hub = engine.render_hub().load();
    let shot = hub.shot();
    for (ms, k) in [(0.0, 0.0), (100.0, 0.25), (400.0, 1.0)] {
        let want = jstream_render::transition::lerp_camera(&from, &to, k);
        let cameras = shot.track_cameras(ms).expect("a transition runs");
        assert!(!cameras.is_empty(), "the model is a track");
        assert!(
            cameras.iter().all(|camera| *camera == Some(want)),
            "at {ms} ms, every item through the moving camera: {cameras:?}, want {want:?}"
        );
        assert_eq!(
            shot.sky_cameras(ms),
            Some((Some(want), Some(want))),
            "at {ms} ms"
        );
    }
}

/// #1598: a Move between two views of a 3D scene that cuts short a Fade onto it, which
/// shows the scene as one picture, moves from the camera that picture was seen through,
/// the scene's own, not from the framing camera, as if the picture were 2D.
#[test]
fn a_move_cutting_short_a_fade_onto_a_3d_scene_starts_from_its_camera() {
    let engine = move_after_a_fade_onto_the_model_scene(200);
    let from = framing_across(-300.0);
    let hub = engine.render_hub().load();
    let shot = hub.shot();
    let cameras = shot.track_cameras(0.0).expect("a transition runs");
    // The scene's picture leaves flat; the keyframe's items glide in.
    let moving: Vec<_> = cameras.iter().flatten().collect();
    assert!(!moving.is_empty(), "{cameras:?}");
    assert!(
        moving.iter().all(|camera| **camera == from),
        "from the scene's camera: {cameras:?}, want {from:?}"
    );
}
