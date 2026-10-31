//! Studio mode and transitions (T1, ADR-0029): a switch resolves its preset, the program
//! mix draws the transition until it ends on the media clock, a switch mid-way starts
//! from what is on screen, and preview is set without touching program.

use super::{add, batch, block_on, connected, create, drain, edits, query};
use crate::Engine;
use jstream_control::{Control, ControlError};
use jstream_media::Timebase;
use jstream_platform::fake::FakeClock;
use jstream_protocol::{
    Action, Batch, Call, Easing, Edit, Event, Query, QueryResult, SceneId, Settings, SourceId,
    SourceKind, TransitionPresetId, TransitionStarted,
};
use serde_json::json;
use std::sync::Arc;

fn colour(id: &str) -> Edit {
    let settings = [
        ("color", json!("#ff0000ff")),
        ("width", json!(2)),
        ("height", json!(2)),
    ];
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: Settings(
            settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        ),
        duplicated_from: None,
    }
}

fn preset(id: &str, kind: &str, duration_ms: u32) -> Edit {
    Edit::TransitionCreate {
        preset_id: TransitionPresetId::new(id),
        name: id.into(),
        kind: kind.into(),
        duration_ms,
        easing: Easing::Linear,
        settings: Settings::default(),
    }
}

fn switch(control: &dyn Control, scene: &str, preset: Option<&str>) -> Result<(), ControlError> {
    let action = Action::SceneSwitch {
        scene_id: SceneId::new(scene),
        preset_id: preset.map(TransitionPresetId::new),
    };
    block_on(control.call(Call::Action { action })).map(|_| ())
}

/// The preset and length each `studio.changed` in `events` reports.
fn started(events: Vec<Event>) -> Vec<Option<(String, u32)>> {
    events
        .into_iter()
        .filter_map(|event| match event {
            Event::StudioChanged { transition, .. } => Some(transition.map(
                |TransitionStarted {
                     preset_id,
                     duration_ms,
                 }| (preset_id.as_str().to_owned(), duration_ms),
            )),
            _ => None,
        })
        .collect()
}

/// Three scenes, each placing one colour source of its own, built and shown, on an
/// engine whose media clock the test moves.
fn studio() -> (Engine, FakeClock) {
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            create("b"),
            create("c"),
            colour("red"),
            colour("green"),
            colour("blue"),
            add("a", "a1", "red"),
            add("b", "b1", "green"),
            add("c", "c1", "blue"),
            preset("fade", "fade", 400),
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
    (engine, clock)
}

#[test]
fn a_fade_draws_on_program_until_it_ends_on_the_clock() {
    let (engine, clock) = studio();
    let control = connected(&engine);
    let mut events = control.events();
    let hub = engine.render_hub();

    switch(&control, "b", Some("fade")).expect("switch");
    assert_eq!(started(drain(&mut events)), [Some(("fade".into(), 400))]);
    let shot = hub.load();
    assert_eq!(shot.shot().program(), Some(&SceneId::new("b")));
    assert_eq!(
        shot.shot().transition_tracks(),
        Some(2),
        "a1 leaves, b1 arrives"
    );

    clock.advance(399_000);
    engine.replan();
    assert_eq!(
        hub.load().shot().transition_tracks(),
        Some(2),
        "still running"
    );
    clock.advance(1_000);
    engine.replan();
    assert_eq!(hub.load().shot().transition_tracks(), None, "ended");
    assert_eq!(hub.load().shot().program(), Some(&SceneId::new("b")));

    // The default preset is the initial `cut`: no transition at all.
    switch(&control, "a", None).expect("cut back");
    assert_eq!(started(drain(&mut events)), [Some(("cut".into(), 0))]);
    assert_eq!(hub.load().shot().transition_tracks(), None);
}

#[test]
fn a_switch_takes_the_named_preset_then_transition_in_then_transition_out() {
    let (engine, _clock) = studio();
    let control = connected(&engine);
    let mut events = control.events();
    batch(
        &control,
        edits(vec![
            preset("in", "fade", 100),
            preset("out", "fade", 200),
            Edit::SceneSetTransitions {
                scene_id: SceneId::new("b"),
                transition_in: Some(TransitionPresetId::new("in")),
                transition_out: None,
            },
            Edit::SceneSetTransitions {
                scene_id: SceneId::new("a"),
                transition_in: None,
                transition_out: Some(TransitionPresetId::new("out")),
            },
        ]),
    )
    .expect("overrides");
    drain(&mut events);

    switch(&control, "b", None).expect("b's transition_in wins over a's transition_out");
    switch(&control, "a", Some("fade")).expect("a named preset wins");
    switch(&control, "c", None).expect("a's transition_out");
    switch(&control, "a", None).expect("the default");
    assert_eq!(
        started(drain(&mut events)),
        [
            Some(("in".into(), 100)),
            Some(("fade".into(), 400)),
            Some(("out".into(), 200)),
            Some(("cut".into(), 0)),
        ]
    );

    let error = switch(&control, "b", Some("ghost")).expect_err("no such preset");
    assert_eq!(error.protocol().map(|e| e.code()), Some("not_found"));
    assert_eq!(drain(&mut events), []);
    // Switching to the scene already on program does nothing, whatever the preset.
    switch(&control, "a", Some("fade")).expect("same scene");
    assert_eq!(drain(&mut events), []);
}

#[test]
fn a_switch_mid_transition_starts_from_what_is_on_screen() {
    let (engine, clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();

    switch(&control, "b", Some("fade")).expect("to b");
    clock.advance(100_000);
    switch(&control, "c", Some("fade")).expect("retarget to c");
    // a1 and b1 are both partly on screen at 100 ms; both leave, and c1 arrives.
    assert_eq!(hub.load().shot().transition_tracks(), Some(3));
    assert_eq!(hub.load().shot().program(), Some(&SceneId::new("c")));

    clock.advance(400_000);
    engine.replan();
    assert_eq!(hub.load().shot().transition_tracks(), None);
}

#[test]
fn preview_is_set_apart_from_program() {
    let (engine, _clock) = studio();
    let control = connected(&engine);
    let mut events = control.events();
    let studio = |control: &dyn Control| match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet {
            program_scene,
            preview_scene,
        }) => (program_scene, preview_scene),
        other => panic!("not a studio result: {other:?}"),
    };
    let scene = |id: &str| Some(SceneId::new(id));
    assert_eq!(studio(&control), (scene("a"), scene("a")));

    let action = Action::StudioSetPreview {
        scene_id: SceneId::new("c"),
    };
    block_on(control.call(Call::Action { action })).expect("preview c");
    assert_eq!(studio(&control), (scene("a"), scene("c")));
    assert!(matches!(
        drain(&mut events).as_slice(),
        [Event::StudioChanged {
            preview_scene: Some(_),
            transition: None,
            ..
        }]
    ));
    assert_eq!(
        engine.render_hub().load().shot().program(),
        scene("a").as_ref()
    );

    // Deleting the preview scene puts the first scene on preview.
    batch(
        &control,
        Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("c"),
        }),
    )
    .expect("delete c");
    assert_eq!(studio(&control), (scene("a"), scene("a")));
}

/// `video.snapshot` mid-fade shows both scenes mixed, and once the fade ends only the
/// new one. A missing software adapter skips the test, and fails it when `CI` is set.
#[test]
fn a_snapshot_mid_fade_mixes_both_scenes() {
    let _gpu = super::one_gpu_test_at_a_time();
    use crate::snapshot::{decode_base64, decode_png, is_no_adapter};
    use std::io::Write;

    fn fill(id: &str, hex: &str) -> Edit {
        let settings = [
            ("color", json!(hex)),
            ("width", json!(1920)),
            ("height", json!(1080)),
        ];
        Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: id.into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: Settings(
                settings
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            ),
            duplicated_from: None,
        }
    }

    /// The centre pixel of the program mix, or `None` without an adapter.
    fn centre(control: &dyn Control) -> Option<[u8; 4]> {
        let result = match query(control, Query::VideoSnapshot) {
            Ok(result) => result,
            Err(error) if error.protocol().is_some_and(is_no_adapter) => {
                if std::env::var_os("CI").is_some() {
                    panic!("CI has no wgpu adapter: {error}");
                }
                let _ = writeln!(
                    std::io::stderr(),
                    "jstream-core: skipping transition GPU test, no adapter: {error}"
                );
                return None;
            }
            Err(error) => panic!("video.snapshot failed: {error}"),
        };
        let QueryResult::VideoSnapshot { png, .. } = result else {
            panic!("not a snapshot: {result:?}");
        };
        let bytes = decode_base64(&png).expect("png is base64");
        let (width, height, rgba) = decode_png(&bytes).expect("png decodes");
        let i = ((height / 2) as usize * width as usize + (width / 2) as usize) * 4;
        Some([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]])
    }

    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            create("b"),
            fill("red", "#ff0000ff"),
            fill("green", "#00ff00ff"),
            add("a", "a1", "red"),
            add("b", "b1", "green"),
            preset("fade", "fade", 400),
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
    let Some(before) = centre(&control) else {
        return;
    };
    assert_eq!(before, [0xff, 0, 0, 0xff]);

    switch(&control, "b", Some("fade")).expect("switch");
    clock.advance(200_000);
    let [red, green, blue, _] = centre(&control).expect("an adapter");
    assert!(
        (32..224).contains(&red) && (32..224).contains(&green) && blue == 0,
        "half-way, red and green are mixed: {red} {green} {blue}"
    );

    clock.advance(200_000);
    assert_eq!(centre(&control), Some([0, 0xff, 0, 0xff]), "the fade ended");
}

/// A scene with a scene filter (ADR-0065 §6) is one picture in a transition: it arrives
/// and leaves as one track however many items it has, and pairs with nothing.
#[test]
fn a_scene_with_a_filter_transitions_as_one_picture() {
    use jstream_protocol::{FilterChain, FilterId, FilterOwner};
    let (engine, clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();
    batch(
        &control,
        edits(vec![
            add("b", "b2", "blue"),
            add("b", "b3", "red"),
            Edit::FilterAdd {
                owner: FilterOwner::Scene {
                    scene_id: SceneId::new("b"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("grade"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
        ]),
    )
    .expect("b filtered");

    switch(&control, "b", Some("fade")).expect("to b");
    assert_eq!(
        hub.load().shot().transition_tracks(),
        Some(2),
        "a1 leaves, and b arrives whole"
    );
    clock.advance(400_000);
    engine.replan();
    assert_eq!(hub.load().shot().transition_tracks(), None);
    switch(&control, "a", Some("fade")).expect("back to a");
    assert_eq!(
        hub.load().shot().transition_tracks(),
        Some(2),
        "b leaves whole, and a1 arrives"
    );
}

/// A Move pairs two placements of a source only when their effects match (ADR-0065
/// §6): `a1` and `b1` both place `red`, and pair until `b1` gets an effect.
#[test]
fn a_move_pairs_items_only_when_their_effects_match() {
    use jstream_protocol::{FilterChain, FilterId, FilterOwner, ItemId};
    let (engine, clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();
    batch(
        &control,
        edits(vec![
            preset("move", "move", 400),
            Edit::ItemSetSource {
                item_id: ItemId::new("b1"),
                source_id: SourceId::new("red"),
            },
        ]),
    )
    .expect("b1 places red");
    switch(&control, "b", Some("move")).expect("to b");
    assert_eq!(
        hub.load().shot().transition_tracks(),
        Some(1),
        "a1 glides to b1"
    );
    clock.advance(400_000);
    engine.replan();

    batch(
        &control,
        edits(vec![Edit::FilterAdd {
            owner: FilterOwner::Item {
                item_id: ItemId::new("a1"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("sharp"),
            kind: "sharpen".into(),
            settings: Settings::default(),
            enabled: None,
            index: None,
        }]),
    )
    .expect("a1 sharpened");
    switch(&control, "a", Some("move")).expect("back to a");
    assert_eq!(
        hub.load().shot().transition_tracks(),
        Some(2),
        "b1 leaves and a1 arrives: their effects differ"
    );
}

/// A nested scene is flattened into its items for a Move only while it and they all
/// draw `normal` (ADR-0081 §5): its items blend with its own layer, so flattening one
/// with a blend mode would draw something else. Flattened, `n1` in the nested scene
/// pairs with `a1`, both placing `red`; whole, nothing pairs.
#[test]
fn a_nested_scene_with_a_blend_mode_moves_as_one_picture() {
    use jstream_protocol::{BlendMode, ItemId};
    let (engine, clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();
    batch(
        &control,
        edits(vec![
            preset("move", "move", 400),
            create("n"),
            Edit::SourceCreate {
                source_id: SourceId::new("nest"),
                name: "nest".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("n"),
                },
                settings: Settings::default(),
                duplicated_from: None,
            },
            add("n", "n1", "red"),
            add("b", "b2", "nest"),
        ]),
    )
    .expect("b nests n");
    let set = |item: &str, blend: BlendMode| {
        batch(
            &control,
            edits(vec![Edit::ItemSetBlend {
                item_id: ItemId::new(item),
                blend,
            }]),
        )
        .expect("blend set");
    };
    let tracks = |to: &str| {
        switch(&control, to, Some("move")).expect("switched");
        let tracks = hub.load().shot().transition_tracks();
        clock.advance(400_000);
        engine.replan();
        tracks
    };
    assert_eq!(tracks("b"), Some(2), "a1 glides to n1, and b1 arrives");
    assert_eq!(tracks("a"), Some(2));
    set("n1", BlendMode::Multiply);
    assert_eq!(tracks("b"), Some(3), "a1 leaves, b1 and n arrive");
    assert_eq!(tracks("a"), Some(3));
    set("n1", BlendMode::Normal);
    set("b2", BlendMode::Screen);
    assert_eq!(tracks("b"), Some(3), "n is placed with a mode");
}
