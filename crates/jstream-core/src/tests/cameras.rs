//! Camera motion and follow cameras (ADR-0113): the shot redraws every frame while a
//! camera moves on its own, and `scene.zoom_camera` reaches the render thread on the
//! parameter channel.

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
