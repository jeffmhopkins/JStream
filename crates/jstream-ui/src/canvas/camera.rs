//! The canvas in a 3D scene (ADR-0094): its boxes drawn and hit through the scene's
//! camera, a drag that moves an item in its own depth plane, and the orbit and dolly
//! drags that move the camera.
//!
//! The engine draws the picture under the boxes. This module only has to agree with
//! it, and it does so through the same `jstream-protocol` functions the compositor
//! uses (`project_seen`, `CameraView::depth_of` and `CameraView::on_plane`).
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0094-scene-cameras.md

use crate::canvas::transform::Point;
use jstream_control::protocol::{
    CameraView, Canvas, CanvasPoint, Finite32, ItemGeometry, Placed, SceneCamera, Size, Vec2, Vec3,
};

/// Degrees the camera turns about its target per screen pixel of an orbit drag.
pub const ORBIT_DEG_PER_PX: f32 = 0.3;
/// How far an orbit tips the camera up or down, at most, in degrees: short of looking
/// straight down, where its picture's up would flip.
pub const MAX_ELEVATION_DEG: f32 = 85.0;
/// How much a dolly drag of one screen pixel scales the camera's distance to its
/// target: `exp(px × DOLLY_PER_PX)`, down to closer.
pub const DOLLY_PER_PX: f32 = 0.005;
/// The closest a dolly brings the camera to its target, in scene pixels.
pub const MIN_DOLLY_PX: f32 = 10.0;

/// The camera seen on `canvas`, or `None` when the scene has none or the engine would
/// refuse it.
pub fn view(camera: Option<SceneCamera>, canvas: Canvas) -> Option<CameraView> {
    camera?.view(canvas).ok()
}

/// The box's four corners on the canvas through `view`, or `None` when any is behind the
/// camera or not finite.
pub fn corners(geometry: &ItemGeometry, view: &CameraView) -> Option<[Point; 4]> {
    let quad = geometry.project_seen(Some(view)).ok()?;
    let mut out = [Point { x: 0.0, y: 0.0 }; 4];
    for (slot, corner) in out.iter_mut().zip(quad.corners) {
        let Placed::At(at) = corner else {
            return None;
        };
        *slot = Point { x: at.x, y: at.y };
    }
    Some(out)
}

/// Whether `point` is inside the box as the camera shows it: inside its four projected
/// corners, either winding (a box seen from behind is mirrored).
pub fn contains(geometry: &ItemGeometry, view: &CameraView, point: Point) -> bool {
    let Some(quad) = corners(geometry, view) else {
        return false;
    };
    let side = |a: Point, b: Point| (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    let sides = [0, 1, 2, 3].map(|i| side(quad[i], quad[(i + 1) % 4]));
    sides.iter().all(|s| *s >= 0.0) || sides.iter().all(|s| *s <= 0.0)
}

/// The box under `point` that the engine draws on top: the nearest along the viewing
/// axis when the camera sorts by depth, ties and an unsorted camera going to the later
/// in stacking order, as the compositor draws them.
pub fn topmost_at<'a, T>(
    items: impl DoubleEndedIterator<Item = (T, &'a ItemGeometry)>,
    view: &CameraView,
    point: Point,
) -> Option<T> {
    let mut best: Option<(f64, T)> = None;
    for (id, geometry) in items {
        if !contains(geometry, view, point) {
            continue;
        }
        let depth = if view.depth_sort() {
            view.depth_of(*geometry)
        } else {
            0.0
        };
        // Later in stacking order wins a tie, so `<=`.
        if best.as_ref().is_none_or(|(near, _)| depth <= *near) {
            best = Some((depth, id));
        }
    }
    best.map(|(_, id)| id)
}

/// The box's outline through the camera as SVG `points`, or empty when a corner is
/// behind it.
pub fn outline(geometry: &ItemGeometry, view: &CameraView) -> String {
    corners(geometry, view).map_or_else(String::new, |quad| {
        quad.iter().fold(String::new(), |mut points, at| {
            points.push_str(&format!("{},{} ", at.x, at.y));
            points
        })
    })
}

/// The upright box around the box as the camera shows it, for a marquee to touch, or
/// `None` when it isn't wholly in front of the camera.
pub fn screen_box(geometry: &ItemGeometry, view: &CameraView) -> Option<ItemGeometry> {
    let quad = corners(geometry, view)?;
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for at in quad {
        left = left.min(at.x);
        top = top.min(at.y);
        right = right.max(at.x);
        bottom = bottom.max(at.y);
    }
    let center = Vec2::new((left + right) * 0.5, (top + bottom) * 0.5)?;
    let size = Size::try_new((right - left).max(1.0), (bottom - top).max(1.0)).ok()?;
    ItemGeometry::try_new(center, size).ok()
}

/// Where a drag at canvas point `at` puts an item whose depth is `geometry.depth_px`:
/// the point of its depth plane under the pointer, in the plane's own x and y, which a
/// move gesture then follows as it follows the pointer in a 2D scene. `None` when the
/// pointer's ray misses the plane.
pub fn on_plane(geometry: &ItemGeometry, view: &CameraView, at: Point) -> Option<Point> {
    let (x, y) = view.on_plane(CanvasPoint { x: at.x, y: at.y }, geometry.depth_px.get())?;
    Some(Point { x, y })
}

fn get(v: Vec3) -> [f32; 3] {
    [v.x.get(), v.y.get(), v.z.get()]
}

fn vec3([x, y, z]: [f32; 3]) -> Option<Vec3> {
    Vec3::new(x, y, z)
}

/// `start` turned about its target by a drag of `dx`, `dy` screen pixels: right turns
/// the camera to the right around the target, down raises it to look down on the set,
/// up to [`MAX_ELEVATION_DEG`]. Its distance, field of view and roll stay.
pub fn orbit(start: SceneCamera, dx: f32, dy: f32) -> SceneCamera {
    let target = get(start.target);
    let position = get(start.position);
    let offset = [0, 1, 2].map(|i| position[i] - target[i]);
    let distance = offset.iter().map(|v| v * v).sum::<f32>().sqrt();
    if distance <= 0.0 {
        return start;
    }
    // Up is −y; the azimuth is measured about it from +z, toward +x.
    let level = offset[0].hypot(offset[2]);
    let elevation = (-offset[1]).atan2(level);
    let azimuth = offset[0].atan2(offset[2]);
    let limit = MAX_ELEVATION_DEG.to_radians();
    let elevation = (elevation + (dy * ORBIT_DEG_PER_PX).to_radians()).clamp(-limit, limit);
    let azimuth = azimuth - (dx * ORBIT_DEG_PER_PX).to_radians();
    let moved = [
        target[0] + distance * elevation.cos() * azimuth.sin(),
        target[1] - distance * elevation.sin(),
        target[2] + distance * elevation.cos() * azimuth.cos(),
    ];
    match vec3(moved) {
        Some(position) => SceneCamera { position, ..start },
        None => start,
    }
}

/// `start` moved along its line of sight by a drag of `dy` screen pixels: down pulls it
/// back, up brings it closer, never nearer than [`MIN_DOLLY_PX`] to its target.
pub fn dolly(start: SceneCamera, dy: f32) -> SceneCamera {
    let target = get(start.target);
    let position = get(start.position);
    let offset = [0, 1, 2].map(|i| position[i] - target[i]);
    let distance = offset.iter().map(|v| v * v).sum::<f32>().sqrt();
    if distance <= 0.0 {
        return start;
    }
    let scaled = (distance * (dy * DOLLY_PER_PX).exp()).max(MIN_DOLLY_PX);
    let k = scaled / distance;
    match vec3([0, 1, 2].map(|i| target[i] + offset[i] * k)) {
        Some(position) => SceneCamera { position, ..start },
        None => start,
    }
}

/// The camera's numbers rounded for a readout: its position, and its distance to the
/// target.
pub fn readout(camera: SceneCamera) -> String {
    let [x, y, z] = get(camera.position);
    let target = get(camera.target);
    let distance = [x - target[0], y - target[1], z - target[2]]
        .iter()
        .map(|v| v * v)
        .sum::<f32>()
        .sqrt();
    format!(
        "Camera {:.0}, {:.0}, {:.0} · {:.0} px from target",
        x, y, z, distance
    )
}

/// `value` as a finite coordinate, or `fallback` when it isn't one.
pub fn finite(value: f32, fallback: Finite32) -> Finite32 {
    Finite32::new(value).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::FitMode;

    const CANVAS: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };

    fn item(x: f32, y: f32, w: f32, h: f32, depth: f32) -> ItemGeometry {
        let mut g = ItemGeometry::try_new(
            Vec2::new(x, y).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("geometry");
        g.fit_mode = FitMode::Stretch;
        g.depth_px = Finite32::new(depth).expect("finite");
        g
    }

    fn framing() -> CameraView {
        view(Some(SceneCamera::framing(CANVAS)), CANVAS).expect("a valid camera")
    }

    fn distance(camera: SceneCamera) -> f32 {
        let p = get(camera.position);
        let t = get(camera.target);
        [p[0] - t[0], p[1] - t[1], p[2] - t[2]]
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            .sqrt()
    }

    #[test]
    fn the_framing_camera_draws_and_hits_a_flat_box_where_2d_does() {
        let seen = framing();
        let g = item(500.0, 300.0, 400.0, 200.0, 0.0);
        let quad = corners(&g, &seen).expect("in front");
        let flat = crate::canvas::transform::corners(&g);
        for (a, b) in quad.iter().zip(flat) {
            assert!(
                (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() < 0.05,
                "{a:?} vs {b:?}"
            );
        }
        assert!(contains(&g, &seen, Point { x: 650.0, y: 350.0 }));
        assert!(!contains(&g, &seen, Point { x: 750.0, y: 350.0 }));
    }

    #[test]
    fn the_nearer_box_is_hit_whatever_the_stacking_order() {
        let seen = framing();
        let near = item(960.0, 540.0, 300.0, 300.0, 200.0);
        let far = item(960.0, 540.0, 300.0, 300.0, -400.0);
        // The near box is first in stacking order, under the far one.
        let items = [("near", &near), ("far", &far)];
        let centre = Point { x: 960.0, y: 540.0 };
        assert_eq!(topmost_at(items.into_iter(), &seen, centre), Some("near"));
        // With depth sorting off, stacking order decides, as the compositor draws.
        let mut camera = SceneCamera::framing(CANVAS);
        camera.depth_sort = false;
        let unsorted = view(Some(camera), CANVAS).expect("valid");
        assert_eq!(
            topmost_at(items.into_iter(), &unsorted, centre),
            Some("far")
        );
    }

    #[test]
    fn a_drag_on_the_depth_plane_follows_the_pointer() {
        // From off to the side, a box pushed back moves under the pointer: the plane
        // point under where the drag ends is where its centre goes.
        let mut camera = SceneCamera::framing(CANVAS);
        camera.position = vec3([400.0, 300.0, 1100.0]).expect("finite");
        let seen = view(Some(camera), CANVAS).expect("valid");
        let g = item(900.0, 500.0, 200.0, 200.0, -300.0);
        let Placed::At(centre) = seen.place(vec3([900.0, 500.0, -300.0]).expect("finite")) else {
            panic!("in front");
        };
        let grabbed = on_plane(
            &g,
            &seen,
            Point {
                x: centre.x,
                y: centre.y,
            },
        )
        .expect("meets");
        assert!((grabbed.x - 900.0).abs() < 0.05 && (grabbed.y - 500.0).abs() < 0.05);
    }

    #[test]
    fn an_orbit_keeps_the_distance_and_stops_short_of_overhead() {
        let start = SceneCamera::framing(CANVAS);
        let turned = orbit(start, 300.0, 0.0);
        assert!((distance(turned) - distance(start)).abs() < 0.5);
        // A drag to the right swings the camera to the left of the target.
        assert!(get(turned.position)[0] < get(start.target)[0]);
        let raised = orbit(start, 0.0, 10_000.0);
        let p = get(raised.position);
        let t = get(raised.target);
        let elevation = (t[1] - p[1])
            .atan2((p[0] - t[0]).hypot(p[2] - t[2]))
            .to_degrees();
        assert!((elevation - MAX_ELEVATION_DEG).abs() < 0.01, "{elevation}");
        assert!(raised.view(CANVAS).is_ok());
    }

    #[test]
    fn a_dolly_scales_the_distance_and_keeps_its_distance() {
        let start = SceneCamera::framing(CANVAS);
        let back = dolly(start, 100.0);
        assert!((distance(back) / distance(start) - (0.5f32).exp()).abs() < 0.001);
        let close = dolly(start, -100_000.0);
        assert!((distance(close) - MIN_DOLLY_PX).abs() < 0.01);
    }

    #[test]
    fn a_marquee_box_bounds_the_projected_corners() {
        let seen = framing();
        let g = item(500.0, 300.0, 400.0, 200.0, 0.0);
        let bounds = screen_box(&g, &seen).expect("in front");
        assert!((bounds.center.x.get() - 500.0).abs() < 0.05);
        assert!((bounds.size.width.get() - 400.0).abs() < 0.1);
    }
}
