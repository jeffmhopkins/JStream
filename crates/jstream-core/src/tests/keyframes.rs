//! Keyframe scenes (K1, ADR-0049) through `InProcessControl`: a keyframe scene draws its
//! parent's items at their effective values and follows the parent's edits, a `move`
//! within a family pairs by item, and a batch reports the overrides it drops, which undo
//! restores.

use super::{add, block_on, connected, create, edits};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, CallResult, DroppedOverride, Easing, Edit, Finite32, HistoryCall, HistoryOp,
    ItemGeometry, ItemId, ItemOverride, Query, QueryResult, SceneId, Settings, Size, SourceId,
    SourceKind, TransitionPresetId, Vec2,
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

/// A live value addressed to a keyframe scene is refused (`values.<n>.scene_id`): live
/// values are drawn per item, so it would move the parent's item wherever it shows
/// (#720 review B2; #702 builds them). Nothing is applied, and a value naming the
/// parent's own scene is still taken.
#[test]
fn a_live_value_on_a_keyframe_scene_is_refused() {
    let engine = family();
    let control = connected(&engine);
    let live_set = |values: Vec<Edit>| {
        let action = Action::LiveSet {
            gesture_id: jstream_protocol::GestureId::new("drag"),
            values,
        };
        block_on(control.call(Call::Action { action }))
    };
    let error = live_set(vec![
        geometry("a1", None, 150.0),
        geometry("a1", Some("k"), 900.0),
    ])
    .expect_err("refused");
    match error.protocol() {
        Some(jstream_protocol::ProtocolError::InvalidArgument { field, .. }) => {
            assert_eq!(field, "values.1.scene_id");
        }
        other => panic!("not invalid_argument: {other:?}"),
    }
    let call = Call::Query {
        query: Query::LiveList,
    };
    let CallResult::Query {
        result: QueryResult::LiveList { gestures },
    } = block_on(control.call(call)).expect("live.list").result
    else {
        panic!("not live.list");
    };
    assert!(gestures.is_empty(), "nothing opened: {gestures:?}");
    live_set(vec![geometry("a1", Some("a"), 150.0)]).expect("the parent's own scene");
}
