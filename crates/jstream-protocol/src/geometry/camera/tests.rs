//! Scene cameras (ADR-0094): the framing camera reproduces 2D boxes, the projection
//! matches a direct pinhole computation, and the plane map's inverse and near clip hold
//! through a camera.

use super::{CameraView, SceneCamera, Vec3};
use crate::command::{Crop, Vec2};
use crate::finite::Finite32;
use crate::geometry::{Canvas, CanvasPoint, FitMode, GeometryError, ItemGeometry, Placed, Size};

const CANVAS: Canvas = Canvas {
    width: 1920,
    height: 1080,
};

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

fn vec3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z).expect("finite")
}

fn item(x: f32, y: f32, depth: f32, tilt: f32, turn: f32, rotation: f32) -> ItemGeometry {
    let mut geometry = ItemGeometry::try_new(
        Vec2::new(x, y).expect("finite"),
        Size::try_new(400.0, 300.0).expect("positive"),
    )
    .expect("geometry");
    geometry.fit_mode = FitMode::Stretch;
    geometry.depth_px = finite(depth);
    geometry.tilt_deg = finite(tilt);
    geometry.turn_deg = finite(turn);
    geometry.rotation_deg = finite(rotation);
    geometry
}

fn camera(position: Vec3, target: Vec3, fov: f32, roll: f32) -> SceneCamera {
    SceneCamera {
        position,
        target,
        fov_deg: finite(fov),
        roll_deg: finite(roll),
        depth_sort: true,
        focus_px: None,
        blur_px: Finite32::ZERO,
    }
}

fn view(camera: SceneCamera) -> CameraView {
    camera.view(CANVAS).expect("view")
}

fn polygon_points(geometry: ItemGeometry, view: Option<&CameraView>) -> Vec<CanvasPoint> {
    let content = geometry
        .content(400, 300, Crop::default())
        .expect("content")
        .expect("pixels");
    geometry
        .drawn_polygon_seen(view, &content)
        .expect("polygon")
        .expect("drawn")
        .vertices()
        .iter()
        .map(|vertex| vertex.canvas)
        .collect()
}

fn near(got: CanvasPoint, expected: CanvasPoint, tolerance: f32) {
    assert!(
        (got.x - expected.x).abs() < tolerance && (got.y - expected.y).abs() < tolerance,
        "{got:?} vs {expected:?}"
    );
}

/// The local point `(x, y)` of `geometry` in the scene's space, by the ADR-0022 rotations.
fn world(geometry: ItemGeometry, x: f32, y: f32) -> Vec3 {
    let (x, y, z) = super::super::rotate_z(x, y, 0.0, geometry.rotation_deg.get());
    let (x, y, z) = super::super::rotate_y(x, y, z, geometry.turn_deg.get());
    let (x, y, z) = super::super::rotate_x(x, y, z, geometry.tilt_deg.get());
    vec3(
        geometry.center.x.get() + x,
        geometry.center.y.get() + y,
        geometry.depth_px.get() + z,
    )
}

#[test]
fn the_framing_camera_draws_a_flat_item_on_its_2d_box() {
    let view = view(SceneCamera::framing(CANVAS));
    for geometry in [
        item(960.0, 540.0, 0.0, 0.0, 0.0, 0.0),
        item(300.0, 200.0, 0.0, 0.0, 0.0, 30.0),
        item(1700.0, 900.0, 0.0, 0.0, 0.0, -75.0),
    ] {
        let flat = polygon_points(geometry, None);
        let seen = polygon_points(geometry, Some(&view));
        assert_eq!(flat.len(), seen.len());
        for (got, expected) in seen.into_iter().zip(flat) {
            near(got, expected, 0.01);
        }
    }
}

#[test]
fn the_framing_camera_sits_at_the_focal_length() {
    let framing = SceneCamera::framing(CANVAS);
    let view = view(framing);
    assert!((framing.position.z.get() - view.focal_px()).abs() < 0.01);
    assert_eq!(framing.fov_deg.get(), 45.0);
    assert!(view.depth_sort());
}

#[test]
fn a_camera_scene_ignores_the_item_perspective() {
    let view = view(SceneCamera::framing(CANVAS));
    let mut geometry = item(800.0, 500.0, 40.0, 20.0, 30.0, 0.0);
    let without = polygon_points(geometry, Some(&view));
    geometry.perspective_px = Some(finite(300.0));
    assert_eq!(polygon_points(geometry, Some(&view)), without);
}

#[test]
fn vertices_match_a_pinhole_camera() {
    let cameras = [
        camera(
            vec3(200.0, 100.0, 1500.0),
            vec3(960.0, 540.0, -200.0),
            50.0,
            0.0,
        ),
        camera(
            vec3(960.0, -800.0, 400.0),
            vec3(960.0, 540.0, 0.0),
            35.0,
            12.0,
        ),
        camera(
            vec3(2400.0, 540.0, 300.0),
            vec3(900.0, 600.0, -100.0),
            70.0,
            -40.0,
        ),
    ];
    let items = [
        item(960.0, 540.0, 0.0, 0.0, 0.0, 0.0),
        item(700.0, 400.0, -150.0, 25.0, -35.0, 10.0),
        item(1200.0, 650.0, 80.0, -70.0, 15.0, 120.0),
    ];
    let mut checked = 0;
    for camera in cameras {
        let view = view(camera);
        for geometry in items {
            for (x, y) in [(-200.0, -150.0), (200.0, -150.0), (0.0, 0.0), (120.0, 90.0)] {
                let expected = match view.place(world(geometry, x, y)) {
                    Placed::At(point) => point,
                    Placed::BehindViewer => continue,
                };
                let content = geometry
                    .content(400, 300, Crop::default())
                    .expect("content")
                    .expect("pixels");
                let local = super::super::LocalPoint { x, y };
                let Some(vertex) = geometry
                    .drawn_vertex_seen(Some(&view), &content, local)
                    .expect("vertex")
                else {
                    continue;
                };
                let tolerance = 0.002 * (expected.x.abs() + expected.y.abs()).max(1000.0);
                near(vertex.canvas, expected, tolerance);
                let w = vertex.w;
                assert!(w > 0.0, "w {w}");
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 30,
        "only {checked} points were in front of a camera"
    );
}

#[test]
fn unplace_inverts_the_camera_projection() {
    let view = view(camera(
        vec3(400.0, 200.0, 1200.0),
        vec3(1000.0, 600.0, -100.0),
        55.0,
        8.0,
    ));
    let geometry = item(900.0, 500.0, -60.0, 20.0, 35.0, -15.0);
    let content = geometry
        .content(400, 300, Crop::default())
        .expect("content")
        .expect("pixels");
    for (x, y) in [(-180.0, -120.0), (150.0, -100.0), (0.0, 0.0), (90.0, 140.0)] {
        let local = super::super::LocalPoint { x, y };
        let vertex = geometry
            .drawn_vertex_seen(Some(&view), &content, local)
            .expect("vertex")
            .expect("in front");
        let back = geometry
            .unplace_seen(Some(&view), vertex.canvas)
            .expect("unplace")
            .expect("on the plane");
        assert!(
            (back.x - x).abs() < 0.05 && (back.y - y).abs() < 0.05,
            "{back:?} vs ({x}, {y})"
        );
    }
}

#[test]
fn an_item_behind_the_camera_draws_nothing() {
    let view = view(SceneCamera::framing(CANVAS));
    let behind = item(960.0, 540.0, 3000.0, 0.0, 0.0, 0.0);
    let content = behind
        .content(400, 300, Crop::default())
        .expect("content")
        .expect("pixels");
    assert_eq!(
        behind
            .drawn_polygon_seen(Some(&view), &content)
            .expect("ok"),
        None
    );
}

#[test]
fn an_item_through_the_camera_plane_is_clipped_not_dropped() {
    // A floor tilted flat, reaching from in front of the camera to behind it.
    let view = view(SceneCamera::framing(CANVAS));
    let mut floor = item(960.0, 800.0, 1000.0, 90.0, 0.0, 0.0);
    floor.size = Size::try_new(800.0, 4000.0).expect("positive");
    let content = floor
        .content(400, 300, Crop::default())
        .expect("content")
        .expect("pixels");
    let polygon = floor
        .drawn_polygon_seen(Some(&view), &content)
        .expect("ok")
        .expect("its front part draws");
    for vertex in polygon.vertices() {
        assert!(vertex.w >= super::super::NEAR_W * 0.999, "w {}", vertex.w);
    }
}

#[test]
fn an_item_seen_edge_on_through_the_camera_draws_nothing() {
    // The camera looks along the item's plane: a plane through the viewer.
    let view = view(camera(
        vec3(960.0, 540.0, 1000.0),
        vec3(960.0, 540.0, 0.0),
        45.0,
        0.0,
    ));
    let geometry = item(960.0, 540.0, 0.0, 0.0, 90.0, 0.0);
    let content = geometry
        .content(400, 300, Crop::default())
        .expect("content")
        .expect("pixels");
    assert_eq!(
        geometry
            .drawn_polygon_seen(Some(&view), &content)
            .expect("ok"),
        None
    );
}

#[test]
fn roll_turns_the_picture_clockwise() {
    let mut rolled = SceneCamera::framing(CANVAS);
    rolled.roll_deg = finite(90.0);
    let view = view(rolled);
    // A point right of centre lands below it once the picture turns a quarter clockwise.
    let Placed::At(point) = view.place(vec3(1160.0, 540.0, 0.0)) else {
        panic!("in front");
    };
    near(point, CanvasPoint { x: 960.0, y: 740.0 }, 0.01);
}

#[test]
fn looking_straight_down_keeps_right_on_the_right() {
    let view = view(camera(
        vec3(960.0, -1000.0, 0.0),
        vec3(960.0, 540.0, 0.0),
        45.0,
        0.0,
    ));
    let Placed::At(right) = view.place(vec3(1160.0, 540.0, 0.0)) else {
        panic!("in front");
    };
    assert!(right.x > 960.0, "{right:?}");
    // World −z is up in the picture.
    let Placed::At(far) = view.place(vec3(960.0, 540.0, -200.0)) else {
        panic!("in front");
    };
    assert!(far.y < 540.0, "{far:?}");
}

#[test]
fn depth_sorts_by_distance_along_the_axis() {
    let view = view(SceneCamera::framing(CANVAS));
    let back = item(960.0, 540.0, -500.0, 0.0, 0.0, 0.0);
    let front = item(200.0, 100.0, 100.0, 0.0, 0.0, 0.0);
    assert!(view.depth_of(back) > view.depth_of(front));
}

#[test]
fn project_seen_matches_the_drawn_corners() {
    let view = view(camera(
        vec3(500.0, 300.0, 1400.0),
        vec3(960.0, 540.0, 0.0),
        45.0,
        0.0,
    ));
    let geometry = item(960.0, 540.0, 0.0, 10.0, 20.0, 5.0);
    let quad = geometry.project_seen(Some(&view)).expect("quad");
    let drawn = polygon_points(geometry, Some(&view));
    for (corner, expected) in quad.corners.iter().zip(drawn) {
        let Placed::At(corner) = corner else {
            panic!("in front");
        };
        near(*corner, expected, 0.01);
    }
}

#[test]
fn bad_cameras_are_refused() {
    let at = vec3(10.0, 10.0, 10.0);
    assert_eq!(
        camera(at, at, 45.0, 0.0).validate(),
        Err(GeometryError::CameraAtTarget)
    );
    let target = vec3(0.0, 0.0, 0.0);
    assert_eq!(
        camera(at, target, 0.5, 0.0).validate(),
        Err(GeometryError::FieldOfView)
    );
    assert_eq!(
        camera(at, target, 171.0, 0.0).validate(),
        Err(GeometryError::FieldOfView)
    );
    assert_eq!(
        camera(vec3(2e15, 0.0, 0.0), target, 45.0, 0.0).validate(),
        Err(GeometryError::TooFar)
    );
    assert_eq!(camera(at, target, 170.0, 0.0).validate(), Ok(()));
}

#[test]
fn the_wire_form_has_defaults() {
    let camera: SceneCamera =
        serde_json::from_str(r#"{"position":{"x":0,"y":0,"z":100},"target":{"x":0,"y":0,"z":0}}"#)
            .expect("parses");
    assert_eq!(camera.fov_deg.get(), 45.0);
    assert_eq!(camera.roll_deg.get(), 0.0);
    assert!(camera.depth_sort);
    let written = serde_json::to_value(camera).expect("writes");
    assert!(written.get("depth_sort").is_none());
    let mut flat = camera;
    flat.depth_sort = false;
    assert_eq!(
        serde_json::to_value(flat).expect("writes")["depth_sort"],
        false
    );
}

#[test]
fn on_plane_inverts_place() {
    let seen = view(camera(
        vec3(300.0, 200.0, 1400.0),
        vec3(1000.0, 600.0, -200.0),
        50.0,
        8.0,
    ));
    for (x, y, z) in [
        (960.0, 540.0, 0.0),
        (200.0, 900.0, -350.0),
        (1500.0, 120.0, 220.0),
    ] {
        let Placed::At(at) = seen.place(vec3(x, y, z)) else {
            panic!("({x}, {y}, {z}) is in front of the camera");
        };
        let (px, py) = seen.on_plane(at, z).expect("meets the plane");
        assert!(
            (px - x).abs() < 0.05 && (py - y).abs() < 0.05,
            "({px}, {py}) for ({x}, {y})"
        );
    }
    // Looking along a plane, or at one behind the camera, meets nothing.
    let level = view(camera(
        vec3(960.0, 540.0, 0.0),
        vec3(1960.0, 540.0, 0.0),
        45.0,
        0.0,
    ));
    let ahead = CanvasPoint { x: 960.0, y: 540.0 };
    assert_eq!(level.on_plane(ahead, 0.0), None);
    let framing = view(SceneCamera::framing(CANVAS));
    assert_eq!(framing.on_plane(ahead, 5000.0), None);
}

/// ADR-0108 decision 1: focus and blur are optional on the wire, checked, and not
/// written when absent.
#[test]
fn depth_of_field_is_optional_and_checked() {
    let at = vec3(960.0, 540.0, 1000.0);
    let target = vec3(960.0, 540.0, 0.0);
    let mut c = camera(at, target, 45.0, 0.0);
    assert_eq!(c.focus_px, None);
    assert_eq!(c.blur_px.get(), 0.0);
    let written = serde_json::to_value(c).expect("writes");
    assert!(written.get("focus_px").is_none() && written.get("blur_px").is_none());
    c.focus_px = Some(finite(0.0));
    assert_eq!(c.validate(), Err(GeometryError::FocusDistance));
    c.focus_px = Some(finite(2e15));
    assert_eq!(c.validate(), Err(GeometryError::FocusDistance));
    c.focus_px = Some(finite(1000.0));
    c.blur_px = finite(65.0);
    assert_eq!(c.validate(), Err(GeometryError::Blur));
    c.blur_px = finite(-1.0);
    assert_eq!(c.validate(), Err(GeometryError::Blur));
    c.blur_px = finite(8.0);
    assert_eq!(c.validate(), Ok(()));
    let back: SceneCamera =
        serde_json::from_value(serde_json::to_value(c).expect("writes")).expect("reads");
    assert_eq!(back, c);
    assert!((c.target_distance() - 1000.0).abs() < 1e-3);
}

/// ADR-0108 decision 2: the blur is 0 on the focus plane, `blur_px` at half its distance
/// and far behind it, and capped close to the camera; with no focus or no blur it is 0.
#[test]
fn the_blur_follows_the_thin_lens() {
    let at = vec3(960.0, 540.0, 1000.0);
    let target = vec3(960.0, 540.0, 0.0);
    let mut c = camera(at, target, 45.0, 0.0);
    c.blur_px = finite(8.0);
    assert_eq!(view(c).focus(), None, "no focus distance");
    assert_eq!(view(c).blur_at(500.0), 0.0);
    c.focus_px = Some(finite(1000.0));
    let seen = view(c);
    assert_eq!(seen.blur_at(1000.0), 0.0);
    assert!((seen.blur_at(500.0) - 8.0).abs() < 1e-4);
    assert!((seen.blur_at(2000.0) - 4.0).abs() < 1e-4);
    assert!((seen.blur_at(1e9) - 8.0).abs() < 1e-3);
    assert_eq!(seen.blur_at(1.0), super::MAX_BLUR_PX);
    assert_eq!(seen.blur_at(-5.0), super::MAX_BLUR_PX);
    c.blur_px = finite(0.0);
    assert_eq!(view(c).focus(), None, "no blur");
}
