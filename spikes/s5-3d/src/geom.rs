//! The placement the spike proposes for 3D: ADR-0022's formula kept in homogeneous form,
//! so the GPU can interpolate texture coordinates perspective-correctly, plus a near-plane
//! clip in the box's local frame and the inverse map from a canvas point to the box.
//!
//! The point formula is ADR-0022 decision 3. Only the output changes: besides the canvas
//! point, each vertex carries `w = (p − z) / p`, the reciprocal of its magnification
//! (1 without perspective).

use jstream_protocol::{ContentRect, ItemGeometry};

/// The smallest `w` a drawn vertex may have: a point is magnified at most 256 times.
/// Everything closer to the viewer is clipped away in the local frame.
pub const W_MIN: f32 = 1.0 / 256.0;

/// One vertex of a drawn polygon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    /// Canvas pixels, y down.
    pub canvas: [f32; 2],
    /// `(p − z) / p`; 1 without perspective. Always ≥ [`W_MIN`].
    pub w: f32,
    /// Source texture coordinate.
    pub uv: [f32; 2],
    /// The box's local frame (origin at the centre, y down, before rotation).
    pub local: [f32; 2],
}

/// The affine part of the placement: local (x, y, 0) ↦ (x3, y3, z3) is
/// `ax * x + ay * y + (0, 0, depth)`, and the projection divides by `w`.
#[derive(Clone, Copy, Debug)]
pub struct Plane {
    ax: [f32; 3],
    ay: [f32; 3],
    depth: f32,
    perspective: Option<f32>,
    center: [f32; 2],
}

impl Plane {
    pub fn of(g: &ItemGeometry) -> Self {
        let r = |x: f32, y: f32| {
            let (x, y, z) = rotate_z(x, y, 0.0, g.rotation_deg.get());
            let (x, y, z) = rotate_y(x, y, z, g.turn_deg.get());
            let (x, y, z) = rotate_x(x, y, z, g.tilt_deg.get());
            [x, y, z]
        };
        Plane {
            ax: r(1.0, 0.0),
            ay: r(0.0, 1.0),
            depth: g.depth_px.get(),
            perspective: g.perspective_px.map(|p| p.get()),
            center: [g.center.x.get(), g.center.y.get()],
        }
    }

    /// The viewing-axis coordinate of a local point.
    pub fn z(&self, x: f32, y: f32) -> f32 {
        self.ax[2] * x + self.ay[2] * y + self.depth
    }

    /// `w` of a local point; ≤ 0 is at or behind the viewer.
    pub fn w(&self, x: f32, y: f32) -> f32 {
        match self.perspective {
            Some(p) => (p - self.z(x, y)) / p,
            None => 1.0,
        }
    }

    /// Canvas point and `w` of a local point, or `None` at or behind the viewer.
    pub fn place(&self, x: f32, y: f32) -> Option<([f32; 2], f32)> {
        let w = self.w(x, y);
        if w <= 0.0 {
            return None;
        }
        let x3 = self.ax[0] * x + self.ay[0] * y;
        let y3 = self.ax[1] * x + self.ay[1] * y;
        Some(([self.center[0] + x3 / w, self.center[1] + y3 / w], w))
    }

    /// The local point a canvas point shows, or `None` when the plane is seen edge-on
    /// or the point is at or behind the viewer.
    ///
    /// With `X, Y` the canvas point relative to the centre, `X · (p − z3) = p · x3`
    /// (and the same for Y) is linear in the local `x, y`, so it is a 2×2 solve.
    pub fn unplace(&self, canvas: [f32; 2]) -> Option<[f32; 2]> {
        let cx = f64::from(canvas[0] - self.center[0]);
        let cy = f64::from(canvas[1] - self.center[1]);
        let (ax, ay) = (self.ax.map(f64::from), self.ay.map(f64::from));
        let d = f64::from(self.depth);
        let (m, rhs) = match self.perspective.map(f64::from) {
            Some(p) => (
                [
                    [p * ax[0] + cx * ax[2], p * ay[0] + cx * ay[2]],
                    [p * ax[1] + cy * ax[2], p * ay[1] + cy * ay[2]],
                ],
                [cx * (p - d), cy * (p - d)],
            ),
            None => ([[ax[0], ay[0]], [ax[1], ay[1]]], [cx, cy]),
        };
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        if det.abs() < 1e-9 {
            return None;
        }
        let x = (rhs[0] * m[1][1] - m[0][1] * rhs[1]) / det;
        let y = (m[0][0] * rhs[1] - rhs[0] * m[1][0]) / det;
        let (x, y) = (x as f32, y as f32);
        (self.w(x, y) > 0.0).then_some([x, y])
    }
}

/// The convex polygon an item draws: its content rectangle clipped to the box (as the 2D
/// compositor already does), then to `w ≥ W_MIN`. Both clips happen in the local frame,
/// where texture coordinates are linear, so they are exact.
///
/// `None` when nothing is left. Up to five vertices (a rectangle cut by one line).
pub fn polygon(g: &ItemGeometry, content: &ContentRect) -> Option<Vec<Vertex>> {
    let plane = Plane::of(g);
    let half_w = g.size.width.get() * 0.5;
    let half_h = g.size.height.get() * 0.5;
    let left = content.left.max(-half_w);
    let top = content.top.max(-half_h);
    let right = (content.left + content.width).min(half_w);
    let bottom = (content.top + content.height).min(half_h);
    if left >= right || top >= bottom {
        return None;
    }
    let mut local = vec![[left, top], [right, top], [right, bottom], [left, bottom]];
    if let Some(p) = plane.perspective {
        // w ≥ W_MIN  ⇔  z ≤ p · (1 − W_MIN). One half-plane in local x, y.
        let z_max = p * (1.0 - W_MIN);
        local = clip_half_plane(&local, |pt| z_max - plane.z(pt[0], pt[1]));
        if local.len() < 3 {
            return None;
        }
    }
    let uv = |x: f32, y: f32| {
        [
            content.u0 + (x - content.left) / content.width * (content.u1 - content.u0),
            content.v0 + (y - content.top) / content.height * (content.v1 - content.v0),
        ]
    };
    let mut out = Vec::with_capacity(local.len());
    for [x, y] in local {
        let (canvas, w) = plane.place(x, y)?;
        out.push(Vertex {
            canvas,
            w: w.max(W_MIN),
            uv: uv(x, y),
            local: [x, y],
        });
    }
    // Seen edge-on the polygon has no area and draws nothing.
    (signed_area(&out).abs() > 0.01).then_some(out)
}

/// Sutherland–Hodgman against one half-plane, keeping `inside(pt) >= 0`.
pub fn clip_half_plane(poly: &[[f32; 2]], inside: impl Fn([f32; 2]) -> f32) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let (da, db) = (inside(a), inside(b));
        if da >= 0.0 {
            out.push(a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            let t = da / (da - db);
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    out
}

pub fn signed_area(poly: &[Vertex]) -> f32 {
    let mut area = 0.0;
    for i in 0..poly.len() {
        let a = poly[i].canvas;
        let b = poly[(i + 1) % poly.len()].canvas;
        area += a[0] * b[1] - b[0] * a[1];
    }
    area * 0.5
}

/// Edge lines of a convex canvas polygon as `(a, b, c)` with `a·x + b·y + c` the signed
/// distance in pixels, positive inside. For the fragment shader's analytic coverage.
pub fn edges(poly: &[Vertex]) -> Vec<[f32; 3]> {
    let sign = signed_area(poly).signum();
    (0..poly.len())
        .map(|i| {
            let a = poly[i].canvas;
            let b = poly[(i + 1) % poly.len()].canvas;
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dy * dy).sqrt().max(1e-12);
            // Inward normal for the polygon's winding.
            let (nx, ny) = (-dy * sign / len, dx * sign / len);
            [nx, ny, -(nx * a[0] + ny * a[1])]
        })
        .collect()
}

/// The polygon pushed out by `by` pixels on the canvas, each new vertex mapped back into
/// the plane so its `w` and texture coordinate stay exact. Analytic coverage needs the
/// pixels just outside each edge rasterized.
pub fn expand(g: &ItemGeometry, content: &ContentRect, poly: &[Vertex], by: f32) -> Vec<Vertex> {
    let plane = Plane::of(g);
    let lines = edges(poly);
    let n = poly.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // Vertex i joins edge i−1 and edge i. Move both lines outward and intersect them.
        let l0 = lines[(i + n - 1) % n];
        let l1 = lines[i];
        let (c0, c1) = (l0[2] + by, l1[2] + by);
        let det = l0[0] * l1[1] - l0[1] * l1[0];
        let canvas = if det.abs() < 1e-6 {
            poly[i].canvas
        } else {
            [
                (-c0 * l1[1] + c1 * l0[1]) / det,
                (-l0[0] * c1 + l1[0] * c0) / det,
            ]
        };
        // A miter of a very sharp corner can be long; cap it at 4 px from the vertex.
        let (dx, dy) = (canvas[0] - poly[i].canvas[0], canvas[1] - poly[i].canvas[1]);
        let len = (dx * dx + dy * dy).sqrt();
        let canvas = if len > 4.0 * by {
            let s = 4.0 * by / len;
            [poly[i].canvas[0] + dx * s, poly[i].canvas[1] + dy * s]
        } else {
            canvas
        };
        let vertex = plane
            .unplace(canvas)
            .and_then(|[x, y]| {
                let w = plane.w(x, y);
                (w >= W_MIN * 0.5).then(|| Vertex {
                    canvas,
                    w,
                    uv: [
                        content.u0 + (x - content.left) / content.width * (content.u1 - content.u0),
                        content.v0 + (y - content.top) / content.height * (content.v1 - content.v0),
                    ],
                    local: [x, y],
                })
            })
            .unwrap_or(poly[i]);
        out.push(vertex);
    }
    out
}

fn rotate_z(x: f32, y: f32, z: f32, degrees: f32) -> (f32, f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x * cos - y * sin, x * sin + y * cos, z)
}

fn rotate_y(x: f32, y: f32, z: f32, degrees: f32) -> (f32, f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x * cos + z * sin, y, -x * sin + z * cos)
}

fn rotate_x(x: f32, y: f32, z: f32, degrees: f32) -> (f32, f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x, y * cos - z * sin, y * sin + z * cos)
}

#[cfg(test)]
mod tests {
    use jstream_protocol::{Crop, Finite32, FitMode, ItemGeometry, Placed, Size, Vec2};

    use super::{Plane, W_MIN, polygon};

    pub fn geometry(tilt: f32, turn: f32, rot: f32, depth: f32, p: Option<f32>) -> ItemGeometry {
        let mut g = ItemGeometry::try_new(
            Vec2::new(480.0, 270.0).unwrap(),
            Size::try_new(400.0, 300.0).unwrap(),
        )
        .unwrap();
        g.fit_mode = FitMode::Stretch;
        g.tilt_deg = Finite32::new(tilt).unwrap();
        g.turn_deg = Finite32::new(turn).unwrap();
        g.rotation_deg = Finite32::new(rot).unwrap();
        g.depth_px = Finite32::new(depth).unwrap();
        g.perspective_px = p.map(|p| Finite32::new(p).unwrap());
        g
    }

    #[test]
    fn matches_the_shared_formula() {
        for (tilt, turn, rot, depth, p) in [
            (0.0, 0.0, 0.0, 0.0, None),
            (30.0, -20.0, 15.0, 40.0, Some(800.0)),
            (-60.0, 45.0, 90.0, -100.0, Some(500.0)),
            (70.0, 10.0, -33.0, 0.0, None),
        ] {
            let g = geometry(tilt, turn, rot, depth, p);
            let plane = Plane::of(&g);
            for (x, y) in [(-200.0, -150.0), (200.0, -150.0), (37.0, 91.0), (0.0, 0.0)] {
                let Placed::At(expected) = g.place(x, y).unwrap() else {
                    panic!("behind the viewer");
                };
                let (got, _) = plane.place(x, y).unwrap();
                assert!(
                    (got[0] - expected.x).abs() < 1e-3,
                    "{got:?} vs {expected:?}"
                );
                assert!(
                    (got[1] - expected.y).abs() < 1e-3,
                    "{got:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn unplace_inverts_place() {
        let g = geometry(-50.0, 35.0, 12.0, 60.0, Some(600.0));
        let plane = Plane::of(&g);
        for (x, y) in [(-180.0, -120.0), (150.0, 100.0), (3.0, -7.0)] {
            let (canvas, _) = plane.place(x, y).unwrap();
            let back = plane.unplace(canvas).unwrap();
            assert!(
                (back[0] - x).abs() < 1e-2 && (back[1] - y).abs() < 1e-2,
                "{back:?}"
            );
        }
    }

    #[test]
    fn a_box_reaching_behind_the_viewer_is_clipped_not_dropped() {
        // Tilted toward the viewer and pushed forward: the top edge is behind it.
        let g = geometry(-70.0, 0.0, 0.0, 250.0, Some(300.0));
        assert!(
            g.project()
                .unwrap()
                .corners
                .iter()
                .any(|c| matches!(c, Placed::BehindViewer))
        );
        let content = g.content(400, 300, Crop::default()).unwrap().unwrap();
        let poly = polygon(&g, &content).expect("the near part still draws");
        assert!(poly.len() >= 3);
        assert!(poly.iter().all(|v| v.w >= W_MIN * 0.999));
    }

    #[test]
    fn edge_on_draws_nothing() {
        let g = geometry(0.0, 90.0, 0.0, 0.0, Some(800.0));
        let content = g.content(400, 300, Crop::default()).unwrap().unwrap();
        assert!(polygon(&g, &content).is_none());
    }
}
