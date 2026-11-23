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
