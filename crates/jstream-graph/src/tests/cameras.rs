//! Scene cameras (ADR-0094): `scene.set_camera`, its checks and inverse, a keyframe
//! scene's override, removing a parent's camera, deleting a scene, and the collection
//! file.

use super::{apply_err, apply_ok, fixture, invalid_reason};
use crate::Graph;
use jstream_protocol::{Canvas, Edit, Finite32, SceneCamera, SceneId, Vec3};

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
