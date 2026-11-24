//! 3D groups (ADR-0164): a group that shares its 3D scene's space draws its children in
//! that space, each sorting on its own; the group item's geometry and opacity move and
//! fade them all; and a camera anchored to the group rides with it. Each test draws the
//! program with `video.snapshot`; a missing software adapter skips it, and fails it
//! when `CI` is set.

use super::transitions::fill;
use super::{batch, connected, create, edits, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Canvas, Edit, Finite32, FitMode, ItemGeometry, ItemId, Query, QueryResult, SceneCamera,
    SceneId, SceneOptions, SceneRole, SourceId, SourceKind, Vec2,
};

/// The program's pixel at `(x, y)` on the 1920x1080 canvas, or `None` without an
/// adapter.
fn pixel(control: &dyn Control, x: u32, y: u32) -> Option<[u8; 4]> {
    let result = match query(control, Query::VideoSnapshot) {
        Ok(result) => result,
        Err(error) if error.protocol().is_some_and(crate::snapshot::is_no_adapter) => {
            if std::env::var_os("CI").is_some() {
                panic!("CI has no wgpu adapter: {error}");
            }
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("jstream-core: skipping 3D group GPU test, no adapter: {error}\n"),
            );
            return None;
        }
        Err(error) => panic!("video.snapshot failed: {error}"),
    };
    let QueryResult::VideoSnapshot { png, .. } = result else {
        panic!("not a snapshot: {result:?}");
    };
    let bytes = crate::snapshot::decode_base64(&png).expect("png is base64");
    let (width, height, rgba) = crate::snapshot::decode_png(&bytes).expect("png decodes");
    // The snapshot may be smaller than the canvas: the point's place in it.
    let (x, y) = (x * width / 1920, y * height / 1080);
    let i = (y as usize * width as usize + x as usize) * 4;
    Some([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]])
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

/// A box of `width` by `height` centred at `(x, y)`, `depth` toward the camera, its
/// source stretched to fill it.
fn boxed(x: f32, y: f32, (width, height): (f32, f32), depth: f32) -> ItemGeometry {
    let mut geometry = ItemGeometry::covering_canvas(1920, 1080).expect("a canvas");
    geometry.center = Vec2 {
        x: finite(x),
        y: finite(y),
    };
    geometry.size.width = finite(width);
    geometry.size.height = finite(height);
    geometry.depth_px = finite(depth);
    geometry.fit_mode = FitMode::Stretch;
    geometry
}

fn place(scene: &str, item: &str, source: &str, geometry: ItemGeometry) -> Edit {
    Edit::ItemAdd {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        transform: None,
        geometry: Some(geometry),
        opacity: None,
        blend: None,
    }
}

fn move_to(item: &str, geometry: ItemGeometry) -> Edit {
    Edit::ItemSetGeometry {
        item_id: ItemId::new(item),
        scene_id: None,
        geometry,
    }
}

fn share(on: bool) -> Edit {
    Edit::SceneSetOptions {
        scene_id: SceneId::new("rig"),
        options: SceneOptions {
            share_space: on,
            reflections: false,
            ..SceneOptions::default()
        },
    }
}

const RED: [u8; 4] = [0xff, 0, 0, 0xff];
const BLUE: [u8; 4] = [0, 0, 0xff, 0xff];

/// Scene `main`, 3D through the framing camera, placing group `rig` over the whole
/// canvas as `rig_item`, then a blue card over the whole canvas at the same depth, on
/// top. The group holds one red card, 400 px square at its centre, 200 px toward the
/// camera. Returns the connected engine, with its sources built.
fn rig(share_space: bool) -> (Engine, crate::InProcessControl) {
    rig_with(share_space, Engine::new())
}

/// [`rig`] in `engine`.
fn rig_with(share_space: bool, engine: Engine) -> (Engine, crate::InProcessControl) {
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("main"),
            create("rig"),
            Edit::SceneSetRole {
                scene_id: SceneId::new("rig"),
                role: SceneRole::Group,
            },
            share(share_space),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
            },
            fill("red", "#ff0000ff"),
            fill("blue", "#0000ffff"),
            Edit::SourceCreate {
                source_id: SourceId::new("rig_src"),
                name: "Rig".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("rig"),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
            place(
                "rig",
                "part",
                "red",
                boxed(960.0, 540.0, (400.0, 400.0), 200.0),
            ),
            place(
                "main",
                "rig_item",
                "rig_src",
                boxed(960.0, 540.0, (1920.0, 1080.0), 0.0),
            ),
            place(
                "main",
                "wall",
                "blue",
                boxed(960.0, 540.0, (1920.0, 1080.0), 0.0),
            ),
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
    (engine, control)
}

/// Shared, the group's red child is 200 px nearer than the wall and sorts in front of
/// it; flat, the group is one card at the wall's depth, under it in stacking order.
#[test]
fn a_3d_groups_children_sort_with_the_scenes_items() {
    let _gpu = super::one_gpu_test_at_a_time();
    let (_engine, control) = rig(false);
    let Some(flat) = pixel(&control, 960, 540) else {
        return;
    };
    assert_eq!(flat, BLUE, "a flat group is a card under the wall");
    batch(&control, edits(vec![share(true)])).expect("share");
    assert_eq!(
        pixel(&control, 960, 540),
        Some(RED),
        "its child sorts in front"
    );
}

/// Moving the group item moves its child; fading it fades the child.
#[test]
fn the_group_item_moves_and_fades_its_children() {
    let _gpu = super::one_gpu_test_at_a_time();
    let (_engine, control) = rig(true);
    if pixel(&control, 960, 540).is_none() {
        return;
    }
    batch(
        &control,
        edits(vec![move_to(
            "rig_item",
            boxed(1560.0, 540.0, (1920.0, 1080.0), 0.0),
        )]),
    )
    .expect("move");
    // 600 px right at the child's depth, magnified by `f / (f - 200)` through the
    // framing camera.
    let focal = 540.0 / 22.5_f32.to_radians().tan();
    let x = 960.0 + 600.0 * focal / (focal - 200.0);
    assert_eq!(pixel(&control, 960, 540), Some(BLUE), "the child left");
    assert_eq!(
        pixel(&control, x as u32, 540),
        Some(RED),
        "it is where it moved"
    );

    batch(
        &control,
        edits(vec![
            Edit::ItemRemove {
                item_id: ItemId::new("wall"),
            },
            Edit::ItemSetOpacity {
                item_id: ItemId::new("rig_item"),
                scene_id: None,
                opacity: finite(0.5),
            },
        ]),
    )
    .expect("fade");
    let [red, green, blue, _] = pixel(&control, x as u32, 540).expect("an adapter");
    assert!(
        (64..200).contains(&red) && green == 0 && blue == 0,
        "half the group's opacity: {red} {green} {blue}"
    );
}

/// A camera anchored to the group rides with it: moving the group item leaves its child
/// where it was on the picture.
#[test]
fn an_anchored_camera_rides_with_its_group() {
    let _gpu = super::one_gpu_test_at_a_time();
    let (_engine, control) = rig(true);
    if pixel(&control, 960, 540).is_none() {
        return;
    }
    batch(
        &control,
        edits(vec![
            Edit::SceneSetCameraAnchor {
                scene_id: SceneId::new("main"),
                anchor: Some(ItemId::new("rig_item")),
            },
            Edit::ItemRemove {
                item_id: ItemId::new("wall"),
            },
            move_to("rig_item", boxed(1560.0, 300.0, (1920.0, 1080.0), -400.0)),
        ]),
    )
    .expect("anchor and move");
    assert_eq!(
        pixel(&control, 960, 540),
        Some(RED),
        "the camera moved with the group"
    );
    batch(
        &control,
        edits(vec![Edit::SceneSetCameraAnchor {
            scene_id: SceneId::new("main"),
            anchor: None,
        }]),
    )
    .expect("unanchor");
    assert_eq!(
        pixel(&control, 960, 540),
        Some([0, 0, 0, 0xff]),
        "without it, the child is off the centre"
    );
}

/// A Move between two views of a 3D scene with a video chain moves item by item through
/// the moving camera (ADR-0180), and keeps a shared group's children in the scene's
/// space while it runs: the red child, 200 px nearer than the wall, stays in front of
/// it half-way through, as it is at either end.
#[test]
fn a_3d_groups_children_stay_in_its_space_during_a_move() {
    use jstream_media::Timebase;
    use jstream_platform::fake::FakeClock;
    use jstream_protocol::{Action, Call, Easing, Settings, TransitionPresetId};
    let _gpu = super::one_gpu_test_at_a_time();
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(std::sync::Arc::new(clock.clone()));
    let (engine, control) = rig_with(true, engine);
    if pixel(&control, 960, 540).is_none() {
        return;
    }
    batch(
        &control,
        edits(vec![
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("k"),
                variant_of: SceneId::new("main"),
                name: "K".into(),
            },
            Edit::ItemSetGeometry {
                item_id: ItemId::new("wall"),
                scene_id: Some(SceneId::new("k")),
                geometry: boxed(960.0, 540.0, (1920.0, 1080.0), 100.0),
            },
            // The scene has a chain, as a 3D set with depth effects has: the Move still
            // moves by item (ADR-0180).
            Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Scene {
                    scene_id: SceneId::new("main"),
                },
                chain: jstream_protocol::FilterChain::Video,
                filter_id: jstream_protocol::FilterId::new("look"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
            // The child has an effect, so it is drawn in a box of its own.
            Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Item {
                    item_id: ItemId::new("part"),
                },
                chain: jstream_protocol::FilterChain::Video,
                filter_id: jstream_protocol::FilterId::new("fx"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
            // The view moves too: the camera steps back.
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some({
                    let mut camera = SceneCamera::framing(Canvas::DEFAULT);
                    camera.position.z = finite(camera.position.z.get() + 300.0);
                    camera
                }),
            },
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("move"),
                name: "Move".into(),
                kind: "move".into(),
                duration_ms: 400,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
        ]),
    )
    .expect("a keyframe and a move");
    let switch = |scene: &str| {
        let action = Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: Some(TransitionPresetId::new("move")),
            via: Vec::new(),
        };
        super::block_on(control.call(Call::Action { action })).expect("switch");
    };
    switch("k");
    clock.advance(200_000);
    engine.replan();
    let loaded = engine.render_hub().load();
    let shot = loaded.shot();
    assert!(shot.transitioning(), "the Move runs");
    let cameras = shot.track_cameras(200.0).expect("tracks");
    assert!(
        cameras.len() > 1 && cameras.iter().all(|camera| camera.is_some()),
        "item by item, through the moving camera: {cameras:?}"
    );
    assert_eq!(
        pixel(&control, 960, 540),
        Some(RED),
        "the child is still in front of the wall half-way"
    );
}

/// An item with an effect shown with a Fade on a keyframe scene of a 3D scene is drawn
/// through the keyframe's camera while it appears: the red card, 400 px square, fades in
/// at its place, and the wall around it stays as it is.
#[test]
fn an_item_shown_on_a_3d_keyframe_is_drawn_through_its_camera() {
    use jstream_media::Timebase;
    use jstream_platform::fake::FakeClock;
    use jstream_protocol::{Action, Call, Easing, Settings, TransitionPresetId};
    let _gpu = super::one_gpu_test_at_a_time();
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(std::sync::Arc::new(clock.clone()));
    let (engine, control) = rig_with(true, engine);
    if pixel(&control, 960, 540).is_none() {
        return;
    }
    let call = |action: Action| {
        super::block_on(control.call(Call::Action { action })).expect("action");
    };
    batch(
        &control,
        edits(vec![
            Edit::ItemRemove {
                item_id: ItemId::new("rig_item"),
            },
            place(
                "main",
                "card",
                "red",
                boxed(960.0, 540.0, (400.0, 400.0), 200.0),
            ),
            Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Item {
                    item_id: ItemId::new("card"),
                },
                chain: jstream_protocol::FilterChain::Video,
                filter_id: jstream_protocol::FilterId::new("fx"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
            // The scene has a chain: a transition draws it as one picture.
            Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Scene {
                    scene_id: SceneId::new("main"),
                },
                chain: jstream_protocol::FilterChain::Video,
                filter_id: jstream_protocol::FilterId::new("look"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
            Edit::ItemSetVisible {
                item_id: ItemId::new("card"),
                scene_id: None,
                visible: false,
            },
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("k"),
                variant_of: SceneId::new("main"),
                name: "K".into(),
            },
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some({
                    let mut camera = SceneCamera::framing(Canvas::DEFAULT);
                    camera.position.z = finite(camera.position.z.get() + 300.0);
                    camera
                }),
            },
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("fade"),
                name: "Fade".into(),
                kind: "fade".into(),
                duration_ms: 400,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
        ]),
    )
    .expect("a card to show and a keyframe");
    call(Action::SceneSwitch {
        scene_id: SceneId::new("k"),
        preset_id: None,
        via: Vec::new(),
    });
    batch(
        &control,
        edits(vec![Edit::TransitionSetDefault {
            preset_id: TransitionPresetId::new("fade"),
        }]),
    )
    .expect("fade by default");
    assert_eq!(pixel(&control, 200, 200), Some(BLUE), "the wall, before");
    call(Action::ItemShow {
        item_id: ItemId::new("card"),
        scene_id: None,
    });
    clock.advance(200_000);
    engine.replan();
    assert!(
        engine.render_hub().load().shot().transitioning(),
        "it appears"
    );
    // The scene has a chain, so it crossfades as one picture (ADR-0065 §6): the wall
    // is still there, seen through k's camera, not the picture seen through it again.
    let [red, green, blue, _] = pixel(&control, 200, 200).expect("an adapter");
    assert!(
        red == 0 && green == 0 && blue > 150,
        "the wall, while it appears: {red} {green} {blue}"
    );
    let [red, green, _, _] = pixel(&control, 960, 540).expect("an adapter");
    assert!(
        red > 64 && green == 0,
        "the card coming in at its place: {red} {green}"
    );
    clock.advance(400_000);
    engine.replan();
    assert_eq!(pixel(&control, 200, 200), Some(BLUE), "the wall, after");
    assert_eq!(pixel(&control, 960, 540), Some(RED), "the card, after");
}

/// An effect's setting changed after its first frame reaches the card in a 3D scene:
/// the chain draws into the same layer, which is marked changed so the card's mips are
/// made again (ADR-0050 decision 5). The hologram pop-up drives `reveal` this way.
#[test]
fn an_effect_setting_changed_later_reaches_a_3d_card() {
    use jstream_protocol::{FilterChain, FilterId, FilterOwner};
    let _gpu = super::one_gpu_test_at_a_time();
    let engine = Engine::new();
    let control = connected(&engine);
    let owner = FilterOwner::Item {
        item_id: ItemId::new("card"),
    };
    let brightness = |value: f64| {
        serde_json::from_value(serde_json::json!({ "brightness": value })).expect("settings")
    };
    batch(
        &control,
        edits(vec![
            create("main"),
            fill("red", "#ff0000ff"),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
            },
            place(
                "main",
                "card",
                "red",
                boxed(960.0, 540.0, (400.0, 400.0), 200.0),
            ),
            Edit::FilterAdd {
                owner: owner.clone(),
                chain: FilterChain::Video,
                filter_id: FilterId::new("fx"),
                kind: "color_correction".into(),
                settings: brightness(-1.0),
                enabled: None,
                index: None,
            },
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
    let set = |value: f64| {
        batch(
            &control,
            edits(vec![Edit::FilterSetSettings {
                owner: owner.clone(),
                filter_id: FilterId::new("fx"),
                patch: brightness(value),
            }]),
        )
        .expect("set");
    };
    let Some(dark) = pixel(&control, 960, 540) else {
        return;
    };
    assert_eq!(dark, [0, 0, 0, 0xff], "at brightness -1 the card is black");
    set(0.0);
    assert_eq!(pixel(&control, 960, 540), Some(RED), "then red");
    set(-1.0);
    assert_eq!(
        pixel(&control, 960, 540),
        Some([0, 0, 0, 0xff]),
        "then black again"
    );
}

/// #1632: a Move between two views of a 3D scene drawn as one picture through its chain,
/// started once the Fade onto it from a 2D scene has ended but before a plan let that
/// go, moves item by item as from the scene at rest, so the group's red child, 200 px
/// nearer than the wall, stays in front of it half-way; it is not the Fade's flat
/// picture of the scene leaving under the keyframe's items.
#[test]
fn a_3d_groups_children_stay_in_its_space_in_a_move_after_a_fade() {
    use jstream_media::Timebase;
    use jstream_platform::fake::FakeClock;
    use jstream_protocol::{Action, Call, Easing, Settings, TransitionPresetId};
    let _gpu = super::one_gpu_test_at_a_time();
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new();
    engine.set_source_clock(std::sync::Arc::new(clock.clone()));
    let (engine, control) = rig_with(true, engine);
    if pixel(&control, 960, 540).is_none() {
        return;
    }
    let preset = |id: &str, kind: &str| Edit::TransitionCreate {
        preset_id: TransitionPresetId::new(id),
        name: id.into(),
        kind: kind.into(),
        duration_ms: 400,
        easing: Easing::Linear,
        settings: Settings::default(),
    };
    batch(
        &control,
        edits(vec![
            create("z"),
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("k"),
                variant_of: SceneId::new("main"),
                name: "K".into(),
            },
            Edit::SceneSetCamera {
                scene_id: SceneId::new("k"),
                camera: Some({
                    let mut camera = SceneCamera::framing(Canvas::DEFAULT);
                    camera.position.z = finite(camera.position.z.get() + 300.0);
                    camera
                }),
            },
            // The scene has a chain, as a 3D set with depth effects has: a Fade onto it
            // from another scene shows it as one picture (ADR-0065 §6).
            Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Scene {
                    scene_id: SceneId::new("main"),
                },
                chain: jstream_protocol::FilterChain::Video,
                filter_id: jstream_protocol::FilterId::new("look"),
                kind: "sharpen".into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            },
            preset("move", "move"),
            preset("fade", "fade"),
        ]),
    )
    .expect("a keyframe and presets");
    let switch = |scene: &str, preset: Option<&str>| {
        let action = Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: preset.map(TransitionPresetId::new),
            via: Vec::new(),
        };
        super::block_on(control.call(Call::Action { action })).expect("switch");
    };
    switch("z", None);
    switch("main", Some("fade"));
    // Past the Fade's end, with no plan since to let it go.
    clock.advance(600_000);
    switch("k", Some("move"));
    clock.advance(200_000);
    engine.replan();
    assert!(
        engine.render_hub().load().shot().transitioning(),
        "the Move runs"
    );
    assert_eq!(
        pixel(&control, 960, 540),
        Some(RED),
        "the child is still in front of the wall half-way"
    );
}

// Follow-ups of #1591's review (#1596): group motion while a transition runs, the pass
// and the transition splitting the same groups, and a legacy-transform child.

/// `item`'s motion: one drift along x at `rate` pixels a second on the engine's clock.
fn drift_x(item: &str, rate: f32) -> Edit {
    Edit::ItemSetMotion {
        item_id: ItemId::new(item),
        scene_id: None,
        motion: vec![jstream_protocol::MotionModifier {
            mode: jstream_protocol::MappingMode::Add,
            modifier_id: "drift".into(),
            property: jstream_protocol::MotionProperty::X,
            effect: None,
            source: None,
            kind: jstream_protocol::MotionKind::Drift {
                rate_per_s: finite(rate),
                wrap: None,
            },
            clock: jstream_protocol::MotionClock::Engine,
            enabled: true,
            amount: None,
        }],
    }
}

fn secs(s: f64) -> jstream_media::MediaTime {
    jstream_media::MediaTime::new((s * 1e6) as i64, jstream_media::Timebase::MICROS)
}

/// [`rig`] with `more` applied, then a keyframe scene `k` of `main` that only moves the
/// wall nearer, and a 400 ms Move from `main` to it started: the engine, mid-switch.
fn moving_rig(more: Vec<Edit>) -> Engine {
    use jstream_protocol::{Action, Call, Easing, Settings, TransitionPresetId};
    let (engine, control) = rig(true);
    let mut list = more;
    list.extend([
        Edit::SceneCreateVariant {
            scene_id: SceneId::new("k"),
            variant_of: SceneId::new("main"),
            name: "K".into(),
        },
        Edit::ItemSetGeometry {
            item_id: ItemId::new("wall"),
            scene_id: Some(SceneId::new("k")),
            geometry: boxed(960.0, 540.0, (1920.0, 1080.0), 100.0),
        },
        Edit::TransitionCreate {
            preset_id: TransitionPresetId::new("move"),
            name: "Move".into(),
            kind: "move".into(),
            duration_ms: 400,
            easing: Easing::Linear,
            settings: Settings::default(),
        },
    ]);
    batch(&control, edits(list)).expect("a keyframe and a move");
    assert!(engine.settle_sources(), "the sources were built in time");
    let action = Action::SceneSwitch {
        scene_id: SceneId::new("k"),
        preset_id: Some(TransitionPresetId::new("move")),
        via: Vec::new(),
    };
    super::block_on(control.call(Call::Action { action })).expect("switch");
    engine
}

/// The centre and width of each of the running transition's tracks `elapsed_ms` into it,
/// at `at`, with their motion, in drawing order.
fn tracks_at(engine: &Engine, elapsed_ms: f32, at: f64) -> Vec<(f32, f32, f32)> {
    engine
        .render_hub()
        .load()
        .shot()
        .tracks_moved_at(elapsed_ms, secs(at))
        .expect("a transition runs")
        .into_iter()
        .flatten()
        .map(|end| {
            let g = end.geometry;
            (g.center.x.get(), g.center.y.get(), g.size.width.get())
        })
        .collect()
}

/// A 3D group item's motion moves its children while a transition runs, as it does in
/// the scene's pass (#1596 N1, ADR-0164 §4): two seconds into its 100 px a second drift,
/// half-way through a Move, the red child is 200 px right of where it is planned.
#[test]
fn a_3d_groups_motion_moves_its_children_during_a_transition() {
    let engine = moving_rig(vec![drift_x("rig_item", 100.0)]);
    let tracks = tracks_at(&engine, 200.0, 2.0);
    assert!(
        tracks.contains(&(1160.0, 540.0, 400.0)),
        "the child moved with its group: {tracks:?}"
    );
    assert!(
        tracks.contains(&(960.0, 540.0, 1920.0)),
        "the wall stays: {tracks:?}"
    );
}

/// A child's own motion moves it in its group's space while a transition runs, as it
/// does in the scene's pass (#1596 N1): in a group turned half a turn in the canvas
/// plane, the child's drift to the right moves it left on the canvas.
#[test]
fn a_3d_groups_childs_motion_is_in_its_groups_space_during_a_transition() {
    let mut turned = boxed(960.0, 540.0, (1920.0, 1080.0), 0.0);
    turned.rotation_deg = finite(180.0);
    let engine = moving_rig(vec![move_to("rig_item", turned), drift_x("part", 100.0)]);
    let tracks = tracks_at(&engine, 200.0, 2.0);
    assert!(
        tracks.iter().any(|&(x, y, width)| (x - 760.0).abs() < 0.01
            && (y - 540.0).abs() < 0.01
            && width == 400.0),
        "the child drifted left, in its turned group's space: {tracks:?}"
    );
}

/// A second item placing the same 3D group draws it flat in the scene's pass, since a
/// group shares a scene's space once; a transition takes it the same way, so the two
/// agree on what is drawn (#1596 N2): at the start of a Move, the tracks are the pass's
/// items at the same sizes, the flat group's card among them.
#[test]
fn a_transition_splits_the_groups_the_scenes_pass_splits() {
    let engine = moving_rig(vec![place(
        "main",
        "rig_again",
        "rig_src",
        boxed(480.0, 270.0, (960.0, 540.0), 0.0),
    )]);
    let mut widths: Vec<f32> = tracks_at(&engine, 0.0, 0.0)
        .into_iter()
        .map(|(_, _, width)| width)
        .collect();
    widths.sort_by(f32::total_cmp);
    assert_eq!(
        widths,
        [400.0, 960.0, 1920.0],
        "one child, the second placement's card and the wall"
    );
}

/// A run of `levels` 3D groups, each placing the next over its whole canvas, the last
/// holding the red card, inside scene `s`, 3D through its own camera, which `main`
/// places over its whole canvas: so a Move flattens `s` into `main`'s tracks.
fn deep_rig(levels: usize) -> Vec<Edit> {
    let mut list = vec![
        create("s"),
        Edit::SceneSetCamera {
            scene_id: SceneId::new("s"),
            camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
        },
        Edit::SourceCreate {
            source_id: SourceId::new("s_src"),
            name: "S".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("s"),
            },
            settings: Default::default(),
            duplicated_from: None,
        },
        Edit::ItemRemove {
            item_id: ItemId::new("rig_item"),
        },
        place(
            "main",
            "s_item",
            "s_src",
            boxed(960.0, 540.0, (1920.0, 1080.0), 0.0),
        ),
    ];
    let whole = boxed(960.0, 540.0, (1920.0, 1080.0), 0.0);
    for level in 0..levels {
        let group = format!("g{level}");
        list.extend([
            create(&group),
            Edit::SceneSetRole {
                scene_id: SceneId::new(&group),
                role: SceneRole::Group,
            },
            Edit::SceneSetOptions {
                scene_id: SceneId::new(&group),
                options: SceneOptions {
                    share_space: true,
                    reflections: false,
                    ..SceneOptions::default()
                },
            },
            Edit::SourceCreate {
                source_id: SourceId::new(format!("{group}_src")),
                name: group.to_uppercase(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new(&group),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
        ]);
        let parent = match level {
            0 => String::from("s"),
            _ => format!("g{}", level - 1),
        };
        list.push(place(
            &parent,
            &format!("{group}_item"),
            &format!("{group}_src"),
            whole,
        ));
    }
    list.push(place(
        &format!("g{}", levels - 1),
        "deep_part",
        "red",
        boxed(960.0, 540.0, (400.0, 400.0), 200.0),
    ));
    list
}

/// Groups nested as deep as a scene's pass splits them are split as deep by a
/// transition, which counts groups as the pass does, not the nested scenes it flattens
/// on the way (#1596 N2): with the most groups a pass splits, inside a scene a Move
/// flattens, the innermost child is still a track of its own, 400 px wide, rather than
/// the innermost group's card over the canvas.
#[test]
fn a_transition_nests_3d_groups_as_deep_as_the_pass() {
    let engine = moving_rig(deep_rig(crate::snapshot::MAX_NESTING - 1));
    let widths: Vec<f32> = tracks_at(&engine, 0.0, 0.0)
        .into_iter()
        .map(|(_, _, width)| width)
        .collect();
    assert!(
        widths.contains(&400.0),
        "the innermost child is its own track: {widths:?}"
    );
}

/// A 3D group's child with a legacy transform is placed through the group as any child
/// is, as the box its transform draws (#1596 N3, ADR-0164 §1 and §2): here a fifth of
/// the red source's size at the group's centre, in a group moved 600 px right.
#[test]
fn a_3d_groups_legacy_transform_child_is_placed_through_it() {
    use jstream_protocol::ItemTransform;
    let _gpu = super::one_gpu_test_at_a_time();
    let (_engine, control) = rig(true);
    batch(
        &control,
        edits(vec![
            Edit::ItemRemove {
                item_id: ItemId::new("wall"),
            },
            Edit::ItemRemove {
                item_id: ItemId::new("part"),
            },
            Edit::ItemAdd {
                scene_id: SceneId::new("rig"),
                item_id: ItemId::new("legacy"),
                source_id: SourceId::new("red"),
                transform: Some(ItemTransform {
                    position: Vec2 {
                        x: finite(768.0),
                        y: finite(432.0),
                    },
                    scale: Vec2 {
                        x: finite(0.2),
                        y: finite(0.2),
                    },
                    rotation_deg: finite(0.0),
                }),
                geometry: None,
                opacity: None,
                blend: None,
            },
            move_to("rig_item", boxed(1560.0, 540.0, (1920.0, 1080.0), 0.0)),
        ]),
    )
    .expect("a legacy child");
    let Some(placed) = pixel(&control, 1560, 540) else {
        return;
    };
    assert_eq!(placed, RED, "the child, where the group places it");
    assert_eq!(
        pixel(&control, 960, 540),
        Some([0, 0, 0, 0xff]),
        "not where it is in the group's own canvas"
    );
}
