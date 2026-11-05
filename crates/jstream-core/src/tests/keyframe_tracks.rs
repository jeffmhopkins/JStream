//! Keyframe tracks (ADR-0116) through `InProcessControl`: `scene.set_keyframe` draws a
//! placed scene through one of its keyframe scenes wherever another scene places it,
//! independently of every other placed scene, with a Move on program; the placing
//! scene's own keyframe wins where it overrides the placed scene's items; and deleting
//! the keyframe scene lets go of it.

use super::{add, block_on, code, connected, create, edits, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, Easing, Edit, Event, ItemGeometry, ItemId, PlacedKeyframe, Query, QueryResult,
    SceneId, SceneRole, Settings, Size, SourceId, SourceKind, TransitionPresetId, Vec2,
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

fn geometry(item: &str, scene: Option<&str>, x: f32) -> Edit {
    Edit::ItemSetGeometry {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        geometry: ItemGeometry::try_new(
            Vec2::new(x, 300.0).expect("finite"),
            Size::try_new(100.0, 100.0).expect("positive"),
        )
        .expect("valid"),
    }
}

fn variant(scene: &str, of: &str) -> Edit {
    Edit::SceneCreateVariant {
        scene_id: SceneId::new(scene),
        variant_of: SceneId::new(of),
        name: scene.to_uppercase(),
    }
}

/// Group `g` with child `{g}1` at `x`, keyframe `{g}k` moving it 50 px right, and its
/// `scene` source `s{g}` placed in `p` as item `p{g}`.
fn track(g: &str, x: f32) -> Vec<Edit> {
    vec![
        Edit::SceneCreate {
            scene_id: SceneId::new(g),
            name: g.to_uppercase(),
            role: Some(SceneRole::Group),
        },
        Edit::SourceCreate {
            source_id: SourceId::new(format!("s{g}")),
            name: g.to_uppercase(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new(g),
            },
            settings: Settings::default(),
            duplicated_from: None,
        },
        add(g, &format!("{g}1"), "red"),
        geometry(&format!("{g}1"), None, x),
        variant(&format!("{g}k"), g),
        geometry(&format!("{g}1"), Some(&format!("{g}k")), x + 50.0),
        add("p", &format!("p{g}"), &format!("s{g}")),
    ]
}

/// Scene `p` on program, placing groups `g`, `h` and `j` (children at 100, 300 and 500),
/// each with one keyframe.
fn three_tracks() -> (Engine, crate::InProcessControl) {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut list = vec![create("p"), colour("red")];
    list.extend(track("g", 100.0));
    list.extend(track("h", 300.0));
    list.extend(track("j", 500.0));
    block_on(control.call(Call::Batch(edits(list)))).expect("applies");
    assert!(engine.settle_sources(), "the sources were built in time");
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("p"),
            preset_id: None,
        },
    }))
    .expect("switch");
    (engine, control)
}

fn act(
    control: &dyn Control,
    scene: &str,
    keyframe: Option<&str>,
    preset: Option<&str>,
) -> Result<(), &'static str> {
    block_on(control.call(Call::Action {
        action: Action::SceneSetKeyframe {
            scene_id: SceneId::new(scene),
            keyframe_id: keyframe.map(SceneId::new),
            preset_id: preset.map(TransitionPresetId::new),
        },
    }))
    .map(|_| ())
    .map_err(|error| code(&error).unwrap_or("not a protocol error"))
}

fn x(placement: &Placement) -> f32 {
    match placement {
        Placement::Geometry(geometry) => geometry.center.x.get(),
        Placement::Legacy(_) | Placement::Layer(_) => panic!("not a geometry placement"),
    }
}

/// The x of each item the program shot places for the planned scene `id`.
fn placed(engine: &Engine, id: &SceneId) -> Vec<f32> {
    engine
        .render_hub()
        .load()
        .shot()
        .placed(id)
        .unwrap_or_else(|| panic!("the shot draws {id}"))
        .iter()
        .map(|(_, placement, _)| x(placement))
        .collect()
}

/// The planned ID of group `g`: its view through `keyframe`, or itself.
fn view(g: &str, keyframe: Option<&str>) -> SceneId {
    match keyframe {
        Some(k) => crate::snapshot::group_view(&SceneId::new(g), &SceneId::new(k)),
        None => SceneId::new(g),
    }
}

fn placed_keyframes(control: &dyn Control) -> Vec<PlacedKeyframe> {
    match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { keyframes, .. }) => keyframes,
        other => panic!("not studio.get: {other:?}"),
    }
}

/// The issue's acceptance (#1068): a scene with three independent states switches each
/// one without touching the others.
#[test]
fn three_placed_scenes_switch_independently() {
    let (engine, control) = three_tracks();
    act(&control, "g", Some("gk"), None).expect("g to gk");
    assert_eq!(placed(&engine, &view("g", Some("gk"))), [150.0]);
    assert_eq!(placed(&engine, &view("h", None)), [300.0]);
    assert_eq!(placed(&engine, &view("j", None)), [500.0]);
    act(&control, "j", Some("jk"), None).expect("j to jk");
    assert_eq!(placed(&engine, &view("g", Some("gk"))), [150.0]);
    assert_eq!(placed(&engine, &view("h", None)), [300.0]);
    assert_eq!(placed(&engine, &view("j", Some("jk"))), [550.0]);
    assert_eq!(
        placed_keyframes(&control),
        [
            PlacedKeyframe {
                scene_id: SceneId::new("g"),
                keyframe_id: SceneId::new("gk"),
            },
            PlacedKeyframe {
                scene_id: SceneId::new("j"),
                keyframe_id: SceneId::new("jk"),
            },
        ]
    );
    act(&control, "g", None, None).expect("g to its own values");
    assert_eq!(placed(&engine, &view("g", None)), [100.0]);
    assert_eq!(placed(&engine, &view("j", Some("jk"))), [550.0]);
    // Program on the placed scene itself draws its own values.
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("j"),
            preset_id: None,
        },
    }))
    .expect("switch");
    assert_eq!(placed(&engine, &view("j", None)), [500.0]);
}

/// On program, a switch moves the placed scene's children from what is on screen to
/// the keyframe's values with the preset's timing, every other item paired with itself.
#[test]
fn a_switch_on_program_moves_the_children() {
    let (engine, control) = three_tracks();
    block_on(control.call(Call::Batch(edits(vec![Edit::TransitionCreate {
        preset_id: TransitionPresetId::new("move"),
        name: "Move".into(),
        kind: "move".into(),
        duration_ms: 400,
        easing: Easing::Linear,
        settings: Settings::default(),
    }]))))
    .expect("applies");
    act(&control, "h", Some("hk"), Some("move")).expect("h to hk");
    let tracks = engine
        .render_hub()
        .load()
        .shot()
        .tracks()
        .expect("a Move runs");
    let mut moves: Vec<(f32, f32)> = tracks
        .iter()
        .map(|track| match track {
            Track::Pair { from, to, .. } => {
                (from.geometry.center.x.get(), to.geometry.center.x.get())
            }
            other => panic!("not a pair: {other:?}"),
        })
        .collect();
    moves.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(moves, [(100.0, 100.0), (300.0, 350.0), (500.0, 500.0)]);
}

/// The placing scene's keyframe that overrides a placed group's child draws it, over
/// the group's placed keyframe; where it overrides nothing of the group, the group's
/// placed keyframe draws.
#[test]
fn the_placing_scenes_keyframe_wins_where_it_overrides() {
    let (engine, control) = three_tracks();
    block_on(control.call(Call::Batch(edits(vec![
        variant("pk", "p"),
        geometry("g1", Some("pk"), 900.0),
    ]))))
    .expect("applies");
    act(&control, "g", Some("gk"), None).expect("g to gk");
    act(&control, "h", Some("hk"), None).expect("h to hk");
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("pk"),
            preset_id: None,
        },
    }))
    .expect("switch");
    assert_eq!(placed(&engine, &view("g", Some("pk"))), [900.0]);
    assert_eq!(placed(&engine, &view("h", Some("hk"))), [350.0]);
}

/// Deleting the keyframe scene lets go of it, with `scene.keyframe_changed`, and the
/// placed scene draws its own values; undo doesn't put it back.
#[test]
fn deleting_the_keyframe_scene_lets_go_of_it() {
    let (engine, control) = three_tracks();
    let mut events = control.events();
    act(&control, "g", Some("gk"), None).expect("g to gk");
    block_on(control.call(Call::Batch(edits(vec![Edit::SceneDelete {
        scene_id: SceneId::new("gk"),
    }]))))
    .expect("applies");
    assert!(placed_keyframes(&control).is_empty());
    assert_eq!(placed(&engine, &view("g", None)), [100.0]);
    let changes: Vec<Option<SceneId>> = super::drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::SceneKeyframeChanged { keyframe_id, .. } => Some(keyframe_id),
            _ => None,
        })
        .collect();
    assert_eq!(changes, [Some(SceneId::new("gk")), None]);
}

#[test]
fn a_switch_is_checked() {
    let (_engine, control) = three_tracks();
    assert_eq!(act(&control, "nope", None, None), Err("not_found"));
    assert_eq!(act(&control, "g", Some("nope"), None), Err("not_found"));
    assert_eq!(act(&control, "gk", None, None), Err("invalid_argument"));
    assert_eq!(
        act(&control, "g", Some("hk"), None),
        Err("invalid_argument")
    );
    assert_eq!(
        act(&control, "g", Some("gk"), Some("nope")),
        Err("not_found")
    );
    // Setting what is already set is a no-op that succeeds.
    act(&control, "g", None, None).expect("already its own values");
}

/// A placed scene drawn as one image (here a box smaller than the canvas, so it isn't
/// flattened) moves as one item, with its children moving inside its layer from the
/// keyframe on screen to the new one (ADR-0049 §3's machinery).
#[test]
fn an_unflattened_placed_scene_moves_its_children_inside_its_layer() {
    let (engine, control) = three_tracks();
    block_on(control.call(Call::Batch(edits(vec![
        Edit::TransitionCreate {
            preset_id: TransitionPresetId::new("move"),
            name: "Move".into(),
            kind: "move".into(),
            duration_ms: 400,
            easing: Easing::Linear,
            settings: Settings::default(),
        },
        geometry("pg", None, 640.0),
    ]))))
    .expect("applies");
    act(&control, "g", Some("gk"), None).expect("g to gk at once: the default is a cut");
    act(&control, "g", None, Some("move")).expect("g back with a Move");
    let moving: Vec<(String, f32, f32)> = engine
        .render_hub()
        .load()
        .shot()
        .moving()
        .into_iter()
        .map(|(item, track)| match track {
            Track::Pair { from, to, .. } => (
                item.to_string(),
                from.geometry.center.x.get(),
                to.geometry.center.x.get(),
            ),
            other => panic!("not a pair: {other:?}"),
        })
        .collect();
    assert_eq!(moving, [("g1".to_owned(), 150.0, 100.0)]);
}

/// The program mixer follows a switched keyframe: a `when_shown` child the keyframe hides
/// goes silent, and is heard again when the scene switches back (#1171 B1). The engine
/// routes audio from the collection as drawn, which carries the switched keyframes.
#[test]
fn a_child_the_switched_keyframe_hides_is_not_heard() {
    use crate::audio::{MasterMetering, MixerHandles, ScopeTaps};
    use jstream_audio::{Mixer, MixerConfig};
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use std::sync::Arc;

    let clip = jstream_protocol::KindDescriptor {
        kind_id: "clip".into(),
        category: jstream_protocol::KindCategory::Source,
        name: "Clip".into(),
        version: 1,
        video: true,
        audio: true,
        max_outset_px: None,
        params: vec![],
    };
    let engine = Engine::new().with_extra_kinds(vec![clip]);
    let control = connected(&engine);
    let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let (_producer, input) = mixer.add_input(format).expect("input");
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let _detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
        scopes: engine.audio_scopes(),
        taps: Arc::new(ScopeTaps::default()),
        wanted: Arc::default(),
    });
    // `p` goes on program as the first scene; it places group `g`, whose child `g1`
    // plays the clip, and `g`'s keyframe `gk` hides `g1`.
    let list = vec![
        create("p"),
        Edit::SceneCreate {
            scene_id: SceneId::new("g"),
            name: "G".into(),
            role: Some(SceneRole::Group),
        },
        Edit::SourceCreate {
            source_id: SourceId::new("clip"),
            name: "Clip".into(),
            kind: SourceKind::Builtin {
                kind: "clip".into(),
            },
            settings: Settings::default(),
            duplicated_from: None,
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
        add("g", "g1", "clip"),
        variant("gk", "g"),
        Edit::ItemSetVisible {
            item_id: ItemId::new("g1"),
            scene_id: Some(SceneId::new("gk")),
            visible: false,
        },
        add("p", "pg", "sg"),
    ];
    block_on(control.call(Call::Batch(edits(list)))).expect("applies");
    engine.set_audio_input(SourceId::new("clip"), input.clone());
    assert_eq!(input.activity_gain(), 1.0, "shown on program through g");

    assert_eq!(act(&control, "g", Some("gk"), None), Ok(()));
    assert_eq!(input.activity_gain(), 0.0, "the switched keyframe hides g1");
    assert_eq!(act(&control, "g", None, None), Ok(()));
    assert_eq!(input.activity_gain(), 1.0, "g's own values show it again");
}

/// The scenes switched to a keyframe, from `studio.get`.
fn switched(control: &dyn Control) -> Vec<(String, String)> {
    let Ok(QueryResult::StudioGet { keyframes, .. }) = query(control, Query::StudioGet) else {
        panic!("not studio.get");
    };
    keyframes
        .into_iter()
        .map(
            |PlacedKeyframe {
                 scene_id,
                 keyframe_id,
             }| { (scene_id.to_string(), keyframe_id.to_string()) },
        )
        .collect()
}

fn set(scene: &str, keyframe: Option<&str>) -> Action {
    Action::SceneSetKeyframe {
        scene_id: SceneId::new(scene),
        keyframe_id: keyframe.map(SceneId::new),
        preset_id: None,
    }
}

/// On a scene already on program, or one of its keyframe scenes, the scene draws as
/// itself, so a switch with a Move preset starts no transition.
#[test]
fn a_switch_on_the_program_family_starts_no_move() {
    let (engine, control) = three_tracks();
    block_on(control.call(Call::Batch(edits(vec![Edit::TransitionCreate {
        preset_id: TransitionPresetId::new("move"),
        name: "Move".into(),
        kind: "move".into(),
        duration_ms: 400,
        easing: Easing::Linear,
        settings: Settings::default(),
    }]))))
    .expect("applies");
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("gk"),
            preset_id: None,
        },
    }))
    .expect("switch");
    act(&control, "g", Some("gk"), Some("move")).expect("g to gk");
    assert!(
        engine.render_hub().load().shot().tracks().is_none(),
        "no Move"
    );
    assert_eq!(switched(&control), [("g".to_owned(), "gk".to_owned())]);
}

/// A timeline cue switches a track, and a seek runs only the last switch of each scene
/// before where it starts (a state keyed by the scene, ADR-0116 decision 5).
#[test]
fn cues_switch_tracks_and_a_seek_keeps_the_last_for_each_scene() {
    use jstream_protocol::{TimelineCue, TimelineId, TimelineInfo, TimelineLoop};
    let (_engine, control) = three_tracks();
    let cues = vec![
        TimelineCue::new(0, &set("g", Some("gk"))),
        TimelineCue::new(100, &set("g", None)),
        TimelineCue::new(5_000, &set("h", Some("hk"))),
        TimelineCue::new(9_000, &set("j", Some("jk"))),
    ];
    block_on(control.call(Call::Batch(edits(vec![Edit::TimelineSet {
        scene_id: SceneId::new("p"),
        timeline: TimelineInfo {
            timeline_id: TimelineId::new("t"),
            name: String::new(),
            duration_ms: 10_000,
            looping: TimelineLoop::Once,
            cues,
        },
    }]))))
    .expect("applies");
    let timeline_id = TimelineId::new("t");
    block_on(control.call(Call::Action {
        action: Action::TimelinePlay {
            timeline_id: timeline_id.clone(),
        },
    }))
    .expect("play");
    assert_eq!(switched(&control), [("g".to_owned(), "gk".to_owned())]);
    block_on(control.call(Call::Action {
        action: Action::TimelineSeek {
            timeline_id: timeline_id.clone(),
            at_ms: 6_000,
        },
    }))
    .expect("seek");
    assert_eq!(switched(&control), [("h".to_owned(), "hk".to_owned())]);
    block_on(control.call(Call::Action {
        action: Action::TimelineStop { timeline_id },
    }))
    .expect("stop");
}

/// A hotkey bound to a switch goes with the keyframe scene it names (ADR-0040 §3).
#[test]
fn a_hotkey_bound_to_a_switch_goes_with_its_keyframe_scene() {
    let (_engine, control) = three_tracks();
    block_on(control.call(Call::Batch(edits(vec![Edit::HotkeySet {
        chord: jstream_protocol::Chord::new("F6"),
        action: set("g", Some("gk")),
    }]))))
    .expect("bound");
    let deleted = block_on(control.call(Call::Batch(edits(vec![Edit::SceneDelete {
        scene_id: SceneId::new("gk"),
    }]))))
    .expect("deleted");
    let jstream_protocol::CallResult::Batch {
        removed_hotkeys, ..
    } = deleted.result
    else {
        panic!("not a batch result");
    };
    assert_eq!(removed_hotkeys.len(), 1);
}

/// #1174: a placed keyframe is drawn through the engine's graph, not a copy rebuilt on
/// every batch. A batch that does not open live source settings leaves no copy, and
/// the track still draws.
#[test]
fn a_placed_keyframe_does_not_keep_a_drawn_copy() {
    let (engine, control) = three_tracks();
    assert!(!engine.has_settings_copy());
    act(&control, "g", Some("gk"), None).expect("g to gk");
    assert_eq!(placed(&engine, &view("g", Some("gk"))), [150.0]);
    assert!(
        !engine.has_settings_copy(),
        "a placed keyframe rebuilt a copy of the collection"
    );
    block_on(control.call(Call::Batch(edits(vec![Edit::SceneRename {
        scene_id: SceneId::new("p"),
        name: "Program".into(),
    }]))))
    .expect("renamed");
    assert_eq!(placed(&engine, &view("g", Some("gk"))), [150.0]);
    assert!(!engine.has_settings_copy());
}
