//! The drawn polygon and the inverse map against ADR-0022's point formula, and the cases
//! spike S5 checked (`spikes/s5-3d`): a turned item's perspective-correct texture
//! coordinates, a box reaching behind the viewer, and an item seen edge-on.

use super::{DrawnPolygon, DrawnVertex, LocalPoint, NEAR_W};
use crate::command::{Crop, Vec2};
use crate::finite::Finite32;
use crate::geometry::{
    CanvasPoint, FitMode, ItemGeometry, Placed, Size, rotate_x, rotate_y, rotate_z,
};

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

/// A 400×300 box at (480, 270) with the given 3D.
fn geometry(tilt: f32, turn: f32, rotation: f32, depth: f32, p: Option<f32>) -> ItemGeometry {
    let mut geometry = ItemGeometry::try_new(
        Vec2::new(480.0, 270.0).expect("finite"),
        Size::try_new(400.0, 300.0).expect("positive"),
    )
    .expect("geometry");
    geometry.fit_mode = FitMode::Stretch;
    geometry.tilt_deg = finite(tilt);
    geometry.turn_deg = finite(turn);
    geometry.rotation_deg = finite(rotation);
    geometry.depth_px = finite(depth);
    geometry.perspective_px = p.map(finite);
    geometry
}

fn polygon(geometry: ItemGeometry) -> Option<DrawnPolygon> {
    let content = geometry
        .content(400, 300, Crop::default())
        .expect("content")
        .expect("pixels");
    geometry.drawn_polygon(&content).expect("polygon")
}

fn placed(geometry: ItemGeometry, x: f32, y: f32) -> CanvasPoint {
    match geometry.place(x, y).expect("place") {
        Placed::At(point) => point,
        Placed::BehindViewer => panic!("({x}, {y}) is behind the viewer"),
    }
}

fn near(got: CanvasPoint, expected: CanvasPoint, tolerance: f32) {
    assert!(
        (got.x - expected.x).abs() < tolerance && (got.y - expected.y).abs() < tolerance,
        "{got:?} vs {expected:?}"
    );
}

const CASES: [(f32, f32, f32, f32, Option<f32>); 5] = [
    (0.0, 0.0, 0.0, 0.0, None),
    (0.0, 50.0, 0.0, 0.0, Some(700.0)),
    (30.0, -20.0, 15.0, 40.0, Some(800.0)),
    (-60.0, 45.0, 90.0, -100.0, Some(500.0)),
    (70.0, 10.0, -33.0, 0.0, None),
];

#[test]
fn neutral_3d_is_the_2d_quad() {
    let geometry = geometry(0.0, 0.0, 20.0, 0.0, None);
    let drawn = polygon(geometry).expect("draws");
    let corners = [
        (-200.0, -150.0),
        (200.0, -150.0),
        (200.0, 150.0),
        (-200.0, 150.0),
    ];
    let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    assert_eq!(drawn.vertices().len(), 4);
    for ((vertex, (x, y)), uv) in drawn.vertices().iter().zip(corners).zip(uvs) {
        assert_eq!(vertex.w, 1.0);
        assert_eq!(vertex.local, LocalPoint { x, y });
        assert_eq!(vertex.uv, uv);
        near(vertex.canvas, placed(geometry, x, y), 1e-3);
    }
}

#[test]
fn every_vertex_is_where_the_shared_formula_puts_it() {
    for (tilt, turn, rotation, depth, p) in CASES {
        let geometry = geometry(tilt, turn, rotation, depth, p);
        let drawn = polygon(geometry).expect("draws");
        for vertex in drawn.vertices() {
            near(
                vertex.canvas,
                placed(geometry, vertex.local.x, vertex.local.y),
                1e-2,
            );
            // z by the formula's own rotations, then w = (p − z) / p.
            let (x, y, z) = rotate_z(vertex.local.x, vertex.local.y, 0.0, rotation);
            let (x, y, z) = rotate_y(x, y, z, turn);
            let (_, _, z) = rotate_x(x, y, z, tilt);
            let expected_w = p.map_or(1.0, |p| (p - (z + depth)) / p);
            assert!(
                (vertex.w - expected_w).abs() < 1e-3,
                "{tilt} {turn} {rotation} {depth} {p:?}: w {} vs {expected_w}",
                vertex.w
            );
        }
    }
}

/// The canvas point's texture coordinate interpolated over the drawn polygon as a GPU
/// does with `w` per vertex: barycentric weights over a fan triangle, each vertex's `uv`
/// divided by its `w`, then divided by the interpolated `1 / w`.
fn perspective_uv(drawn: &DrawnPolygon, at: CanvasPoint) -> [f32; 2] {
    let vertices = drawn.vertices();
    for i in 1..vertices.len() - 1 {
        let triangle = [vertices[0], vertices[i], vertices[i + 1]];
        if let Some(weights) = barycentric(&triangle, at) {
            let mut uv = [0.0, 0.0];
            let mut one = 0.0;
            for (vertex, weight) in triangle.iter().zip(weights) {
                uv[0] += weight * vertex.uv[0] / vertex.w;
                uv[1] += weight * vertex.uv[1] / vertex.w;
                one += weight / vertex.w;
            }
            return [uv[0] / one, uv[1] / one];
        }
    }
    panic!("{at:?} is outside the polygon");
}

fn barycentric(triangle: &[DrawnVertex; 3], at: CanvasPoint) -> Option<[f32; 3]> {
    let [a, b, c] = triangle.map(|vertex| vertex.canvas);
    let det = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
    let l0 = ((b.y - c.y) * (at.x - c.x) + (c.x - b.x) * (at.y - c.y)) / det;
    let l1 = ((c.y - a.y) * (at.x - c.x) + (a.x - c.x) * (at.y - c.y)) / det;
    let l2 = 1.0 - l0 - l1;
    let inside = [l0, l1, l2].iter().all(|l| *l >= -1e-4);
    inside.then_some([l0, l1, l2])
}

/// Spike S5's scenario A: an item turned 50° with a perspective of 700. Interpolating
/// the texture coordinate with each vertex's `w` lands on the source point the formula
/// places there; with `w = 1` (affine) it is far off.
#[test]
fn a_turned_item_samples_its_texture_perspective_correctly() {
    let geometry = geometry(0.0, 50.0, 0.0, 0.0, Some(700.0));
    let drawn = polygon(geometry).expect("draws");
    for (x, y) in [(37.0, 91.0), (-150.0, -100.0), (120.0, 40.0)] {
        let at = placed(geometry, x, y);
        let uv = perspective_uv(&drawn, at);
        let expected = [(x + 200.0) / 400.0, (y + 150.0) / 300.0];
        assert!(
            (uv[0] - expected[0]).abs() < 1e-3 && (uv[1] - expected[1]).abs() < 1e-3,
            "({x}, {y}): {uv:?} vs {expected:?}"
        );
    }
    let mut affine = drawn;
    for vertex in &mut affine.vertices {
        vertex.w = 1.0;
    }
    let uv = perspective_uv(&affine, placed(geometry, 120.0, 40.0));
    assert!(
        (uv[0] - 0.8).abs() > 0.01,
        "w = 1 would sample {uv:?}, close to the right point by chance"
    );
}

/// Fill's content extends past the box, and the polygon is clipped to the box with its
/// texture coordinates clipped to match.
#[test]
fn fill_is_clipped_to_the_box() {
    let mut geometry = geometry(0.0, 0.0, 0.0, 0.0, None);
    geometry.fit_mode = FitMode::Fill;
    // A square source fills the 400×300 box as 400×400: 50 px off the top and bottom.
    let content = geometry
        .content(100, 100, Crop::default())
        .expect("content")
        .expect("pixels");
    let drawn = geometry
        .drawn_polygon(&content)
        .expect("ok")
        .expect("draws");
    let locals: Vec<_> = drawn.vertices().iter().map(|v| v.local).collect();
    assert_eq!(
        locals,
        [
            LocalPoint {
                x: -200.0,
                y: -150.0
            },
            LocalPoint {
                x: 200.0,
                y: -150.0
            },
            LocalPoint { x: 200.0, y: 150.0 },
            LocalPoint {
                x: -200.0,
                y: 150.0
            },
        ]
    );
    assert_eq!(drawn.vertices()[0].uv, [0.0, 0.125]);
    assert_eq!(drawn.vertices()[2].uv, [1.0, 0.875]);
}

/// Spike S5's scenario B: tilted toward the viewer and pushed forward, the top edge is
/// behind the viewer. ADR-0022's four corners can't draw it; the clipped polygon draws
/// the near part, and the cut runs along the near plane.
#[test]
fn a_box_reaching_behind_the_viewer_is_clipped_not_dropped() {
    let geometry = geometry(-70.0, 0.0, 0.0, 250.0, Some(300.0));
    assert!(
        geometry
            .project()
            .expect("project")
            .corners
            .iter()
            .any(|corner| matches!(corner, Placed::BehindViewer))
    );
    let drawn = polygon(geometry).expect("the near part still draws");
    assert_eq!(drawn.vertices().len(), 4, "the cut replaces the top edge");
    assert!(drawn.vertices().iter().all(|v| v.w >= NEAR_W));
    let cut: Vec<_> = drawn
        .vertices()
        .iter()
        .filter(|v| (v.w - NEAR_W).abs() < 1e-4)
        .collect();
    assert_eq!(cut.len(), 2, "two vertices on the near plane: {drawn:?}");
}

/// A corner cut off: one corner behind the near plane leaves five vertices.
#[test]
fn a_corner_behind_the_viewer_leaves_five_vertices() {
    let geometry = geometry(-45.0, 45.0, 0.0, 200.0, Some(300.0));
    let behind = geometry
        .project()
        .expect("project")
        .corners
        .iter()
        .filter(|corner| matches!(corner, Placed::BehindViewer))
        .count();
    assert_eq!(behind, 1, "one corner behind");
    let drawn = polygon(geometry).expect("draws");
    assert_eq!(drawn.vertices().len(), 5);
    assert!(drawn.vertices().iter().all(|v| v.w >= NEAR_W));
}

#[test]
fn an_item_behind_the_viewer_or_edge_on_draws_nothing() {
    assert!(polygon(geometry(0.0, 0.0, 0.0, 1000.0, Some(300.0))).is_none());
    assert!(polygon(geometry(0.0, 90.0, 0.0, 0.0, Some(800.0))).is_none());
    assert!(polygon(geometry(90.0, 0.0, 0.0, 0.0, None)).is_none());
}

#[test]
fn unplace_inverts_place() {
    for (tilt, turn, rotation, depth, p) in CASES {
        let geometry = geometry(tilt, turn, rotation, depth, p);
        for (x, y) in [(-180.0, -120.0), (150.0, 100.0), (3.0, -7.0), (0.0, 0.0)] {
            let back = geometry
                .unplace(placed(geometry, x, y))
                .expect("ok")
                .expect("in front");
            assert!(
                (back.x - x).abs() < 1e-2 && (back.y - y).abs() < 1e-2,
                "{tilt} {turn} {rotation} {depth} {p:?}: ({x}, {y}) came back as {back:?}"
            );
        }
    }
}

#[test]
fn unplace_finds_nothing_edge_on_or_behind_the_near_plane() {
    let edge_on = geometry(0.0, 90.0, 0.0, 0.0, None);
    assert_eq!(
        edge_on.unplace(CanvasPoint { x: 480.0, y: 270.0 }),
        Ok(None)
    );
    // Tilted toward the viewer: a canvas point far above the box maps to the part of the
    // plane past the near plane.
    let toward = geometry(-70.0, 0.0, 0.0, 250.0, Some(300.0));
    assert_eq!(
        toward.unplace(CanvasPoint {
            x: 480.0,
            y: -5000.0
        }),
        Ok(None)
    );
    assert!(
        toward
            .unplace(CanvasPoint {
                x: f32::NAN,
                y: 0.0
            })
            .is_err()
    );
}

/// Edge-on is the plane's facing, not an area: in f32 a quarter turn leaves a sliver whose
/// area grows with the item, so a canvas-sized item turned or tilted exactly 90° would
/// otherwise draw a polygon thousands of pixels tall (#611 review, B1).
#[test]
fn a_large_item_edge_on_draws_nothing_and_unplaces_nothing() {
    let sized = |width: f32, height: f32, tilt: f32, turn: f32, p: Option<f32>| {
        let mut geometry = ItemGeometry::try_new(
            Vec2::new(960.0, 540.0).expect("finite"),
            Size::try_new(width, height).expect("positive"),
        )
        .expect("geometry");
        geometry.fit_mode = FitMode::Stretch;
        geometry.tilt_deg = finite(tilt);
        geometry.turn_deg = finite(turn);
        geometry.perspective_px = p.map(finite);
        geometry
    };
    for geometry in [
        sized(1920.0, 1080.0, 0.0, 90.0, Some(1000.0)),
        sized(1920.0, 1080.0, 90.0, 0.0, Some(1000.0)),
        sized(1280.0, 720.0, 0.0, 90.0, None),
        sized(8000.0, 8000.0, 0.0, 90.0, Some(800.0)),
        sized(8000.0, 8000.0, -90.0, 0.0, None),
    ] {
        let content = geometry
            .content(1920, 1080, Crop::default())
            .expect("content")
            .expect("pixels");
        assert_eq!(geometry.drawn_polygon(&content), Ok(None), "{geometry:?}");
        for (x, y) in [(960.0, 540.0), (960.001, -12960.0), (0.0, 0.0)] {
            assert_eq!(geometry.unplace(CanvasPoint { x, y }), Ok(None));
        }
    }
    // A degree off, it draws.
    let content = sized(1920.0, 1080.0, 0.0, 89.0, Some(1000.0))
        .content(1920, 1080, Crop::default())
        .expect("content")
        .expect("pixels");
    assert!(
        sized(1920.0, 1080.0, 0.0, 89.0, Some(1000.0))
            .drawn_polygon(&content)
            .expect("ok")
            .is_some()
    );
}
