//! What a geometry item draws in 3D, and the inverse map (ADR-0050 decisions 1 to 3,
//! docs/adr/0050-3d-transforms-rendering.md).
//!
//! ADR-0022's point formula ([`ItemGeometry::place`]) is kept, in homogeneous form: each
//! drawn vertex also carries `w = (p − z) / p`, the reciprocal of its magnification (1
//! without perspective), so the compositor can interpolate texture coordinates
//! perspective-correctly. The content rectangle is clipped to the box and then by the
//! near plane, both in the box's local frame, where texture coordinates are linear, so
//! both clips are exact. The engine and the UI draw, outline and hit-test from the same
//! polygon and the same inverse map.

use super::{CanvasPoint, ContentRect, GeometryError, ItemGeometry, rotate_x, rotate_y, rotate_z};

/// The near plane: the smallest `w` a drawn point may have (ADR-0050 decision 2). A part
/// of an item closer to the viewer than `p / 256`, so magnified more than 256 times, is
/// not drawn.
pub const NEAR_W: f32 = 1.0 / 256.0;

/// How far from the canvas origin, in canvas pixels along either axis, a vertex of an
/// accepted 3D item's drawn polygon may be ([`ItemGeometry::check_drawable`]). Far past
/// any canvas, and far enough inside f32's range that the compositor's edge and
/// push-out arithmetic on such a vertex stays finite.
pub const MAX_DRAWN_PX: f32 = 1e15;

/// A polygon with less area than this, in square canvas pixels, draws nothing:
/// too small to see.
const MIN_AREA: f32 = 0.01;

/// How small the plane's area scale on the canvas may get before the item is seen
/// edge-on: the facing ([`Plane::facing`]) for the whole item, and the same scale at one
/// canvas point for [`ItemGeometry::unplace`]. A quarter turn leaves a few times 1e-8 in
/// f32, not 0, whatever the item's size.
const EDGE_ON: f64 = 1e-6;

/// The most vertices a drawn polygon has: a rectangle cut by one line.
const MAX_VERTICES: usize = 5;

/// A point in the box's local frame: pixels from the box centre, x right and y down,
/// before rotation and 3D.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalPoint {
    /// Horizontal offset from the centre.
    pub x: f32,
    /// Vertical offset from the centre, y down.
    pub y: f32,
}

/// One vertex of a [`DrawnPolygon`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnVertex {
    /// Where it lands on the canvas: [`ItemGeometry::place`] of `local`.
    pub canvas: CanvasPoint,
    /// The point in the box's local frame.
    pub local: LocalPoint,
    /// The source texture coordinate, `[u, v]` with v down, linear in `local`.
    pub uv: [f32; 2],
    /// `(p − z) / p`, 1 without perspective; never below [`NEAR_W`]. A compositor emits
    /// the clip position `(ndc · w, 0, w)`, so the GPU divides the interpolated texture
    /// coordinate by it.
    pub w: f32,
}

/// The convex polygon a geometry item draws ([`ItemGeometry::drawn_polygon`]): three to
/// five vertices, in the content rectangle's order (top-left, top-right, bottom-right,
/// bottom-left before rotation), with any near-plane cut in its place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnPolygon {
    vertices: [DrawnVertex; MAX_VERTICES],
    len: usize,
}

impl DrawnPolygon {
    /// The vertices, three to five.
    pub fn vertices(&self) -> &[DrawnVertex] {
        &self.vertices[..self.len]
    }
}

/// The placement's affine part: local `(x, y, 0)` goes to `ax · x + ay · y + (0, 0,
/// depth)`, and the projection divides x and y by `w`.
#[derive(Clone, Copy, Debug)]
struct Plane {
    ax: [f32; 3],
    ay: [f32; 3],
    depth: f32,
    perspective: Option<f32>,
    center: [f32; 2],
}

impl Plane {
    fn of(geometry: ItemGeometry) -> Self {
        let axis = |x: f32, y: f32| {
            let (x, y, z) = rotate_z(x, y, 0.0, geometry.rotation_deg.get());
            let (x, y, z) = rotate_y(x, y, z, geometry.turn_deg.get());
            let (x, y, z) = rotate_x(x, y, z, geometry.tilt_deg.get());
            [x, y, z]
        };
        Self {
            ax: axis(1.0, 0.0),
            ay: axis(0.0, 1.0),
            depth: geometry.depth_px.get(),
            perspective: geometry.perspective_px.map(|p| p.get()),
            center: [geometry.center.x.get(), geometry.center.y.get()],
        }
    }

    /// The z of the plane's unit normal, `ax × ay`: 1 facing the viewer, 0 edge-on. With
    /// perspective, 0 means the plane passes through the viewer, so every point of it is
    /// seen edge-on; without, it is seen side-on. It does not depend on the item's size,
    /// so it decides edge-on where an area would not. Under [`EDGE_ON`] is a turn within
    /// about 6e-5° of a quarter, so anything visibly turned still draws.
    fn facing(&self) -> f64 {
        f64::from(self.ax[0]) * f64::from(self.ay[1])
            - f64::from(self.ax[1]) * f64::from(self.ay[0])
    }

    /// The viewing-axis coordinate of a local point, positive toward the viewer.
    fn z(&self, x: f32, y: f32) -> f32 {
        self.ax[2] * x + self.ay[2] * y + self.depth
    }

    /// `w` of a local point: at or below 0 it is at or behind the viewer.
    fn w(&self, x: f32, y: f32) -> f32 {
        match self.perspective {
            Some(p) => (p - self.z(x, y)) / p,
            None => 1.0,
        }
    }

    /// The canvas point of a local point with `w > 0`.
    fn canvas(&self, x: f32, y: f32, w: f32) -> CanvasPoint {
        let x3 = self.ax[0] * x + self.ay[0] * y;
        let y3 = self.ax[1] * x + self.ay[1] * y;
        CanvasPoint {
            x: self.center[0] + x3 / w,
            y: self.center[1] + y3 / w,
        }
    }
}

impl ItemGeometry {
    /// The polygon this item draws for `content` ([`Self::content`]): the content
    /// rectangle clipped to the box (what [`FitMode::Fill`](super::FitMode::Fill) needs),
    /// then to `w ≥` [`NEAR_W`], each vertex placed by ADR-0022's formula. `Ok(None)`
    /// when nothing is left: the content misses the box, the whole item is behind the
    /// near plane, it is seen edge-on (whatever its size), or it covers under 0.01 px².
    ///
    /// With neutral 3D every `w` is 1 and the polygon is the 2D compositor's quad, its
    /// texture coordinates clipped to the box.
    ///
    /// # Errors
    /// The same as [`Self::validate`], or [`GeometryError::NotFinite`] when a vertex is
    /// not finite.
    pub fn drawn_polygon(
        self,
        content: &ContentRect,
    ) -> Result<Option<DrawnPolygon>, GeometryError> {
        self.validate()?;
        let plane = Plane::of(self);
        if plane.facing().abs() < EDGE_ON {
            return Ok(None);
        }
        let half_w = self.size.width.get() * 0.5;
        let half_h = self.size.height.get() * 0.5;
        let left = content.left.max(-half_w);
        let top = content.top.max(-half_h);
        let right = (content.left + content.width).min(half_w);
        let bottom = (content.top + content.height).min(half_h);
        if !(left < right && top < bottom) {
            return Ok(None);
        }
        let mut local = Clip::default();
        for point in [[left, top], [right, top], [right, bottom], [left, bottom]] {
            local.push(point);
        }
        if let Some(p) = plane.perspective {
            // w ≥ NEAR_W  ⇔  z ≤ p · (1 − NEAR_W): one half-plane in local x and y.
            let z_max = p * (1.0 - NEAR_W);
            local = local.keep(|[x, y]| z_max - plane.z(x, y));
            if local.len < 3 {
                return Ok(None);
            }
        }
        let unused = DrawnVertex {
            canvas: CanvasPoint { x: 0.0, y: 0.0 },
            local: LocalPoint::default(),
            uv: [0.0, 0.0],
            w: 1.0,
        };
        let mut polygon = DrawnPolygon {
            vertices: [unused; MAX_VERTICES],
            len: local.len,
        };
        for (vertex, &[x, y]) in polygon.vertices.iter_mut().zip(local.points()) {
            // The clip leaves w ≥ NEAR_W up to rounding; the floor keeps it there.
            *vertex = drawn_vertex(&plane, content, x, y, plane.w(x, y).max(NEAR_W))?;
        }
        Ok((signed_area(polygon.vertices()).abs() > MIN_AREA).then_some(polygon))
    }

    /// Whether the compositor can draw this geometry, so an edit may store it (ADR-0022:
    /// stored state never claims what isn't drawn). With neutral 3D that is
    /// [`Self::project`]'s finite corners. Otherwise the polygon the whole box draws
    /// ([`Self::drawn_polygon`], which bounds the polygon of any content, since content
    /// is clipped to the box) must have finite vertices within [`MAX_DRAWN_PX`] of the
    /// origin: a huge item whose near-plane clip magnifies it 256 times, or a centre near
    /// f32's limit, would otherwise overflow while it is drawn. An item that draws
    /// nothing (behind the viewer, edge-on) is drawable.
    ///
    /// # Errors
    /// The same as [`Self::validate`], [`GeometryError::NotFinite`] when a corner or a
    /// vertex is not finite, or [`GeometryError::TooFar`] when a vertex is past
    /// [`MAX_DRAWN_PX`].
    pub fn check_drawable(self) -> Result<(), GeometryError> {
        self.validate()?;
        self.project()?;
        if self.is_neutral_3d() {
            return Ok(());
        }
        let width = self.size.width.get();
        let height = self.size.height.get();
        let whole_box = ContentRect {
            left: -width * 0.5,
            top: -height * 0.5,
            width,
            height,
            u0: 0.0,
            v0: 0.0,
            u1: 1.0,
            v1: 1.0,
        };
        let Some(polygon) = self.drawn_polygon(&whole_box)? else {
            return Ok(());
        };
        let near = |value: f32| value.abs() <= MAX_DRAWN_PX;
        if polygon
            .vertices()
            .iter()
            .all(|vertex| near(vertex.canvas.x) && near(vertex.canvas.y))
        {
            Ok(())
        } else {
            Err(GeometryError::TooFar)
        }
    }

    /// The local point that `point` on the canvas shows: the inverse of
    /// [`Self::place`] (ADR-0050 decision 3). `Ok(None)` when the item is seen edge-on,
    /// or when that local point is behind the near plane ([`NEAR_W`]), so nothing is
    /// drawn there. It is not checked against the box or the content: a caller that
    /// hit-tests compares it with them.
    ///
    /// # Errors
    /// The same as [`Self::validate`], or [`GeometryError::NotFinite`] for a point that
    /// is not finite.
    pub fn unplace(self, point: CanvasPoint) -> Result<Option<LocalPoint>, GeometryError> {
        self.validate()?;
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(GeometryError::NotFinite);
        }
        let plane = Plane::of(self);
        if plane.facing().abs() < EDGE_ON {
            return Ok(None);
        }
        // With (X, Y) the point from the centre, X · (p − z) = p · x3 and the same for
        // Y: linear in the local x and y, so a 2×2 solve, in f64 for the near-edge-on
        // cases.
        let cx = f64::from(point.x - plane.center[0]);
        let cy = f64::from(point.y - plane.center[1]);
        let (ax, ay) = (plane.ax.map(f64::from), plane.ay.map(f64::from));
        let depth = f64::from(plane.depth);
        let (m, rhs, scale) = match plane.perspective.map(f64::from) {
            Some(p) => (
                [
                    [p * ax[0] + cx * ax[2], p * ay[0] + cx * ay[2]],
                    [p * ax[1] + cy * ax[2], p * ay[1] + cy * ay[2]],
                ],
                [cx * (p - depth), cy * (p - depth)],
                p * p,
            ),
            None => ([[ax[0], ay[0]], [ax[1], ay[1]]], [cx, cy], 1.0),
        };
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        // The determinant is how much the plane's area shrinks on the canvas (times p²
        // with perspective). A quarter turn leaves a few times 1e-8 in f32, not 0, so anything
        // under a millionth is edge-on.
        if (det / scale).abs() < EDGE_ON {
            return Ok(None);
        }
        let x = ((rhs[0] * m[1][1] - m[0][1] * rhs[1]) / det) as f32;
        let y = ((m[0][0] * rhs[1] - rhs[0] * m[1][0]) / det) as f32;
        if !x.is_finite() || !y.is_finite() {
            return Ok(None);
        }
        Ok((plane.w(x, y) >= NEAR_W).then_some(LocalPoint { x, y }))
    }

    /// The [`DrawnVertex`] of one local point for `content`: its canvas point, texture
    /// coordinate and `w`, as [`Self::drawn_polygon`] gives its own vertices. A
    /// compositor uses it for a point [`Self::unplace`] found, such as a vertex of the
    /// polygon pushed out for edge coverage. `Ok(None)` when the point is behind the
    /// near plane. The point is not checked against the box or the content.
    ///
    /// # Errors
    /// The same as [`Self::validate`], or [`GeometryError::NotFinite`] when the point or
    /// the vertex is not finite.
    pub fn drawn_vertex(
        self,
        content: &ContentRect,
        local: LocalPoint,
    ) -> Result<Option<DrawnVertex>, GeometryError> {
        self.validate()?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(GeometryError::NotFinite);
        }
        let plane = Plane::of(self);
        let w = plane.w(local.x, local.y);
        if w < NEAR_W {
            return Ok(None);
        }
        drawn_vertex(&plane, content, local.x, local.y, w).map(Some)
    }
}

/// The vertex at local `(x, y)`, with `w` already found (and floored, for the polygon's
/// own vertices).
fn drawn_vertex(
    plane: &Plane,
    content: &ContentRect,
    x: f32,
    y: f32,
    w: f32,
) -> Result<DrawnVertex, GeometryError> {
    let canvas = plane.canvas(x, y, w);
    let uv = [
        content.u0 + (x - content.left) / content.width * (content.u1 - content.u0),
        content.v0 + (y - content.top) / content.height * (content.v1 - content.v0),
    ];
    if ![canvas.x, canvas.y, uv[0], uv[1]]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(GeometryError::NotFinite);
    }
    Ok(DrawnVertex {
        canvas,
        local: LocalPoint { x, y },
        uv,
        w,
    })
}

/// Up to [`MAX_VERTICES`] local points, clipped in place without allocating.
#[derive(Clone, Copy, Default)]
struct Clip {
    points: [[f32; 2]; MAX_VERTICES],
    len: usize,
}

impl Clip {
    fn push(&mut self, point: [f32; 2]) {
        if let Some(slot) = self.points.get_mut(self.len) {
            *slot = point;
            self.len += 1;
        }
    }

    fn points(&self) -> &[[f32; 2]] {
        &self.points[..self.len]
    }

    /// Sutherland–Hodgman against one half-plane, keeping `inside(point) >= 0`. One cut
    /// of a convex polygon adds at most one vertex, and the input here is a rectangle.
    fn keep(&self, inside: impl Fn([f32; 2]) -> f32) -> Self {
        let mut out = Self::default();
        let points = self.points();
        for (i, &a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
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
}

/// The canvas polygon's signed (shoelace) area, positive for clockwise on a y-down
/// canvas.
fn signed_area(vertices: &[DrawnVertex]) -> f32 {
    let mut area = 0.0;
    for (i, a) in vertices.iter().enumerate() {
        let b = vertices[(i + 1) % vertices.len()];
        area += a.canvas.x * b.canvas.y - b.canvas.x * a.canvas.y;
    }
    area * 0.5
}

#[cfg(test)]
mod tests;
