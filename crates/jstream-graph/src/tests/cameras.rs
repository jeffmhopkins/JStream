//! Scene cameras (ADR-0094): `scene.set_camera`, its checks and inverse, a keyframe
//! scene's override, removing a parent's camera, deleting a scene, and the collection
//! file.

use super::{add, apply_err, apply_ok, fixture, invalid_reason};
use crate::Graph;
use jstream_protocol::{
    CameraFollow, CameraMotionModifier, CameraProperty, Canvas, Edit, Finite32, ItemId,
    MotionClock, MotionKind, MotionShape, SceneCamera, SceneId, Vec3,
};

fn framing() -> SceneCamera {
    SceneCamera::framing(Canvas::DEFAULT)
}

fn moved(x: f32) -> SceneCamera {
    let mut camera = framing();
    camera.position = Vec3::new(x, 200.0, 1500.0).expect("finite");
    camera
}

fn set(scene: &str, camera: Option<SceneCamera>) -> Edit {
    Edit::SceneSetCamera {
        scene_id: SceneId::new(scene),
        camera,
    }
}

fn variant(id: &str, of: &str) -> Edit {
    Edit::SceneCreateVariant {
        scene_id: SceneId::new(id),
        variant_of: SceneId::new(of),
        name: format!("Keyframe {id}"),
    }
}

fn own(graph: &Graph, scene: &str) -> Option<SceneCamera> {
    graph
        .state()
        .scene(&SceneId::new(scene))
        .and_then(|s| s.camera().copied())
}

fn effective(graph: &Graph, scene: &str) -> Option<SceneCamera> {
    let state = graph.state();
    state
        .scene(&SceneId::new(scene))
        .and_then(|s| state.effective_camera(s))
}

#[test]
fn a_camera_is_set_reported_and_undone() {
    let mut graph = fixture();
    let applied = apply_ok(&mut graph, vec![set("a", Some(framing()))]);
    assert_eq!(applied.inverse.edits, vec![set("a", None)]);
    assert_eq!(own(&graph, "a"), Some(framing()));
    let info = graph
        .state()
        .scene(&SceneId::new("a"))
        .expect("a")
        .to_info();
    assert_eq!(info.camera, Some(framing()));
    assert_eq!(own(&graph, "b"), None);
}

#[test]
fn a_bad_camera_is_refused_with_its_field() {
    let mut graph = fixture();
    let mut at_target = framing();
    at_target.target = at_target.position;
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", Some(at_target))]));
    assert!(field.ends_with("camera.target"), "{field}");
    let mut wide = framing();
    wide.fov_deg = Finite32::new(175.0).expect("finite");
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", Some(wide))]));
    assert!(field.ends_with("camera.fov_deg"), "{field}");
    let mut unfocused = framing();
    unfocused.focus_px = Some(Finite32::ZERO);
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", Some(unfocused))]));
    assert!(field.ends_with("camera.focus_px"), "{field}");
    let mut soft = framing();
    soft.blur_px = Finite32::new(100.0).expect("finite");
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set("a", Some(soft))]));
    assert!(field.ends_with("camera.blur_px"), "{field}");
}

#[test]
fn a_keyframe_follows_or_overrides_its_parent_camera() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![variant("k", "a")]);
    // A keyframe of a 2D scene can't be 3D.
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![set("k", Some(framing()))]));
    assert!(field.ends_with("camera"), "{field}");
    assert!(reason.contains("parent"), "{reason}");
    apply_ok(&mut graph, vec![set("a", Some(framing()))]);
    assert_eq!(own(&graph, "k"), None);
    assert_eq!(effective(&graph, "k"), Some(framing()));
    apply_ok(&mut graph, vec![set("k", Some(moved(300.0)))]);
    assert_eq!(effective(&graph, "k"), Some(moved(300.0)));
    assert_eq!(effective(&graph, "a"), Some(framing()));
    // Removing the override follows the parent again.
    apply_ok(&mut graph, vec![set("k", None)]);
    assert_eq!(effective(&graph, "k"), Some(framing()));
}

#[test]
fn removing_a_parent_camera_drops_its_keyframes_overrides_and_undo_restores_them() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            variant("k", "a"),
            variant("k2", "a"),
            set("a", Some(framing())),
            set("k", Some(moved(100.0))),
        ],
    );
    // apply_ok also checks that the inverse restores the graph exactly.
    let applied = apply_ok(&mut graph, vec![set("a", None)]);
    assert_eq!(
        applied.inverse.edits,
        vec![set("a", Some(framing())), set("k", Some(moved(100.0)))]
    );
    assert_eq!(own(&graph, "k"), None);
    assert_eq!(effective(&graph, "k"), None);
}

#[test]
fn deleting_a_scene_with_a_camera_is_undone_with_it() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![super::scene_create("d"), set("d", Some(moved(50.0)))],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("d"),
        }],
    );
    assert!(applied.inverse.edits.contains(&set("d", Some(moved(50.0)))));
}

#[test]
fn cameras_round_trip_through_the_collection_file() {
    let mut graph = fixture();
    let before = graph.to_collection_json();
    assert!(
        !before.contains("camera"),
        "a file without cameras is unchanged"
    );
    apply_ok(
        &mut graph,
        vec![
            variant("k", "a"),
            set("a", Some(framing())),
            set("k", Some(moved(700.0))),
        ],
    );
    let json = graph.to_collection_json();
    let loaded = super::graph()
        .with_collection_json(&json)
        .expect("the file loads");
    assert_eq!(loaded.state(), graph.state());
    assert_eq!(loaded.to_collection_json(), json);
}

/// A display capture `screen`, placed in `a` as `s1`; a colour placed as `c1`.
fn with_screen() -> Graph {
    let mut kinds = super::kinds();
    kinds.extend(
        jstream_protocol::parity::kinds()
            .into_iter()
            .filter(|k| k.kind_id == "display"),
    );
    let mut graph = Graph::new().with_kinds(kinds);
    apply_ok(
        &mut graph,
        vec![super::scene_create("a"), super::builtin("cam")],
    );
    apply_ok(
        &mut graph,
        vec![
            Edit::SourceCreate {
                source_id: jstream_protocol::SourceId::new("screen"),
                name: "Screen".into(),
                kind: jstream_protocol::SourceKind::Builtin {
                    kind: "display".into(),
                },
                settings: jstream_protocol::Settings::new(),
                duplicated_from: None,
            },
            add("a", "s1", "screen"),
            add("a", "c1", "cam"),
            set("a", Some(framing())),
        ],
    );
    graph
}

fn sway() -> Vec<CameraMotionModifier> {
    vec![CameraMotionModifier {
        modifier_id: "sway".into(),
        property: CameraProperty::Roll,
        kind: MotionKind::Oscillate {
            shape: MotionShape::Sine,
            amplitude: Finite32::ONE,
            period_ms: 3000,
            phase: Finite32::ZERO,
        },
        clock: MotionClock::Engine,
        enabled: true,
    }]
}

fn set_motion(scene: &str, motion: Vec<CameraMotionModifier>) -> Edit {
    Edit::SceneSetCameraMotion {
        scene_id: SceneId::new(scene),
        motion,
    }
}

fn set_follow(scene: &str, item: Option<&str>) -> Edit {
    Edit::SceneSetCameraFollow {
        scene_id: SceneId::new(scene),
        follow: item.map(|item| CameraFollow::of(ItemId::new(item))),
    }
}

fn info(graph: &Graph, scene: &str) -> jstream_protocol::SceneInfo {
    graph
        .state()
        .scene(&SceneId::new(scene))
        .expect("scene")
        .to_info()
}

#[test]
fn camera_motion_and_a_follow_camera_are_set_reported_and_undone() {
    let mut graph = with_screen();
    let applied = apply_ok(
        &mut graph,
        vec![set_motion("a", sway()), set_follow("a", Some("s1"))],
    );
    assert_eq!(
        applied.inverse.edits,
        vec![set_follow("a", None), set_motion("a", Vec::new())]
    );
    let a = info(&graph, "a");
    assert_eq!(a.camera_motion, sway());
    assert_eq!(a.camera_follow, Some(CameraFollow::of(ItemId::new("s1"))));
}

#[test]
fn a_follow_camera_needs_a_capture_in_the_scene() {
    let mut graph = with_screen();
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![set_follow("a", Some("c1"))]));
    assert!(field.ends_with("follow.item_id"), "{field}");
    assert!(reason.contains("display or window capture"), "{reason}");
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set_follow("a", Some("nope"))]));
    assert!(field.ends_with("follow.item_id"), "{field}");
    let mut far = CameraFollow::of(ItemId::new("s1"));
    far.zoom = Finite32::new(9.0).expect("finite");
    let (field, _) = invalid_reason(apply_err(
        &mut graph,
        vec![Edit::SceneSetCameraFollow {
            scene_id: SceneId::new("a"),
            follow: Some(far),
        }],
    ));
    assert!(field.ends_with("follow.zoom"), "{field}");
}

#[test]
fn camera_motion_and_follow_belong_to_the_family() {
    let mut graph = with_screen();
    apply_ok(&mut graph, vec![variant("k", "a")]);
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set_motion("k", sway())]));
    assert!(field.ends_with("motion"), "{field}");
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![set_follow("k", Some("s1"))]));
    assert!(field.ends_with("follow"), "{field}");
}

#[test]
fn deleting_a_scene_restores_its_camera_motion_and_follow_on_undo() {
    let mut graph = with_screen();
    apply_ok(
        &mut graph,
        vec![set_motion("a", sway()), set_follow("a", Some("s1"))],
    );
    let before = info(&graph, "a");
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("a"),
        }],
    );
    let mut undone = graph.clone();
    undone
        .apply(&applied.inverse.edits, None)
        .expect("the inverse applies");
    assert_eq!(info(&undone, "a"), before);
}

#[test]
fn removing_the_followed_item_leaves_the_follow_inert() {
    let mut graph = with_screen();
    apply_ok(&mut graph, vec![set_follow("a", Some("s1"))]);
    apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("s1"),
        }],
    );
    assert_eq!(
        info(&graph, "a").camera_follow,
        Some(CameraFollow::of(ItemId::new("s1")))
    );
}
