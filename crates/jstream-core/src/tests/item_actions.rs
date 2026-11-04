//! `item.show`, `item.hide` and `item.toggle` (ADR-0101) through `InProcessControl`: they
//! change the placement's visibility as `item.set_visible` does, with a `rev` and no
//! undo step, play the item's own appear and disappear effects on program, and can be
//! bound to hotkeys, which go with the item.

use super::{add, block_on, code, connected, create, edits, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, CallResult, Chord, Easing, Edit, HistoryCall, HistoryOp, HotkeyBinding, ItemId,
    ItemTransition, Query, QueryResult, Rev, SceneId, Settings, SourceId, SourceKind,
    TransitionEffect, TransitionPresetId,
};
use jstream_render::transition::{Effect, Track};
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

fn hidden(item: &str, scene: Option<&str>) -> Edit {
    Edit::ItemSetVisible {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        visible: false,
    }
}

fn apply(control: &dyn Control, list: Vec<Edit>) -> Rev {
    match block_on(control.call(Call::Batch(edits(list))))
        .expect("applies")
        .result
    {
        CallResult::Batch { rev, .. } => rev,
        other => panic!("not a batch result: {other:?}"),
    }
}

fn act(control: &dyn Control, action: Action) -> Result<(), &'static str> {
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|error| code(&error).unwrap_or("not a protocol error"))
}

fn undo(control: &dyn Control) -> Result<(), &'static str> {
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo))))
        .map(|_| ())
        .map_err(|error| code(&error).unwrap_or("not a protocol error"))
}

fn show(item: &str) -> Action {
    Action::ItemShow {
        item_id: ItemId::new(item),
        scene_id: None,
    }
}

fn hide(item: &str) -> Action {
    Action::ItemHide {
        item_id: ItemId::new(item),
        scene_id: None,
    }
}

fn toggle(item: &str, scene: Option<&str>) -> Action {
    Action::ItemToggle {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
    }
}

/// A `fade` preset of 400 ms, made the default.
fn fade_default() -> Vec<Edit> {
    vec![
        Edit::TransitionCreate {
            preset_id: TransitionPresetId::new("fade"),
            name: "Fade".into(),
            kind: "fade".into(),
            duration_ms: 400,
            easing: Easing::Linear,
            settings: Settings::default(),
        },
        Edit::TransitionSetDefault {
            preset_id: TransitionPresetId::new("fade"),
        },
    ]
}

/// Scene `a` on program, placing `red` as `a1` (shown) and `a2` (hidden).
fn scene(extra: Vec<Edit>) -> Engine {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut list = vec![
        create("a"),
        colour("red"),
        add("a", "a1", "red"),
        add("a", "a2", "red"),
        hidden("a2", None),
    ];
    list.extend(extra);
    apply(&control, list);
    assert!(engine.settle_sources(), "the sources were built in time");
    engine
}

fn visible(control: &dyn Control, item: &str) -> bool {
    let QueryResult::ItemList { items, .. } = query(
        control,
        Query::ItemList {
            scene_id: SceneId::new("a"),
        },
    )
    .expect("item.list") else {
        panic!("not an item list");
    };
    items
        .iter()
        .find(|info| info.item_id.as_str() == item)
        .map(|info| info.visible)
        .expect("the item")
}

fn tracks(engine: &Engine) -> Option<Vec<Track>> {
    engine.render_hub().load().shot().tracks()
}

#[test]
fn a_toggle_shows_an_item_with_its_appear_effect_and_moves_nothing_else() {
    let engine = scene(fade_default());
    let control = connected(&engine);
    let rev = engine.rev();
    act(&control, toggle("a2", None)).expect("toggles");
    assert!(visible(&control, "a2"), "shown in the collection");
    assert_eq!(engine.rev(), rev.next().expect("a next rev"), "one rev");
    let tracks = tracks(&engine).expect("the effect runs");
    let [
        Track::Pair { from, to, .. },
        Track::Appear {
            effect: Effect::Fade,
            timing,
            ..
        },
    ] = tracks.as_slice()
    else {
        panic!("not a1 held and a2 fading in: {tracks:?}");
    };
    assert_eq!(from, to, "a1 stays where it is");
    assert_eq!(timing.duration_ms, 400.0, "the default preset's duration");
}

#[test]
fn hide_plays_the_items_own_disappear_effect() {
    let mut extra = fade_default();
    extra.push(Edit::ItemSetTransition {
        item_id: ItemId::new("a1"),
        transition: ItemTransition {
            disappear: TransitionEffect::Scale,
            ..ItemTransition::default()
        },
    });
    let engine = scene(extra);
    let control = connected(&engine);
    act(&control, hide("a1")).expect("hides");
    assert!(!visible(&control, "a1"));
    let tracks = tracks(&engine).expect("the effect runs");
    assert!(
        matches!(
            tracks.as_slice(),
            [Track::Disappear {
                effect: Effect::Scale,
                ..
            }]
        ),
        "{tracks:?}"
    );
}

/// Under the initial `cut` preset, `inherit` is no animation: the item shows on the next
/// frame, with no run. An item's own effect still plays, over the cut's duration.
#[test]
fn under_a_cut_an_inherited_effect_shows_at_once_and_an_own_one_plays() {
    let engine = scene(Vec::new());
    let control = connected(&engine);
    act(&control, show("a2")).expect("shows");
    assert_eq!(tracks(&engine), None, "no run");
    apply(
        &control,
        vec![Edit::ItemSetTransition {
            item_id: ItemId::new("a1"),
            transition: ItemTransition {
                disappear: TransitionEffect::Slide,
                ..ItemTransition::default()
            },
        }],
    );
    act(&control, hide("a1")).expect("hides");
    let tracks = tracks(&engine).expect("its own effect runs");
    assert!(
        tracks.iter().any(|track| matches!(
            track,
            Track::Disappear {
                effect: Effect::Slide,
                ..
            }
        )),
        "{tracks:?}"
    );
}

#[test]
fn showing_a_shown_item_changes_nothing_and_errors_name_the_actions_fields() {
    let engine = scene(Vec::new());
    let control = connected(&engine);
    let rev = engine.rev();
    act(&control, show("a1")).expect("already shown");
    act(&control, hide("a2")).expect("already hidden");
    assert_eq!(engine.rev(), rev, "no change, no rev");
    assert_eq!(act(&control, show("ghost")), Err("not_found"));
    apply(&control, vec![create("b")]);
    let error = block_on(control.call(Call::Action {
        action: Action::ItemShow {
            item_id: ItemId::new("a1"),
            scene_id: Some(SceneId::new("b")),
        },
    }))
    .expect_err("b doesn't place a1");
    assert_eq!(code(&error), Some("invalid_argument"));
    assert!(
        error.to_string().contains("scene_id") && !error.to_string().contains("edits."),
        "{error}"
    );
}

/// An action is never undone (ADR-0024 §5): it makes no step, and the sender's undo
/// still takes back its last batch. Another client's undo that would overwrite the
/// toggle conflicts.
#[test]
fn a_toggle_is_no_undo_step_and_another_clients_undo_over_it_conflicts() {
    let engine = scene(Vec::new());
    let first = connected(&engine);
    let other = connected(&engine);
    apply(&first, vec![hidden("a1", None)]);
    act(&other, toggle("a1", None)).expect("shows a1 again");
    assert_eq!(undo(&other), Err("history_empty"), "no step");
    assert_eq!(
        undo(&first),
        Err("undo_conflict"),
        "the undo would overwrite the toggle"
    );
    assert!(visible(&first, "a1"));
}

#[test]
fn a_toggle_in_a_keyframe_scene_follows_its_override() {
    let engine = scene(vec![
        Edit::SceneCreateVariant {
            scene_id: SceneId::new("k"),
            variant_of: SceneId::new("a"),
            name: "K".into(),
        },
        Edit::ItemSetVisible {
            item_id: ItemId::new("a2"),
            scene_id: Some(SceneId::new("k")),
            visible: true,
        },
    ]);
    let control = connected(&engine);
    act(&control, toggle("a2", Some("k"))).expect("toggles");
    let QueryResult::StateGet { scenes, .. } = query(&control, Query::StateGet).expect("state")
    else {
        panic!("not state.get");
    };
    let k = scenes
        .iter()
        .find(|scene| scene.scene_id.as_str() == "k")
        .expect("k");
    let a2 = k.overrides.iter().find(|o| o.item_id.as_str() == "a2");
    assert_eq!(
        a2.and_then(|o| o.visible),
        Some(false),
        "the override was shown, so it hides"
    );
    assert!(!visible(&control, "a2"), "the item's own stays hidden");
}

#[test]
fn a_hotkey_binds_a_toggle_and_goes_with_its_item() {
    let engine = scene(Vec::new());
    let control = connected(&engine);
    let binding = Edit::HotkeySet {
        chord: Chord::new("F1"),
        action: toggle("a2", None),
    };
    apply(&control, vec![binding]);
    let ghost = Edit::HotkeySet {
        chord: Chord::new("F2"),
        action: toggle("ghost", None),
    };
    let refused =
        block_on(control.call(Call::Batch(edits(vec![ghost])))).expect_err("no such item");
    assert_eq!(code(&refused), Some("not_found"));
    let removed = match block_on(control.call(Call::Batch(edits(vec![Edit::ItemRemove {
        item_id: ItemId::new("a2"),
    }]))))
    .expect("removes")
    .result
    {
        CallResult::Batch {
            removed_hotkeys, ..
        } => removed_hotkeys,
        other => panic!("not a batch: {other:?}"),
    };
    assert_eq!(
        removed,
        [HotkeyBinding::new(Chord::new("F1"), &toggle("a2", None))]
    );
    undo(&control).expect("undoes");
    let QueryResult::StateGet { hotkeys, .. } = query(&control, Query::StateGet).expect("state")
    else {
        panic!("not state.get");
    };
    assert_eq!(
        hotkeys,
        [HotkeyBinding::new(Chord::new("F1"), &toggle("a2", None))],
        "the undo brings the binding back with the item"
    );
}

/// A `fade` default preset that runs for a minute, so a switch is still in flight when
/// the test acts.
fn slow_fade_default() -> Vec<Edit> {
    let mut list = fade_default();
    if let Edit::TransitionCreate { duration_ms, .. } = &mut list[0] {
        *duration_ms = 60_000;
    }
    list
}

fn switch(control: &dyn Control, scene: &str) {
    act(
        control,
        Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: None,
        },
    )
    .expect("switches");
}

/// A placement program doesn't draw changes nothing on screen: a keyframe of the program
/// scene that program isn't showing starts no run.
#[test]
fn a_toggle_of_a_placement_program_doesnt_draw_starts_no_run() {
    let mut extra = fade_default();
    extra.push(Edit::SceneCreateVariant {
        scene_id: SceneId::new("k"),
        variant_of: SceneId::new("a"),
        name: "K".into(),
    });
    let engine = scene(extra);
    let control = connected(&engine);
    act(&control, toggle("a2", Some("k"))).expect("toggles k's placement");
    assert_eq!(tracks(&engine), None, "a on program shows no change");
}

/// A press while a switch is in flight retargets it from what is on screen, so the
/// item appears over the rest of the switch; a press program doesn't see leaves the
/// switch as it was.
#[test]
fn a_press_during_a_switch_continues_it_and_one_program_doesnt_see_leaves_it() {
    let mut extra = slow_fade_default();
    extra.extend([
        create("b"),
        add("b", "b1", "red"),
        add("b", "b2", "red"),
        hidden("b2", None),
        Edit::SceneCreateVariant {
            scene_id: SceneId::new("bk"),
            variant_of: SceneId::new("b"),
            name: "BK".into(),
        },
    ]);
    let engine = scene(extra);
    let control = connected(&engine);
    switch(&control, "b");
    let switching = tracks(&engine).expect("the switch runs");
    act(&control, toggle("b2", Some("bk"))).expect("toggles bk's placement");
    assert_eq!(
        tracks(&engine).as_ref(),
        Some(&switching),
        "program doesn't draw bk, so the switch runs on as it was"
    );
    act(&control, show("b2")).expect("shows b2");
    let tracks = tracks(&engine).expect("still running");
    assert!(
        tracks.iter().any(|track| matches!(
            track,
            Track::Appear {
                effect: Effect::Fade,
                ..
            }
        )),
        "b2 fades in over the rest of the switch: {tracks:?}"
    );
}
