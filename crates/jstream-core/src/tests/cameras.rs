//! Camera motion and follow cameras (ADR-0113): the shot redraws every frame while a
//! camera moves on its own, and `scene.zoom_camera` reaches the render thread on the
//! parameter channel; a Follow pointer filter supplies a 2D scene its camera (ADR-0120).

use super::{batch, block_on, code, connected, create, edits};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Batch, Call, CameraFollow, CameraMotionModifier, CameraProperty, Canvas, Edit,
    Finite32, ItemGeometry, ItemId, MotionClock, MotionKind, SceneCamera, SceneId, Size, SourceId,
    SourceKind, Vec2,
};

fn zoom(scene: &str, steps: i32) -> Call {
    Call::Action {
        action: Action::SceneZoomCamera {
            scene_id: SceneId::new(scene),
            steps,
        },
    }
}

/// Scene `a`, 3D, with a display capture `screen` filling most of it, and scene `b`, 2D.
fn scenes() -> Vec<Edit> {
    let geometry = ItemGeometry::try_new(
        Vec2::new(960.0, 540.0).expect("finite"),
        Size::try_new(1440.0, 900.0).expect("size"),
    )
    .expect("geometry");
    vec![
        create("a"),
        create("b"),
        Edit::SourceCreate {
            source_id: SourceId::new("display"),
            name: "Display".into(),
            kind: SourceKind::Builtin {
                kind: "display".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("a"),
            item_id: ItemId::new("screen"),
            source_id: SourceId::new("display"),
            transform: None,
            geometry: Some(geometry),
            opacity: None,
            blend: None,
        },
        Edit::SceneSetCamera {
            scene_id: SceneId::new("a"),
            camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
        },
    ]
}

#[test]
fn a_follow_camera_redraws_every_frame_and_takes_zoom_steps() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    assert!(!hub.load().shot().changes_with_time(), "a still camera");
    let failed = block_on(control.call(zoom("a", 1))).expect_err("no follow camera");
    assert_eq!(code(&failed), Some("action_failed"));

    batch(
        &control,
        Batch::single(Edit::SceneSetCameraFollow {
            scene_id: SceneId::new("a"),
            follow: Some(CameraFollow::of(ItemId::new("screen"))),
        }),
    )
    .expect("follow");
    assert!(hub.load().shot().changes_with_time(), "a follow camera");
    let family = SceneId::new("a");
    block_on(control.call(zoom("a", 2))).expect("zoom in");
    block_on(control.call(zoom("a", -1))).expect("zoom out");
    assert_eq!(hub.load_live().zooms(&family), 1);

    for (scene, steps, expected) in [
        ("a", 17, "invalid_argument"),
        ("b", 1, "action_failed"),
        ("nope", 1, "not_found"),
    ] {
        let failed = block_on(control.call(zoom(scene, steps))).expect_err(scene);
        assert_eq!(code(&failed), Some(expected), "{scene} {steps}");
    }
    assert_eq!(
        hub.load_live().zooms(&family),
        1,
        "a refused zoom takes no step"
    );
}

#[test]
fn camera_motion_redraws_every_frame() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    let sway = CameraMotionModifier {
        modifier_id: "sway".into(),
        property: CameraProperty::Roll,
        kind: MotionKind::Drift {
            rate_per_s: Finite32::ONE,
            wrap: None,
        },
        clock: MotionClock::Engine,
        enabled: true,
    };
    let set = |motion: Vec<CameraMotionModifier>| {
        Batch::single(Edit::SceneSetCameraMotion {
            scene_id: SceneId::new("a"),
            motion,
        })
    };
    batch(&control, set(vec![sway.clone()])).expect("motion");
    assert!(hub.load().shot().changes_with_time(), "a swaying camera");
    let off = CameraMotionModifier {
        enabled: false,
        ..sway
    };
    batch(&control, set(vec![off])).expect("motion off");
    assert!(!hub.load().shot().changes_with_time(), "a disabled sway");
}

/// ADR-0120: a Follow pointer filter on a capture in a 2D scene supplies the scene's
/// camera and moves it every frame; turned off, the scene is still and 2D again.
#[test]
fn a_follow_filter_moves_a_2d_scenes_camera_until_it_is_off() {
    use jstream_protocol::{FOLLOW_POINTER_KIND, FilterChain, FilterId, FilterOwner};
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut flat = scenes();
    flat.pop();
    batch(&control, edits(flat)).expect("scenes");
    assert!(!hub.load().shot().changes_with_time(), "a 2D scene");
    let owner = FilterOwner::Item {
        item_id: ItemId::new("screen"),
    };
    batch(
        &control,
        Batch::single(Edit::FilterAdd {
            owner: owner.clone(),
            chain: FilterChain::Video,
            filter_id: FilterId::new("follow"),
            kind: FOLLOW_POINTER_KIND.into(),
            settings: Default::default(),
            enabled: None,
            index: None,
        }),
    )
    .expect("the filter");
    assert!(hub.load().shot().changes_with_time(), "a follow camera");
    block_on(control.call(zoom("a", 1))).expect("the filter's scene zooms");
    batch(
        &control,
        Batch::single(Edit::FilterSetEnabled {
            owner,
            filter_id: FilterId::new("follow"),
            enabled: false,
        }),
    )
    .expect("off");
    assert!(!hub.load().shot().changes_with_time(), "off: still again");
    let failed = block_on(control.call(zoom("a", 1))).expect_err("no follow camera");
    assert_eq!(code(&failed), Some("action_failed"));
}

fn drive(scene: &str, pan: Option<i8>, tilt: Option<i8>, zoom: Option<i8>) -> Call {
    Call::Action {
        action: Action::SceneDriveCamera {
            scene_id: SceneId::new(scene),
            pan,
            tilt,
            zoom,
        },
    }
}

fn aim_at(scene: &str, pan: f32, tilt: f32, zoom: f32) -> Call {
    Call::Action {
        action: Action::SceneAimCamera {
            scene_id: SceneId::new(scene),
            aim: jstream_protocol::CameraAim {
                pan_deg: Finite32::new(pan),
                tilt_deg: Finite32::new(tilt),
                zoom: Finite32::new(zoom),
            },
            speed: None,
        },
    }
}

fn camera_aim(control: &dyn Control, scene: &str) -> jstream_protocol::CameraAimInfo {
    match super::query(
        control,
        jstream_protocol::Query::SceneCameraAim {
            scene_id: SceneId::new(scene),
        },
    )
    .expect("scene.camera_aim")
    {
        jstream_protocol::QueryResult::SceneCameraAim { aim } => aim,
        other => panic!("{other:?}"),
    }
}

/// ADR-0152: `scene.drive_camera` and `scene.aim_camera` move a scene family's live aim,
/// which reaches the render thread on the parameter channel and `scene.camera_aim`
/// reads; a refused call moves nothing, and a drive stops when its session ends.
#[test]
fn a_scene_cameras_live_aim_is_driven_aimed_and_read() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    batch(
        &control,
        Batch::single(Edit::SceneCreateVariant {
            scene_id: SceneId::new("a2"),
            variant_of: SceneId::new("a"),
            name: "A2".into(),
        }),
    )
    .expect("keyframe");

    let home = camera_aim(&control, "a");
    assert_eq!(
        (
            home.pan_deg.get(),
            home.tilt_deg.get(),
            home.zoom.get(),
            home.moving
        ),
        (0.0, 0.0, 1.0, false),
        "a camera nobody aimed is at its home"
    );
    assert!(!hub.load_live().aims_move());

    // An aim through a keyframe scene aims its family's camera, at once at the fastest
    // speed for a small turn.
    block_on(control.call(aim_at("a2", 1.0, -1.0, 1.0))).expect("aim");
    assert!(
        hub.load_live().aims_move(),
        "the aim reached the render thread"
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let aimed = camera_aim(&control, "a");
    assert_eq!(
        (aimed.pan_deg.get(), aimed.tilt_deg.get(), aimed.zoom.get()),
        (1.0, -1.0, 1.0)
    );
    assert_eq!(camera_aim(&control, "a2"), aimed, "one aim for the family");
    // A zoom to 2 at the fastest speed glides for most of a second.
    block_on(control.call(aim_at("a", 1.0, -1.0, 2.0))).expect("zoom");
    let zooming = camera_aim(&control, "a");
    assert!(zooming.moving && zooming.zoom.get() < 2.0, "{zooming:?}");
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(camera_aim(&control, "a").zoom.get(), 2.0, "there");

    // A drive moves it until a stop.
    block_on(control.call(drive("a", Some(24), None, None))).expect("drive");
    assert!(camera_aim(&control, "a").moving);
    std::thread::sleep(std::time::Duration::from_millis(20));
    block_on(control.call(drive("a", Some(0), None, None))).expect("stop");
    let stopped = camera_aim(&control, "a");
    assert!(
        !stopped.moving && stopped.pan_deg.get() > 1.0,
        "{stopped:?}"
    );

    for (call, expected) in [
        (drive("a", Some(25), None, None), "invalid_argument"),
        (drive("a", None, None, None), "invalid_argument"),
        (aim_at("a", 171.0, 0.0, 1.0), "invalid_argument"),
        (aim_at("a", 0.0, 0.0, 9.0), "invalid_argument"),
        (drive("b", Some(1), None, None), "action_failed"),
        (aim_at("b", 0.0, 0.0, 1.0), "action_failed"),
        (drive("nope", Some(1), None, None), "not_found"),
    ] {
        let failed = block_on(control.call(call.clone())).expect_err("refused");
        assert_eq!(code(&failed), Some(expected), "{call:?}");
    }
    assert_eq!(
        camera_aim(&control, "a"),
        stopped,
        "a refused call moves nothing"
    );

    // `scene.drive_camera` can't be bound; `scene.aim_camera` can.
    let bind = |action: Action| {
        batch(
            &control,
            Batch::single(Edit::HotkeySet {
                chord: jstream_protocol::Chord::new("F1"),
                action,
            }),
        )
    };
    let refused = bind(Action::SceneDriveCamera {
        scene_id: SceneId::new("a"),
        pan: Some(4),
        tilt: None,
        zoom: None,
    })
    .expect_err("a drive can't be bound");
    assert_eq!(code(&refused), Some("invalid_argument"));

    // A drive belongs to its session: when that session ends, the camera stops.
    let other = connected(&engine);
    block_on(other.call(drive("a", None, Some(-23), None))).expect("drive");
    assert!(camera_aim(&control, "a").moving);
    drop(other);
    let left = camera_aim(&control, "a");
    assert!(!left.moving, "{left:?}");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(
        camera_aim(&control, "a"),
        left,
        "it stays where the drive left it"
    );
}
