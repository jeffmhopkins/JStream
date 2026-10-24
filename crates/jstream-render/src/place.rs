//! Item placement: a legacy 0.3 transform through [`jstream_protocol::legacy_quad`], or
//! an ADR-0022 geometry through [`ItemGeometry::content`] and [`ItemGeometry::place`],
//! and with 3D through ADR-0050's [`ItemGeometry::drawn_polygon`].

use jstream_protocol::{
    CanvasPoint, ContentRect, Crop as ProtocolCrop, DrawnVertex, Finite32, GeometryError,
    ItemGeometry, ItemTransform, Placed, Vec2, legacy_quad,
};

use crate::error::RenderError;
use crate::mips::Region;
use crate::scene::{Crop, LegacyTransform, Placement};

/// The most vertices a [`Shape`] has: a rectangle cut by the near plane.
pub(crate) const MAX_VERTICES: usize = 5;

/// An edge line no pixel is outside of: an item drawn without edge coverage has only
/// these.
pub(crate) const NO_EDGE: [f32; 3] = [0.0, 0.0, 1.0e9];

/// Texture-coordinate bounds that never clamp: a 2D item has no pushed-out fringe.
pub(crate) const NO_UV_BOUNDS: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

/// How far, in canvas pixels, a 3D item's polygon is pushed out so the pixels its edges
/// partly cover are drawn (ADR-0050 decision 6).
const PUSH_OUT: f32 = 1.0;

/// One drawn vertex: a canvas point (pixels, y down), its texture coordinate, and `w`,
/// which is 1 for a 2D item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Vertex {
    pub canvas: [f32; 2],
    pub uv: [f32; 2],
    pub w: f32,
}

/// What one item draws: a convex polygon of three to five vertices, and the edge lines
/// its coverage is measured from.
///
/// A 2D item is the four corners top-left, top-right, bottom-right, bottom-left, and has
/// only [`NO_EDGE`]s. A 3D item is ADR-0050's drawn polygon pushed out by a pixel, and
/// its edges are the polygon's own, before the push: `[nx, ny, c]`, where
/// `nx · x + ny · y + c` is the signed distance in canvas pixels, positive inside.
pub(crate) struct Shape {
    vertices: [Vertex; MAX_VERTICES],
    len: usize,
    pub edges: [[f32; 3]; MAX_VERTICES],
    /// `[u_min, v_min, u_max, v_max]` of the polygon before the push: the shader clamps
    /// to it, so a fringe pixel samples the content's edge, not the crop or what `fill`
    /// clips away. [`NO_UV_BOUNDS`] for a 2D item.
    pub uv_bounds: [f32; 4],
    /// The crop's texels, in whole source pixels: what a 3D item's mip chain is built
    /// from. Carried as integers from the item's crop, never recovered from its texture
    /// coordinates, which don't round-trip through `f32`. `None` for a 2D item.
    pub crop: Option<Region>,
}

impl Shape {
    /// The vertices, in order around the polygon.
    pub(crate) fn vertices(&self) -> &[Vertex] {
        &self.vertices[..self.len]
    }

    /// Whether this is a 3D item's shape, which has edge lines and samples mips.
    pub(crate) fn is_3d(&self) -> bool {
        self.edges[0] != NO_EDGE
    }

    /// Maps the texture coordinates, the vertices' and the clamp bounds, from the
    /// source's into a chain's whose `[0, 1]` spans `[u, v, width, height]` of it.
    pub(crate) fn remap_uv(&mut self, onto: [f32; 4]) {
        let map = |value: f32, start: f32, span: f32| (value - start) / span;
        for vertex in &mut self.vertices[..self.len] {
            vertex.uv = [
                map(vertex.uv[0], onto[0], onto[2]),
                map(vertex.uv[1], onto[1], onto[3]),
            ];
        }
        if self.uv_bounds != NO_UV_BOUNDS {
            self.uv_bounds = [
                map(self.uv_bounds[0], onto[0], onto[2]),
                map(self.uv_bounds[1], onto[1], onto[3]),
                map(self.uv_bounds[2], onto[0], onto[2]),
                map(self.uv_bounds[3], onto[1], onto[3]),
            ];
        }
    }

    /// A 2D quad: `w = 1` and no edge coverage.
    fn flat(corners: [[f32; 2]; 4], uvs: [[f32; 2]; 4]) -> Self {
        let mut vertices = [UNUSED; MAX_VERTICES];
        for (vertex, (canvas, uv)) in vertices.iter_mut().zip(corners.into_iter().zip(uvs)) {
            *vertex = Vertex { canvas, uv, w: 1.0 };
        }
        Self {
            vertices,
            len: 4,
            edges: [NO_EDGE; MAX_VERTICES],
            uv_bounds: NO_UV_BOUNDS,
            crop: None,
        }
    }
}

const UNUSED: Vertex = Vertex {
    canvas: [0.0, 0.0],
    uv: [0.0, 0.0],
    w: 1.0,
};

/// The shape for one item, or `Ok(None)` when the crop, the scale or the 3D leaves
/// nothing to draw.
///
/// A non-finite component, including a product that overflows, is
/// [`RenderError::Invalid`].
///
/// # Errors
/// [`RenderError::Invalid`] when a placement component is not finite or not drawable.
pub(crate) fn shape(
    source_width: u32,
    source_height: u32,
    placement: Placement,
    crop: Crop,
) -> Result<Option<Shape>, RenderError> {
    let crop = ProtocolCrop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    };
    match placement {
        Placement::Legacy(transform) => legacy(source_width, source_height, transform, crop),
        Placement::Geometry(geometry) => boxed(source_width, source_height, geometry, crop),
    }
}

/// Corners and UVs from [`legacy_quad`].
fn legacy(
    source_width: u32,
    source_height: u32,
    transform: LegacyTransform,
    crop: ProtocolCrop,
) -> Result<Option<Shape>, RenderError> {
    let transform = ItemTransform {
        position: finite_vec(transform.position)?,
        scale: finite_vec(transform.scale)?,
        rotation_deg: Finite32::new(transform.rotation_deg).ok_or_else(not_finite)?,
    };
    let placed =
        legacy_quad(transform, crop, source_width, source_height).map_err(placement_error)?;
    Ok(placed.map(|quad| Shape::flat(quad.corners.map(|point| [point.x, point.y]), quad.uvs)))
}

/// The content rectangle the shared formula fits into the box, clipped to the box (only
/// `fill` reaches past it), with its UVs cut to match, then placed corner by corner.
///
/// Clipping happens in the box's local frame, before rotation, so it is exact for any
/// in-plane rotation. With 3D neutral every corner is in front of the viewer and the
/// placement is affine, so linear UVs are correct. A flip arrives as the content's
/// swapped UV pair, so the same interpolation mirrors the clipped part. Any other 3D is
/// [`drawn`].
fn boxed(
    source_width: u32,
    source_height: u32,
    geometry: ItemGeometry,
    crop: ProtocolCrop,
) -> Result<Option<Shape>, RenderError> {
    let Some(content) = geometry
        .content(source_width, source_height, crop)
        .map_err(placement_error)?
    else {
        return Ok(None);
    };
    if !geometry.is_neutral_3d() {
        return drawn(
            geometry,
            &content,
            crop_region(source_width, source_height, crop),
        );
    }
    let half_w = geometry.size.width.get() * 0.5;
    let half_h = geometry.size.height.get() * 0.5;
    let left = content.left.max(-half_w);
    let top = content.top.max(-half_h);
    let right = (content.left + content.width).min(half_w);
    let bottom = (content.top + content.height).min(half_h);
    if left >= right || top >= bottom {
        return Ok(None);
    }
    // Where an edge falls along the content rectangle, as a texture coordinate.
    let u = |x: f32| content.u0 + (x - content.left) / content.width * (content.u1 - content.u0);
    let v = |y: f32| content.v0 + (y - content.top) / content.height * (content.v1 - content.v0);
    let local = [(left, top), (right, top), (right, bottom), (left, bottom)];
    let mut corners = [[0.0; 2]; 4];
    let mut uvs = [[0.0; 2]; 4];
    for (i, (x, y)) in local.into_iter().enumerate() {
        corners[i] = match geometry.place(x, y).map_err(placement_error)? {
            Placed::At(point) => [point.x, point.y],
            _ => return Err(not_finite()),
        };
        uvs[i] = [u(x), v(y)];
    }
    Ok(Some(Shape::flat(corners, uvs)))
}

/// The texels `crop` keeps of a `source_width` × `source_height` source, at least one
/// on each axis and never past the source. `ItemGeometry::content` has already returned
/// `None` for a crop that keeps nothing, so the clamps only guard the arithmetic.
fn crop_region(source_width: u32, source_height: u32, crop: ProtocolCrop) -> Region {
    let span = |start: u32, end: u32, size: u32| {
        let start = start.min(size.saturating_sub(1));
        let width = size.saturating_sub(start).saturating_sub(end).max(1);
        (start, width)
    };
    let (x, width) = span(crop.left, crop.right, source_width);
    let (y, height) = span(crop.top, crop.bottom, source_height);
    Region {
        x,
        y,
        width,
        height,
    }
}

/// A 3D item (ADR-0050 decisions 1, 2 and 6): the drawn polygon, clipped to the box and
/// the near plane, with its edge lines, and each vertex pushed out by [`PUSH_OUT`] along
/// both its edges. The pushed point is mapped back into the plane, so its `w` and texture
/// coordinate stay exact; where it can't be (past the near plane), the vertex stays.
fn drawn(
    geometry: ItemGeometry,
    content: &ContentRect,
    crop: Region,
) -> Result<Option<Shape>, RenderError> {
    let Some(polygon) = geometry.drawn_polygon(content).map_err(placement_error)? else {
        return Ok(None);
    };
    let corners = polygon.vertices();
    let len = corners.len();
    let edges = edge_lines(corners);
    let mut uv_bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for corner in corners {
        uv_bounds[0] = uv_bounds[0].min(corner.uv[0]);
        uv_bounds[1] = uv_bounds[1].min(corner.uv[1]);
        uv_bounds[2] = uv_bounds[2].max(corner.uv[0]);
        uv_bounds[3] = uv_bounds[3].max(corner.uv[1]);
    }
    let mut vertices = [UNUSED; MAX_VERTICES];
    for (i, corner) in corners.iter().enumerate() {
        let target = push_out(edges[(i + len - 1) % len], edges[i], corner);
        let pushed = match geometry.unplace(target).map_err(placement_error)? {
            Some(local) => geometry
                .drawn_vertex(content, local)
                .map_err(placement_error)?,
            None => None,
        };
        vertices[i] = vertex(&pushed.unwrap_or(*corner));
    }
    Ok(Some(Shape {
        vertices,
        len,
        edges,
        uv_bounds,
        crop: Some(crop),
    }))
}

fn vertex(drawn: &DrawnVertex) -> Vertex {
    Vertex {
        canvas: [drawn.canvas.x, drawn.canvas.y],
        uv: drawn.uv,
        w: drawn.w,
    }
}

/// Each edge `i` (vertex `i` to `i + 1`) as a line with its normal pointing inside,
/// whichever way the polygon winds; [`NO_EDGE`] past the last.
fn edge_lines(corners: &[DrawnVertex]) -> [[f32; 3]; MAX_VERTICES] {
    let mut area = 0.0;
    for (i, a) in corners.iter().enumerate() {
        let b = corners[(i + 1) % corners.len()];
        area += a.canvas.x * b.canvas.y - b.canvas.x * a.canvas.y;
    }
    let sign = if area < 0.0 { -1.0 } else { 1.0 };
    let mut edges = [NO_EDGE; MAX_VERTICES];
    for (i, (edge, a)) in edges.iter_mut().zip(corners).enumerate() {
        let b = corners[(i + 1) % corners.len()];
        let (dx, dy) = (b.canvas.x - a.canvas.x, b.canvas.y - a.canvas.y);
        let len = (dx * dx + dy * dy).sqrt().max(1e-12);
        let (nx, ny) = (-dy * sign / len, dx * sign / len);
        *edge = [nx, ny, -(nx * a.canvas.x + ny * a.canvas.y)];
    }
    edges
}

/// Where the corner between edges `before` and `after` goes when both move out by
/// [`PUSH_OUT`]: the two moved lines' crossing, at most `4 · PUSH_OUT` from the corner
/// (a very sharp corner's miter is long), or the corner itself when the edges are
/// parallel.
fn push_out(before: [f32; 3], after: [f32; 3], corner: &DrawnVertex) -> CanvasPoint {
    let at = corner.canvas;
    let (c0, c1) = (before[2] + PUSH_OUT, after[2] + PUSH_OUT);
    let det = before[0] * after[1] - before[1] * after[0];
    if det.abs() < 1e-6 {
        return at;
    }
    let x = (-c0 * after[1] + c1 * before[1]) / det;
    let y = (-before[0] * c1 + after[0] * c0) / det;
    let (dx, dy) = (x - at.x, y - at.y);
    let len = (dx * dx + dy * dy).sqrt();
    let cap = 4.0 * PUSH_OUT;
    if len > cap {
        let s = cap / len;
        return CanvasPoint {
            x: at.x + dx * s,
            y: at.y + dy * s,
        };
    }
    CanvasPoint { x, y }
}

fn finite_vec(xy: [f32; 2]) -> Result<Vec2, RenderError> {
    Vec2::new(xy[0], xy[1]).ok_or_else(not_finite)
}

fn not_finite() -> RenderError {
    RenderError::Invalid {
        what: "placement",
        detail: "a component is not finite".into(),
    }
}

/// [`GeometryError::NotFinite`] keeps the long-standing non-finite message; any other
/// variant keeps its own.
fn placement_error(err: GeometryError) -> RenderError {
    match err {
        GeometryError::NotFinite => not_finite(),
        other => RenderError::Invalid {
            what: "placement",
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use jstream_protocol::GeometryError;

    use jstream_protocol::{Finite32, FitMode, ItemGeometry, Size, Vec2};

    use super::{NO_EDGE, NO_UV_BOUNDS, Shape, placement_error, shape};
    use crate::error::RenderError;
    use crate::mips::Region;
    use crate::scene::{Crop, LegacyTransform, Placement};

    fn near(got: [f32; 2], expected: [f32; 2]) {
        assert!(
            (got[0] - expected[0]).abs() < 1e-3,
            "{got:?} vs {expected:?}"
        );
        assert!(
            (got[1] - expected[1]).abs() < 1e-3,
            "{got:?} vs {expected:?}"
        );
    }

    #[test]
    fn identity_is_the_source_rectangle_at_the_top_left() {
        let q = shape(4, 2, Placement::IDENTITY, Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(q.corners()[0], [0.0, 0.0]);
        assert_eq!(q.corners()[1], [4.0, 0.0]);
        assert_eq!(q.corners()[2], [4.0, 2.0]);
        assert_eq!(q.corners()[3], [0.0, 2.0]);
        assert_eq!(q.uvs()[0], [0.0, 0.0]);
        assert_eq!(q.uvs()[2], [1.0, 1.0]);
    }

    #[test]
    fn crop_then_scale_then_top_left() {
        let placement = Placement::Legacy(LegacyTransform {
            position: [10.0, 4.0],
            scale: [2.0, 3.0],
            rotation_deg: 0.0,
        });
        let crop = Crop {
            left: 2,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let q = shape(4, 2, placement, crop)
            .expect("finite")
            .expect("draws");
        // Cropped size is 2x2, scaled to 4x6, top-left at (10, 4).
        assert_eq!(q.corners()[0], [10.0, 4.0]);
        assert_eq!(q.corners()[1], [14.0, 4.0]);
        assert_eq!(q.corners()[2], [14.0, 10.0]);
        assert_eq!(q.uvs()[0], [0.5, 0.0]);
        assert_eq!(q.uvs()[1], [1.0, 0.0]);
    }

    #[test]
    fn ninety_degrees_clockwise_about_the_top_left() {
        let placement = Placement::Legacy(LegacyTransform {
            position: [30.0, 30.0],
            scale: [1.0, 1.0],
            rotation_deg: 90.0,
        });
        let q = shape(20, 10, placement, Crop::NONE)
            .expect("finite")
            .expect("draws");
        near(q.corners()[0], [30.0, 30.0]);
        near(q.corners()[1], [30.0, 50.0]);
        near(q.corners()[2], [20.0, 50.0]);
        near(q.corners()[3], [20.0, 30.0]);
    }

    #[test]
    fn a_negative_scale_reflects_about_the_top_left() {
        let placement = Placement::Legacy(LegacyTransform {
            position: [8.0, 6.0],
            scale: [-1.0, 1.0],
            rotation_deg: 0.0,
        });
        let q = shape(4, 2, placement, Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(q.corners()[0], [8.0, 6.0]);
        assert_eq!(q.corners()[1], [4.0, 6.0]);
        assert_eq!(q.corners()[2], [4.0, 8.0]);
        assert_eq!(q.corners()[3], [8.0, 8.0]);
    }

    #[test]
    fn a_crop_or_scale_that_leaves_nothing_draws_nothing() {
        let full = Crop {
            left: 2,
            top: 0,
            right: 2,
            bottom: 0,
        };
        assert!(
            shape(4, 2, Placement::IDENTITY, full)
                .expect("finite")
                .is_none()
        );
        let over = Crop {
            left: 5,
            top: 0,
            right: 0,
            bottom: 0,
        };
        assert!(
            shape(4, 2, Placement::IDENTITY, over)
                .expect("finite")
                .is_none()
        );
        let flat = Placement::Legacy(LegacyTransform {
            scale: [0.0, 1.0],
            ..LegacyTransform::IDENTITY
        });
        assert!(shape(4, 2, flat, Crop::NONE).expect("finite").is_none());
        assert!(
            shape(0, 2, Placement::IDENTITY, Crop::NONE)
                .expect("finite")
                .is_none()
        );
    }

    #[test]
    fn non_finite_placement_is_an_error() {
        let placement = Placement::Legacy(LegacyTransform {
            position: [f32::INFINITY, 0.0],
            ..LegacyTransform::IDENTITY
        });
        assert!(shape(4, 2, placement, Crop::NONE).is_err());
    }

    #[test]
    fn a_geometry_error_other_than_not_finite_keeps_its_own_message() {
        let RenderError::Invalid { what, detail } = placement_error(GeometryError::NonPositiveSize)
        else {
            panic!("a geometry error maps to Invalid");
        };
        assert_eq!(what, "placement");
        assert_eq!(detail, GeometryError::NonPositiveSize.to_string());
        assert_ne!(detail, "a component is not finite");
    }

    #[test]
    fn not_finite_keeps_the_non_finite_message() {
        assert_eq!(
            placement_error(GeometryError::NotFinite),
            RenderError::Invalid {
                what: "placement",
                detail: "a component is not finite".into(),
            }
        );
    }

    fn boxed(width: f32, height: f32, fit_mode: FitMode, rotation_deg: f32) -> Placement {
        Placement::Geometry(ItemGeometry {
            fit_mode,
            rotation_deg: Finite32::new(rotation_deg).expect("finite"),
            ..ItemGeometry::try_new(
                Vec2::new(50.0, 40.0).expect("finite"),
                Size::try_new(width, height).expect("positive"),
            )
            .expect("positive")
        })
    }

    #[test]
    fn stretch_fills_the_box_with_the_whole_crop() {
        let q = shape(4, 2, boxed(20.0, 20.0, FitMode::Stretch, 0.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners(),
            [[40.0, 30.0], [60.0, 30.0], [60.0, 50.0], [40.0, 50.0]]
        );
        assert_eq!(q.uvs(), [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    #[test]
    fn fit_letterboxes_inside_the_box() {
        // A 4×2 source in a 20×20 box: 20×10, centred.
        let q = shape(4, 2, boxed(20.0, 20.0, FitMode::Fit, 0.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners(),
            [[40.0, 35.0], [60.0, 35.0], [60.0, 45.0], [40.0, 45.0]]
        );
        assert_eq!(q.uvs()[2], [1.0, 1.0]);
    }

    #[test]
    fn fill_is_clipped_to_the_box_and_its_uvs_cut_to_match() {
        // A 4×2 source filling a 20×20 box is 40×20; the box keeps the middle half.
        let q = shape(4, 2, boxed(20.0, 20.0, FitMode::Fill, 0.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners(),
            [[40.0, 30.0], [60.0, 30.0], [60.0, 50.0], [40.0, 50.0]]
        );
        assert_eq!(
            q.uvs(),
            [[0.25, 0.0], [0.75, 0.0], [0.75, 1.0], [0.25, 1.0]]
        );
    }

    #[test]
    fn a_flip_mirrors_the_clipped_uvs_and_keeps_the_corners() {
        let Placement::Geometry(geometry) = boxed(20.0, 20.0, FitMode::Fill, 0.0) else {
            unreachable!("boxed is a geometry");
        };
        let mirrored = Placement::Geometry(ItemGeometry {
            flip_h: true,
            flip_v: true,
            ..geometry
        });
        // The same middle half as the unflipped fill, sampled right to left and bottom
        // to top.
        let q = shape(4, 2, mirrored, Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners(),
            [[40.0, 30.0], [60.0, 30.0], [60.0, 50.0], [40.0, 50.0]]
        );
        assert_eq!(
            q.uvs(),
            [[0.75, 1.0], [0.25, 1.0], [0.25, 0.0], [0.75, 0.0]]
        );
    }

    #[test]
    fn a_geometry_rotates_about_its_centre_and_crops_first() {
        let crop = Crop {
            left: 2,
            ..Crop::NONE
        };
        // Cropped to 2×2, stretched to the 20×10 box, turned 90° clockwise about (50, 40).
        let q = shape(4, 2, boxed(20.0, 10.0, FitMode::Stretch, 90.0), crop)
            .expect("finite")
            .expect("draws");
        near(q.corners()[0], [55.0, 30.0]);
        near(q.corners()[1], [55.0, 50.0]);
        near(q.corners()[2], [45.0, 50.0]);
        near(q.corners()[3], [45.0, 30.0]);
        assert_eq!(q.uvs()[0], [0.5, 0.0]);
        assert_eq!(q.uvs()[2], [1.0, 1.0]);
    }

    #[test]
    fn a_geometry_crop_that_leaves_nothing_draws_nothing() {
        let crop = Crop {
            left: 4,
            ..Crop::NONE
        };
        assert!(
            shape(4, 2, boxed(20.0, 20.0, FitMode::Fit, 0.0), crop)
                .expect("finite")
                .is_none()
        );
    }

    impl Shape {
        fn corners(&self) -> Vec<[f32; 2]> {
            self.vertices().iter().map(|v| v.canvas).collect()
        }

        fn uvs(&self) -> Vec<[f32; 2]> {
            self.vertices().iter().map(|v| v.uv).collect()
        }
    }

    fn in_3d(tilt: f32, turn: f32, depth: f32, perspective: Option<f32>) -> ItemGeometry {
        let Placement::Geometry(geometry) = boxed(40.0, 30.0, FitMode::Stretch, 15.0) else {
            unreachable!("boxed is a geometry");
        };
        ItemGeometry {
            tilt_deg: Finite32::new(tilt).expect("finite"),
            turn_deg: Finite32::new(turn).expect("finite"),
            depth_px: Finite32::new(depth).expect("finite"),
            perspective_px: perspective.map(|p| Finite32::new(p).expect("finite")),
            ..geometry
        }
    }

    fn distance(edge: [f32; 3], at: [f32; 2]) -> f32 {
        edge[0] * at[0] + edge[1] * at[1] + edge[2]
    }

    #[test]
    fn a_2d_item_has_w_one_and_no_edge_coverage() {
        let q = shape(4, 2, boxed(20.0, 20.0, FitMode::Fit, 30.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert!(q.vertices().iter().all(|v| v.w == 1.0));
        assert_eq!(q.edges, [NO_EDGE; 5]);
        assert_eq!(q.uv_bounds, NO_UV_BOUNDS);
    }

    #[test]
    fn a_3d_item_is_its_drawn_polygon_pushed_out_a_pixel() {
        for geometry in [
            in_3d(0.0, 50.0, 0.0, Some(70.0)),
            in_3d(30.0, -20.0, 5.0, Some(120.0)),
            in_3d(60.0, 0.0, 0.0, None),
        ] {
            let content = geometry
                .content(4, 2, jstream_protocol::Crop::default())
                .expect("content")
                .expect("pixels");
            let polygon = geometry
                .drawn_polygon(&content)
                .expect("ok")
                .expect("drawn");
            let q = shape(4, 2, Placement::Geometry(geometry), Crop::NONE)
                .expect("finite")
                .expect("draws");
            let corners = polygon.vertices();
            let len = corners.len();
            assert_eq!(q.vertices().len(), len);
            // Stretched and uncropped, the content is the whole source.
            assert_eq!(q.uv_bounds, [0.0, 0.0, 1.0, 1.0]);
            assert_eq!(q.edges[len..], [NO_EDGE; 5][len..]);
            let centre = [geometry.center.x.get(), geometry.center.y.get()];
            for (i, (pushed, corner)) in q.vertices().iter().zip(corners).enumerate() {
                let at = [corner.canvas.x, corner.canvas.y];
                // The polygon's corners are on their two edges, and its centre inside.
                assert!(distance(q.edges[i], at).abs() < 1e-3);
                assert!(distance(q.edges[(i + len - 1) % len], at).abs() < 1e-3);
                assert!(distance(q.edges[i], centre) > 0.0);
                // The pushed vertex is a pixel outside both, and still in the plane: its
                // w and texture coordinate are those of the local point it shows.
                assert!((distance(q.edges[i], pushed.canvas) + 1.0).abs() < 1e-2);
                assert!((distance(q.edges[(i + len - 1) % len], pushed.canvas) + 1.0).abs() < 1e-2);
                let local = geometry
                    .unplace(jstream_protocol::CanvasPoint {
                        x: pushed.canvas[0],
                        y: pushed.canvas[1],
                    })
                    .expect("ok")
                    .expect("in front");
                let again = geometry
                    .drawn_vertex(&content, local)
                    .expect("ok")
                    .expect("in front");
                assert!((again.w - pushed.w).abs() < 1e-4, "{again:?} vs {pushed:?}");
                assert!((again.uv[0] - pushed.uv[0]).abs() < 1e-3);
                assert!((again.uv[1] - pushed.uv[1]).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn a_3d_item_reaching_behind_the_viewer_draws_its_front_part() {
        // Tilted toward the viewer with its top past the near plane.
        let geometry = in_3d(-70.0, 0.0, 10.0, Some(12.0));
        let q = shape(4, 2, Placement::Geometry(geometry), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert!((3..=5).contains(&q.vertices().len()));
        assert!(q.vertices().iter().all(|v| v.w >= jstream_protocol::NEAR_W));
    }

    #[test]
    fn a_3d_item_edge_on_or_behind_the_viewer_draws_nothing() {
        for geometry in [
            in_3d(0.0, 0.0, 500.0, Some(100.0)),
            in_3d(90.0, 0.0, 0.0, None),
        ] {
            assert!(
                shape(4, 2, Placement::Geometry(geometry), Crop::NONE)
                    .expect("finite")
                    .is_none()
            );
        }
    }

    /// A 3D item's mip region is its crop's texels exactly, at sizes where `k / W * W`
    /// isn't `k` in `f32`: 1920 × 1080 cropped to columns 63..241 and rows 7..69 is
    /// (63, 7, 178, 62), not a texel wider on each side (#687 round 1 B1). Every whole
    /// left and top edge at 1920 × 1080 comes out as itself. A 2D item has no region.
    #[test]
    fn a_3d_items_region_is_its_crops_texels() {
        let geometry = Placement::Geometry(in_3d(30.0, 0.0, 0.0, None));
        let region = |crop: Crop| {
            shape(1920, 1080, geometry, crop)
                .expect("finite")
                .expect("draws")
                .crop
                .expect("a 3D item has a region")
        };
        let crop = Crop {
            left: 63,
            top: 7,
            right: 1920 - 241,
            bottom: 1080 - 69,
        };
        assert_eq!(
            region(crop),
            Region {
                x: 63,
                y: 7,
                width: 178,
                height: 62
            }
        );
        for edge in 0..1080 {
            let crop = Crop {
                left: edge,
                top: edge,
                right: 1,
                bottom: 0,
            };
            let got = region(crop);
            assert_eq!((got.x, got.y), (edge, edge));
            assert_eq!((got.width, got.height), (1919 - edge, 1080 - edge));
        }
        assert_eq!(region(Crop::NONE), Region::whole(1920, 1080));
        let flat = shape(1920, 1080, boxed(20.0, 20.0, FitMode::Fit, 0.0), crop)
            .expect("finite")
            .expect("draws");
        assert_eq!(flat.crop, None);
    }
}
