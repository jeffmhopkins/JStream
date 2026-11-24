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
    // No client can delete the show scene: to clients it doesn't exist (alpha.13 ST5).
    assert_eq!(
        code(batch(
            &control,
            edits(vec![edit(
                json!({"type": "scene.delete", "scene_id": "show"}),
            )]),
        )),
        "not_found",
        "never deleted by a client"
    );
    assert!(engine.render_hub().load().shot().draws_show());
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

/// `item.show` or `item.hide` on an item of the show scene.
fn shown(control: &dyn Control, item: &str, visible: bool) {
    let item_id = jstream_protocol::ItemId::new(item);
    let action = if visible {
        Action::ItemShow {
            item_id,
            scene_id: None,
        }
    } else {
        Action::ItemHide {
            item_id,
            scene_id: None,
        }
    };
    block_on(control.call(Call::Action { action })).expect("shown or hidden");
}

/// Whether `value` is well between the two ends: a channel partway through a fade.
fn between(value: u8) -> bool {
    (16..240).contains(&value)
}

/// An overlay shown or hidden plays its own effect (ADR-0101) over program, as a show
/// scene item: on its own, so a program transition running at the same time keeps
/// running under it, and the two overlap.
#[test]
fn an_overlay_fades_by_its_own_effect_over_a_program_transition() {
    let (engine, clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();
    batch(
        &control,
        edits(vec![edit(
            json!({"type": "transition.set_default", "preset_id": "fade"}),
        )]),
    )
    .expect("the 400 ms fade is the default");

    // Hidden, it fades out over the default preset's 400 ms, and program stays as it is.
    shown(&control, "overlay", false);
    assert!(hub.load().shot().draws_show(), "still on its way out");
    assert_eq!(
        hub.load().shot().transition_tracks(),
        None,
        "program runs nothing"
    );
    if let Some(pixels) = program_at(&engine, ms(0)) {
        assert_eq!(at(&pixels, 4, 4), BLUE, "not yet faded");
    }
    if let Some(pixels) = program_at(&engine, ms(200)) {
        let [r, g, b, a] = at(&pixels, 4, 4);
        assert!(
            between(r) && between(b) && g == 0 && a == 255,
            "halfway out, over red: {r} {g} {b}"
        );
        assert_eq!(at(&pixels, 48, 27), RED, "program under it");
    }
    clock.advance(400_000);
    engine.replan();
    assert!(!hub.load().shot().draws_show(), "gone once its fade ended");

    // Shown while program fades from red to green: both run at once.
    switch(&control, "b", Some("fade")).expect("switch");
    shown(&control, "overlay", true);
    let shot = hub.load();
    assert_eq!(
        shot.shot().transition_tracks(),
        Some(2),
        "program's fade runs"
    );
    assert!(shot.shot().draws_show(), "and the overlay's over it");
    drop(shot);
    if let Some(pixels) = program_at(&engine, ms(400)) {
        assert_eq!(at(&pixels, 4, 4), RED, "the overlay starts from nothing");
    }
    if let Some(pixels) = program_at(&engine, ms(600)) {
        let [r, g, b, _] = at(&pixels, 4, 4);
        assert!(
            r > 0 && g > 0 && between(b),
            "the overlay halfway in, over red fading into green: {r} {g} {b}"
        );
        let [r, g, b, a] = at(&pixels, 48, 27);
        assert!(
            between(r) && between(g) && b == 0 && a == 255,
            "program halfway: {r} {g} {b}"
        );
    }
    clock.advance(400_000);
    engine.replan();
    let shot = hub.load();
    assert_eq!(shot.shot().transition_tracks(), None, "both ended");
    assert!(shot.shot().draws_show());
    drop(shot);
    if let Some(pixels) = program_at(&engine, ms(800)) {
        assert_eq!(at(&pixels, 4, 4), BLUE);
        assert_eq!(at(&pixels, 48, 27), GREEN);
    }
}

/// The show runner fades an overlay over a length of its own in place of the item's
/// effect, 0 cutting; reversed partway, the fade turns back from where it is.
#[test]
fn the_show_runner_fades_an_overlay_over_its_own_length() {
    let (engine, clock) = studio();
    let hub = engine.render_hub();
    let overlay = jstream_protocol::ItemId::new("overlay");
    engine
        .set_show_scene_item_shown(&overlay, false, Some(0))
        .expect("hidden");
    assert!(!hub.load().shot().draws_show(), "0 cuts");
    engine
        .set_show_scene_item_shown(&overlay, true, Some(1000))
        .expect("shown");
    assert!(hub.load().shot().draws_show());
    let mut half = None;
    if let Some(pixels) = program_at(&engine, ms(0)) {
        assert_eq!(at(&pixels, 4, 4), RED, "not in yet");
    }
    if let Some(pixels) = program_at(&engine, ms(500)) {
        let [r, g, b, _] = at(&pixels, 4, 4);
        assert!(between(r) && g == 0 && between(b), "halfway: {r} {g} {b}");
        half = Some([r, g, b]);
    }
    if let Some(pixels) = program_at(&engine, ms(1000)) {
        assert_eq!(at(&pixels, 4, 4), BLUE, "all the way in");
    }

    // Hidden halfway in, it goes back out from halfway, with what is left of the way.
    clock.advance(500_000);
    engine.replan();
    engine
        .set_show_scene_item_shown(&overlay, false, Some(1000))
        .expect("hidden");
    if let Some(pixels) = program_at(&engine, ms(500)) {
        let [r, g, b, _] = at(&pixels, 4, 4);
        let close = |x: u8, y: u8| x.abs_diff(y) <= 2;
        let [hr, hg, hb] = half.expect("drawn above");
        assert!(
            close(r, hr) && close(g, hg) && close(b, hb),
            "no jump: {r} {g} {b}"
        );
    }
    if let Some(pixels) = program_at(&engine, ms(1000)) {
        assert_eq!(at(&pixels, 4, 4), RED, "out");
    }
    clock.advance(500_000);
    engine.replan();
    assert!(!hub.load().shot().draws_show());

    // Only the show scene's own items.
    let refused = |item: &str| {
        engine
            .set_show_scene_item_shown(&jstream_protocol::ItemId::new(item), true, None)
            .expect_err("refused")
    };
    assert!(matches!(
        refused("a1"),
        jstream_protocol::ProtocolError::InvalidArgument { .. }
    ));
    assert!(matches!(
        refused("missing"),
        jstream_protocol::ProtocolError::NotFound { .. }
    ));
}

/// The show scene's items are drawn items like any pass's: their `shown` clock starts
/// when they first show, and an overlay that moves redraws a still program.
#[test]
fn the_show_scenes_items_are_drawn_items_with_their_own_motion() {
    let (engine, _clock) = studio();
    let control = connected(&engine);
    let hub = engine.render_hub();
    let overlay = jstream_protocol::ItemId::new("overlay");
    assert!(hub.load().shot().drawn_items().contains(&overlay));
    assert!(
        !hub.load().shot().changes_with_time(),
        "a still program with a still overlay"
    );
    batch(
        &control,
        edits(vec![Edit::ItemSetMotion {
            item_id: overlay,
            scene_id: None,
            motion: vec![jstream_protocol::MotionModifier {
                modifier_id: "drift".into(),
                property: jstream_protocol::MotionProperty::X,
                effect: None,
                source: None,
                kind: jstream_protocol::MotionKind::Drift {
                    rate_per_s: jstream_protocol::Finite32::new(10.0).expect("finite"),
                    wrap: None,
                },
                clock: jstream_protocol::MotionClock::Shown,
                enabled: true,
                amount: None,
            }],
        }]),
    )
    .expect("the overlay drifts");
    assert!(
        hub.load().shot().changes_with_time(),
        "the moving overlay redraws"
    );
}
