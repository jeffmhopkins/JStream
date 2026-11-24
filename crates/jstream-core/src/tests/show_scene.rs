//! The internal show scene (alpha.13 show timeline, ST4): a scene the engine draws over
//! program wherever the program mix is drawn, with program, its transitions included, as
//! its bottom layer. Clients never see it in the scene lists, and nothing may show it.

use super::{batch, block_on, connected, edits, query};
use crate::Engine;
use crate::snapshot::{self, DrawAt, LiveParams, NoCaptures, View};
use jstream_control::{Control, ControlError};
use jstream_media::{FrameSize, MediaTime, Timebase};
use jstream_platform::fake::FakeClock;
use jstream_protocol::{Action, Call, Edit, Query, QueryResult, SceneId, SourceId};
use jstream_render::{Compositor, OpenOptions, ResourceCache};
use serde_json::json;
use std::sync::Arc;

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// The canvas, small so a read back is cheap.
const W: u32 = 64;
const H: u32 = 36;

fn edit(value: serde_json::Value) -> Edit {
    serde_json::from_value(value).expect("an edit")
}

/// An item placing `source` in `scene` over the box at `x, y` of `w` by `h`.
fn place(scene: &str, item: &str, source: &str, [x, y, w, h]: [f32; 4]) -> Edit {
    edit(json!({
        "type": "item.add", "scene_id": scene, "item_id": item, "source_id": source,
        "geometry": {"center": [x + w / 2.0, y + h / 2.0], "size": [w, h], "fit_mode": "stretch"}
    }))
}

fn image(id: &str) -> Edit {
    edit(json!({"type": "source.create", "source_id": id, "name": id,
                "kind": {"type": "builtin", "kind": "image"}}))
}

fn scene_source(id: &str, scene: &str) -> Edit {
    edit(json!({"type": "source.create", "source_id": id, "name": id,
                "kind": {"type": "scene", "scene_id": scene}}))
}

fn create(id: &str) -> Edit {
    edit(json!({"type": "scene.create", "scene_id": id, "name": id}))
}

fn switch(control: &dyn Control, scene: &str, preset: Option<&str>) -> Result<(), ControlError> {
    let action = Action::SceneSwitch {
        scene_id: SceneId::new(scene),
        preset_id: preset.map(jstream_protocol::TransitionPresetId::new),
        via: Vec::new(),
    };
    block_on(control.call(Call::Action { action })).map(|_| ())
}

fn code(result: Result<impl std::fmt::Debug, ControlError>) -> &'static str {
    match result {
        Err(error) => super::code(&error).expect("a protocol error"),
        Ok(ok) => panic!("not refused: {ok:?}"),
    }
}

/// Program scenes `a` (all red) and `b` (all green), a scene `logo` with blue in its
/// top-left quarter, and the scene `show`, whose one item places `logo` over the whole
/// canvas, made the show scene. A `fade` of 400 ms is there to switch with.
fn studio() -> (Engine, FakeClock) {
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    let (w, h) = (W as f32, H as f32);
    batch(
        &control,
        edits(vec![
            edit(json!({"type": "canvas.set_size", "width": W, "height": H})),
            create("a"),
            create("b"),
            create("logo"),
            create("show"),
            image("red"),
            image("green"),
            image("blue"),
            scene_source("logo-scene", "logo"),
            place("a", "a1", "red", [0.0, 0.0, w, h]),
            place("b", "b1", "green", [0.0, 0.0, w, h]),
            place("logo", "l1", "blue", [0.0, 0.0, w / 2.0, h / 2.0]),
            place("show", "overlay", "logo-scene", [0.0, 0.0, w, h]),
            edit(
                json!({"type": "transition.create", "preset_id": "fade", "name": "Fade",
                        "kind": "fade", "duration_ms": 400, "easing": "linear",
                        "settings": {}}),
            ),
        ]),
    )
    .expect("built");
    for (source, rgba) in [("red", RED), ("green", GREEN), ("blue", BLUE)] {
        engine
            .set_source_frame(&SourceId::new(source), 1, 1, &rgba)
            .expect("a frame");
    }
    assert!(engine.set_show_scene(Some(SceneId::new("show"))));
    (engine, clock)
}

/// The program mix the engine planned last, drawn at the canvas size at `at`, or
/// `None` without a GPU adapter outside CI.
fn program_at(engine: &Engine, at: MediaTime) -> Option<Vec<[u8; 4]>> {
    program_sized(engine, at, FrameSize::new(W, H).expect("non-zero"))
}

/// [`program_at`] into an output of `size`, which the canvas is fitted into.
fn program_sized(engine: &Engine, at: MediaTime, size: FrameSize) -> Option<Vec<[u8; 4]>> {
    let _gpu = super::one_gpu_test_at_a_time();
    let Ok(mut compositor) = Compositor::open(OpenOptions::hardware()) else {
        assert!(std::env::var_os("CI").is_none(), "CI has no wgpu adapter");
        return None;
    };
    let mut cache = ResourceCache::new();
    let mut chains = crate::chain::Chains::default();
    let hub = engine.render_hub();
    let shot = hub.load();
    cache.begin_frame();
    let output = snapshot::compose(
        &mut compositor,
        (&mut cache, &mut chains),
        (shot.shot(), at),
        View::default(),
        &LiveParams::default(),
        &NoCaptures,
        DrawAt::read(size),
    )
    .expect("compose");
    drop(cache.end_frame(&compositor));
    let frame = output.pixels().expect("read back");
    Some(
        frame
            .rgba()
            .chunks_exact(4)
            .map(|p| [p[0], p[1], p[2], p[3]])
            .collect(),
    )
}

fn at(pixels: &[[u8; 4]], x: u32, y: u32) -> [u8; 4] {
    pixels[(y * W + x) as usize]
}

fn ms(ms: i64) -> MediaTime {
    MediaTime::new(ms * 1000, Timebase::MICROS)
}

#[test]
fn the_show_scene_draws_over_program_and_stays_up_through_a_transition() {
    let (engine, clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();
    assert_eq!(hub.load().shot().program(), Some(&SceneId::new("a")));
    assert!(hub.load().shot().draws_show(), "the overlay is up");
    if let Some(pixels) = program_at(&engine, ms(0)) {
        assert_eq!(at(&pixels, 4, 4), BLUE, "the overlay's quarter");
        assert_eq!(at(&pixels, 48, 27), RED, "program under it");
    }
    // An output of another size gets the same picture, scaled.
    if let Some(pixels) = program_sized(&engine, ms(0), FrameSize::new(32, 18).expect("non-zero")) {
        assert_eq!(pixels[(2 * 32 + 2) as usize], BLUE);
        assert_eq!(pixels[(13 * 32 + 24) as usize], RED);
    }

    // Halfway through a fade from red to green, the overlay is still whole.
    switch(&control, "b", Some("fade")).expect("switch");
    clock.advance(200_000);
    engine.replan();
    let shot = hub.load();
    assert_eq!(shot.shot().transition_tracks(), Some(2));
    assert!(
        shot.shot().draws_show(),
        "the overlay rides over the transition"
    );
    drop(shot);
    if let Some(pixels) = program_at(&engine, ms(200)) {
        assert_eq!(at(&pixels, 4, 4), BLUE, "the overlay doesn't fade");
        let [r, g, b, a] = at(&pixels, 48, 27);
        assert!(
            r > 0 && g > 0 && b == 0 && a == 255,
            "red fading into green: {r} {g}"
        );
    }

    // Hidden, the overlay leaves program drawn as it always was.
    clock.advance(200_000);
    engine.replan();
    block_on(control.call(Call::Action {
        action: Action::ItemHide {
            item_id: jstream_protocol::ItemId::new("overlay"),
            scene_id: None,
        },
    }))
    .expect("hide");
    assert!(!hub.load().shot().draws_show(), "nothing over program");
    if let Some(pixels) = program_at(&engine, ms(400)) {
        assert_eq!(at(&pixels, 4, 4), GREEN);
        assert_eq!(at(&pixels, 48, 27), GREEN);
    }
}

#[test]
fn clients_never_see_the_show_scene_and_nothing_may_show_it() {
    let (engine, _clock) = studio();
    let control = connected(&engine);
    match query(&control, Query::SceneList).expect("scene.list") {
        QueryResult::SceneList { scenes, .. } => {
            let ids: Vec<&str> = scenes.iter().map(|s| s.scene_id.as_str()).collect();
            assert_eq!(ids, ["a", "b", "logo"]);
        }
        other => panic!("not scene.list: {other:?}"),
    }
    match query(&control, Query::StateGet).expect("state.get") {
        QueryResult::StateGet { scenes, .. } => {
            assert!(scenes.iter().all(|s| s.scene_id.as_str() != "show"));
        }
        other => panic!("not state.get: {other:?}"),
    }
    assert_eq!(
        code(switch(&control, "show", None)),
        "invalid_argument",
        "never on program"
    );
    assert_eq!(
        code(block_on(control.call(Call::Action {
            action: Action::StudioSetPreview {
                scene_id: SceneId::new("show"),
            },
        }))),
        "invalid_argument",
        "never on preview"
    );
    assert_eq!(
        code(batch(&control, edits(vec![scene_source("nested", "show")]))),
        "invalid_argument",
        "never placed in a scene"
    );
    assert_eq!(
        code(batch(
            &control,
            edits(vec![edit(json!({
                "type": "video_mix.create", "mix_id": "m", "name": "M",
                "mix": {"input": {"type": "scene", "scene_id": "show"},
                        "resolution": {"width": 64, "height": 36}}
            }))])
        )),
        "invalid_argument",
        "never a mix's scene"
    );
    // A scene something already shows can't become the show scene.
    assert!(!engine.set_show_scene(Some(SceneId::new("logo"))));
    assert!(!engine.set_show_scene(Some(SceneId::new("missing"))));
    // Deleting the show scene takes the marker with it.
    batch(
        &control,
        edits(vec![edit(
            json!({"type": "scene.delete", "scene_id": "show"}),
        )]),
    )
    .expect("deleted");
    assert!(!engine.render_hub().load().shot().draws_show());
}

/// With program on the scene that becomes the show scene, program moves to the first
/// other scene.
#[test]
fn program_leaves_the_scene_that_becomes_the_show_scene() {
    let (engine, _clock) = studio();
    assert!(engine.set_show_scene(None));
    let control = connected(&engine);
    switch(&control, "show", None).expect("an ordinary scene again");
    assert!(engine.set_show_scene(Some(SceneId::new("show"))));
    assert_eq!(
        engine.render_hub().load().shot().program(),
        Some(&SceneId::new("a"))
    );
}
