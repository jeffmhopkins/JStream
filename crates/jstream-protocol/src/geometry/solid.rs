//! Solids: a 3D model placed by an item (ADR-0107 decision 3), in `f64` until the last
//! step, so a far camera or a large scene doesn't jitter.
//!
//! An item's geometry is the model's pose: the model's bounds are centred on the item's
//! centre and scaled by its fit mode, flipped by its flips, turned by its three angles and
//! moved to `depth_px`. In a 3D scene the result is seen through the scene's camera, as
//! [`super::drawn`] sees a box; in a 2D scene, through the item's own viewer: its
//! `perspective_px`, or orthographically. Engine and UI both draw and pick from [`Solid`].

use super::camera::{CameraView, dot, item_origin};
use super::drawn::NEAR_W;
use super::{Canvas, CanvasPoint, FitMode, GeometryError, ItemGeometry, Placed};

/// The box around a model, in its own space: x right, y down, z toward the viewer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolidBounds {
    /// The smallest x, y and z.
    pub min: [f32; 3],
    /// The largest x, y and z.
    pub max: [f32; 3],
}

/// Extents under this are flat: a plane has no depth, and fit ignores the axis.
const FLAT: f64 = 1e-9;

impl SolidBounds {
    fn size(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| f64::from(self.max[i]) - f64::from(self.min[i]))
    }

    fn center(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| (f64::from(self.min[i]) + f64::from(self.max[i])) * 0.5)
    }

    /// The scale along x, y and z that places these bounds in a box of `width` by
    /// `height` by `fit_mode`: `fit` the largest uniform scale that keeps width and height
    /// inside the box, `fill` the smallest that covers it, and `stretch` the box's
    /// width and height with z scaled by the smaller of the two. A flat extent takes the
    /// other's scale.
    pub fn scale(&self, width: f32, height: f32, fit_mode: FitMode) -> [f64; 3] {
        let [w, h, _] = self.size();
        let sx = (w > FLAT).then(|| f64::from(width) / w);
        let sy = (h > FLAT).then(|| f64::from(height) / h);
        let (sx, sy) = match (sx, sy) {
            (Some(x), Some(y)) => (x, y),
            (Some(x), None) => (x, x),
            (None, Some(y)) => (y, y),
            (None, None) => (1.0, 1.0),
        };
        match fit_mode {
            FitMode::Fit => [sx.min(sy); 3],
            FitMode::Fill => [sx.max(sy); 3],
            FitMode::Stretch => [sx, sy, sx.min(sy)],
        }
    }
}

/// A model placed by an item: the map from the model's space to the viewing frame, and
/// how that frame lands on the canvas. Built by [`ItemGeometry::solid`].
///
/// The viewing frame is the one ADR-0050's drawn polygon uses: x right and y down off the
/// point that lands on `center`, z toward the viewer, with the viewer at `z = p` and
/// `w = (p − z) / p`; without perspective, `w = 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solid {
    /// Rows of a 3x4 affine map from a model point to the viewing frame.
    map: [[f64; 4]; 3],
    perspective: Option<f64>,
    center: [f64; 2],
}

/// ADR-0022's rotations, in `f64`: about z by `rotation_deg`, then y by `turn_deg`, then
/// x by `tilt_deg` (CSS `rotateZ`, `rotateY`, `rotateX` in a y-down frame).
fn rotate(geometry: ItemGeometry, [x, y, z]: [f64; 3]) -> [f64; 3] {
    let (sin, cos) = f64::from(geometry.rotation_deg.get())
        .to_radians()
        .sin_cos();
    let (x, y) = (x * cos - y * sin, x * sin + y * cos);
    let (sin, cos) = f64::from(geometry.turn_deg.get()).to_radians().sin_cos();
    let (x, z) = (x * cos + z * sin, -x * sin + z * cos);
    let (sin, cos) = f64::from(geometry.tilt_deg.get()).to_radians().sin_cos();
    let (y, z) = (y * cos - z * sin, y * sin + z * cos);
    [x, y, z]
}

impl ItemGeometry {
    /// The model with `bounds` placed by this item and seen through `view` (a 3D scene),
    /// or by the item's own viewer without one (ADR-0107 decision 3).
    ///
    /// # Errors
    /// The same as [`Self::validate`], and [`GeometryError::NotFinite`] when the map is
    /// not finite (bounds that aren't).
    pub fn solid(
        self,
        bounds: SolidBounds,
        view: Option<&CameraView>,
    ) -> Result<Solid, GeometryError> {
        self.validate()?;
        let scale = bounds.scale(self.size.width.get(), self.size.height.get(), self.fit_mode);
        let flip = [
            if self.flip_h { -1.0 } else { 1.0 },
            if self.flip_v { -1.0 } else { 1.0 },
            1.0,
        ];
        let middle = bounds.center();
        // Each model axis, scaled and flipped, then turned into the scene (or the item's
        // own frame).
        let axis = |i: usize| {
            let mut unit = [0.0; 3];
            unit[i] = scale[i] * flip[i];
            rotate(self, unit)
        };
        let axes = [axis(0), axis(1), axis(2)];
        // Where the model's origin lands: the item's centre, less the bounds' middle.
        let shift = [0, 1, 2].map(|r| -(0..3).map(|c| axes[c][r] * middle[c]).sum::<f64>());
        let (axes, origin, perspective, center) = match view {
            Some(view) => {
                let to_view =
                    |v: [f64; 3]| [dot(v, view.right), dot(v, view.down), -dot(v, view.forward)];
                let item = item_origin(self);
                let rel = [0, 1, 2].map(|i| item[i] + shift[i] - view.position[i]);
                let seen = to_view(rel);
                (
                    axes.map(to_view),
                    [seen[0], seen[1], view.focal + seen[2]],
                    Some(view.focal),
                    view.principal.map(f64::from),
                )
            }
            None => (
                axes,
                [
                    shift[0],
                    shift[1],
                    shift[2] + f64::from(self.depth_px.get()),
                ],
                self.perspective_px.map(|p| f64::from(p.get())),
                [
                    f64::from(self.center.x.get()),
                    f64::from(self.center.y.get()),
                ],
            ),
        };
        let map = [0, 1, 2].map(|r| [axes[0][r], axes[1][r], axes[2][r], origin[r]]);
        if !map.iter().flatten().all(|v| v.is_finite()) {
            return Err(GeometryError::NotFinite);
        }
        Ok(Solid {
            map,
            perspective,
            center,
        })
    }
}

impl Solid {
    /// A model point in the viewing frame.
    pub fn in_view(&self, point: [f32; 3]) -> [f64; 3] {
        let p = point.map(f64::from);
        self.map
            .map(|row| row[0] * p[0] + row[1] * p[1] + row[2] * p[2] + row[3])
    }

    /// `w` of a viewing-frame point: at or under 0 it is at or behind the viewer.
    pub fn w(&self, seen: [f64; 3]) -> f64 {
        self.perspective.map_or(1.0, |p| (p - seen[2]) / p)
    }

    /// Whether it is seen with perspective (a camera, or the item's `perspective_px`).
    pub fn has_perspective(&self) -> bool {
        self.perspective.is_some()
    }

    /// The viewer's distance `p` in the viewing frame (the viewer is at `z = p`), or
    /// `None` without perspective.
    pub fn perspective_px(&self) -> Option<f64> {
        self.perspective
    }

    /// Where a model point lands on the canvas, or [`Placed::BehindViewer`] when it is
    /// behind ADR-0050's near plane, `w <` [`NEAR_W`].
    pub fn place(&self, point: [f32; 3]) -> Placed {
        let seen = self.in_view(point);
        let w = self.w(seen);
        if w < f64::from(NEAR_W) {
            return Placed::BehindViewer;
        }
        Placed::At(CanvasPoint {
            x: (self.center[0] + seen[0] / w) as f32,
            y: (self.center[1] + seen[1] / w) as f32,
        })
    }

    /// The eight corners of `bounds` on the canvas, in the order min/max x, then y, then
    /// z (bit 0 x, bit 1 y, bit 2 z): what an editor outlines and hits.
    pub fn corners(&self, bounds: SolidBounds) -> [Placed; 8] {
        std::array::from_fn(|i| {
            let pick = |bit: usize, axis: usize| {
                if i & (1 << bit) == 0 {
                    bounds.min[axis]
                } else {
                    bounds.max[axis]
                }
            };
            self.place([pick(0, 0), pick(1, 1), pick(2, 2)])
        })
    }

    /// Whether the map mirrors (its determinant is negative), which swaps which side of a
    /// triangle faces the viewer.
    pub fn mirrored(&self) -> bool {
        let m = &self.map;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        det < 0.0
    }

    /// The model-to-clip matrix for a `canvas`, column-major as WGSL's `mat4x4` takes it,
    /// for wgpu's clip space: x right and y up in `-w..w`, depth `z / w` in `0..1`.
    ///
    /// Depth is reversed, larger nearer. With perspective, `z = NEAR_W`, so `z / w` is
    /// `NEAR_W / w`: 1 on the near plane, falling to 0 at infinity, and the GPU's own clip
    /// at `z ≤ w` is ADR-0050's near plane. Without, depth is `0.5 + z · ortho_depth`,
    /// which the caller picks so the model's depth stays inside `0..1`.
    pub fn clip_matrix(&self, canvas: Canvas, ortho_depth: f64) -> [[f32; 4]; 4] {
        let (cw, ch) = (f64::from(canvas.width), f64::from(canvas.height));
        // Each clip coordinate as a row acting on (x, y, z, 1) of the viewing frame.
        let w_row = match self.perspective {
            Some(p) => [0.0, 0.0, -1.0 / p, 1.0],
            None => [0.0, 0.0, 0.0, 1.0],
        };
        let kx = 2.0 * self.center[0] / cw - 1.0;
        let ky = 1.0 - 2.0 * self.center[1] / ch;
        let x_row = [0, 1, 2, 3].map(|i| kx * w_row[i] + if i == 0 { 2.0 / cw } else { 0.0 });
        let y_row = [0, 1, 2, 3].map(|i| ky * w_row[i] - if i == 1 { 2.0 / ch } else { 0.0 });
        let z_row = match self.perspective {
            Some(_) => [0.0, 0.0, 0.0, f64::from(NEAR_W)],
            None => [0.0, 0.0, ortho_depth, 0.5],
        };
        let rows = [x_row, y_row, z_row, w_row];
        // Compose with the model map: clip = rows · [map; 0 0 0 1], all in f64.
        let full = rows.map(|row| {
            [0, 1, 2, 3].map(|c| {
                let mut sum = (0..3).map(|k| row[k] * self.map[k][c]).sum::<f64>();
                if c == 3 {
                    sum += row[3];
                }
                sum
            })
        });
        std::array::from_fn(|c| std::array::from_fn(|r| full[r][c] as f32))
    }

    /// The model-to-view map, column-major as WGSL's `mat4x4` takes it, for the viewing
    /// frame's positions a fragment lights in. Only the model's own extent and its
    /// offset from the viewer are in it, so it keeps `f32` precision near the viewer.
    pub fn view_matrix(&self) -> [[f32; 4]; 4] {
        std::array::from_fn(|c| {
            std::array::from_fn(|r| match r {
                3 => {
                    if c == 3 {
                        1.0
                    } else {
                        0.0
                    }
                }
                _ => self.map[r][c] as f32,
            })
        })
    }

    /// The matrix that takes a model normal into the viewing frame (the inverse
    /// transpose of the map's linear part), column-major, normalised so its largest
    /// column is about 1. A degenerate map gives the identity.
    pub fn normal_matrix(&self) -> [[f32; 3]; 3] {
        let m = &self.map;
        let a = |r: usize, c: usize| m[r][c];
        // Cofactors: the inverse transpose up to the determinant's scale and sign.
        let cof = [
            [
                a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1),
                a(1, 2) * a(2, 0) - a(1, 0) * a(2, 2),
                a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0),
            ],
            [
                a(0, 2) * a(2, 1) - a(0, 1) * a(2, 2),
                a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0),
                a(0, 1) * a(2, 0) - a(0, 0) * a(2, 1),
            ],
            [
                a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1),
                a(0, 2) * a(1, 0) - a(0, 0) * a(1, 2),
                a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0),
            ],
        ];
        let det = a(0, 0) * cof[0][0] + a(0, 1) * cof[0][1] + a(0, 2) * cof[0][2];
        let largest = cof.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
        if largest == 0.0 || !largest.is_finite() {
            return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        }
        // The sign of the determinant keeps normals pointing out of a mirrored model.
        let s = det.signum() / largest;
        // Row r of the inverse transpose is cof[r]; column-major output.
        std::array::from_fn(|c| std::array::from_fn(|r| (cof[r][c] * s) as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Vec2;
    use crate::finite::Finite32;
    use crate::geometry::{SceneCamera, Size};

    fn unit() -> SolidBounds {
        SolidBounds {
            min: [-0.5; 3],
            max: [0.5; 3],
        }
    }

    fn item(x: f32, y: f32, w: f32, h: f32) -> ItemGeometry {
        ItemGeometry::try_new(Vec2::new(x, y).unwrap(), Size::try_new(w, h).unwrap()).unwrap()
    }

    fn at(placed: Placed) -> (f32, f32) {
        match placed {
            Placed::At(p) => (p.x, p.y),
            Placed::BehindViewer => panic!("behind the viewer"),
        }
    }

    fn near(got: (f32, f32), want: (f32, f32)) {
        assert!(
            (got.0 - want.0).abs() < 1e-2 && (got.1 - want.1).abs() < 1e-2,
            "{got:?} != {want:?}"
        );
    }

    #[test]
    fn fit_fill_and_stretch_scale_the_bounds_into_the_box() {
        let wide = SolidBounds {
            min: [0.0, 0.0, 0.0],
            max: [2.0, 1.0, 4.0],
        };
        assert_eq!(wide.scale(100.0, 100.0, FitMode::Fit), [50.0; 3]);
        assert_eq!(wide.scale(100.0, 100.0, FitMode::Fill), [100.0; 3]);
        assert_eq!(
            wide.scale(100.0, 100.0, FitMode::Stretch),
            [50.0, 100.0, 50.0]
        );
        let flat = SolidBounds {
            min: [0.0, 0.0, 0.0],
            max: [2.0, 0.0, 0.0],
        };
        assert_eq!(flat.scale(100.0, 30.0, FitMode::Fit), [50.0; 3]);
    }

    #[test]
    fn in_a_2d_scene_the_front_of_a_cube_lands_on_the_box() {
        let geometry = item(300.0, 200.0, 100.0, 50.0);
        let solid = geometry.solid(unit(), None).unwrap();
        // Fit: the cube is 50 px each way, centred on the box.
        near(at(solid.place([-0.5, -0.5, 0.5])), (275.0, 175.0));
        near(at(solid.place([0.5, 0.5, 0.5])), (325.0, 225.0));
        // Orthographic: the back lands on the front.
        near(at(solid.place([0.5, 0.5, -0.5])), (325.0, 225.0));
        assert!(!solid.mirrored());
    }

    #[test]
    fn through_the_framing_camera_the_middle_plane_lands_on_the_box() {
        let canvas = Canvas::DEFAULT;
        let view = SceneCamera::framing(canvas).view(canvas).unwrap();
        let geometry = item(500.0, 300.0, 200.0, 200.0);
        let solid = geometry.solid(unit(), Some(&view)).unwrap();
        near(at(solid.place([-0.5, -0.5, 0.0])), (400.0, 200.0));
        near(at(solid.place([0.5, 0.5, 0.0])), (600.0, 400.0));
        // The front face is nearer, so bigger.
        let (left, _) = at(solid.place([-0.5, 0.5, 0.5]));
        let (right, _) = at(solid.place([0.5, 0.5, 0.5]));
        assert!(right - left > 200.5, "{left} {right}");
        assert!(solid.has_perspective());
    }

    #[test]
    fn a_flip_mirrors() {
        let mut geometry = item(0.0, 0.0, 10.0, 10.0);
        geometry.flip_h = true;
        let solid = geometry.solid(unit(), None).unwrap();
        assert!(solid.mirrored());
        near(at(solid.place([0.5, 0.0, 0.0])), (-5.0, 0.0));
    }

    #[test]
    fn the_clip_matrix_agrees_with_place() {
        let canvas = Canvas::DEFAULT;
        let camera = SceneCamera {
            position: crate::geometry::Vec3::new(700.0, 200.0, 900.0).unwrap(),
            target: crate::geometry::Vec3::new(960.0, 540.0, 0.0).unwrap(),
            ..SceneCamera::framing(canvas)
        };
        let view = camera.view(canvas).unwrap();
        let mut geometry = item(800.0, 500.0, 300.0, 200.0);
        geometry.turn_deg = Finite32::new(30.0).unwrap();
        geometry.depth_px = Finite32::new(-120.0).unwrap();
        let solid = geometry.solid(unit(), Some(&view)).unwrap();
        let m = solid.clip_matrix(canvas, 0.0);
        for point in [[0.5, -0.5, 0.5], [-0.5, 0.5, -0.5], [0.1, 0.2, 0.3]] {
            let p = [point[0], point[1], point[2], 1.0];
            let clip: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c][r] * p[c]).sum());
            let x = (clip[0] / clip[3] + 1.0) * 0.5 * canvas.width as f32;
            let y = (1.0 - clip[1] / clip[3]) * 0.5 * canvas.height as f32;
            near((x, y), at(solid.place(point)));
            let depth = clip[2] / clip[3];
            assert!(depth > 0.0 && depth < 1.0, "{depth}");
        }
    }

    #[test]
    fn a_far_camera_keeps_a_pixel_apart_through_the_f32_matrix() {
        // 10 million pixels away, where an f32 world position is only good to about a
        // pixel: two model points one scene pixel apart must still land apart, by what
        // the f64 placement says, after the matrix is cast to f32.
        let canvas = Canvas::DEFAULT;
        let geometry = item(960.0, 540.0, 400.0, 400.0);
        let camera = SceneCamera {
            position: crate::geometry::Vec3::new(960.0, 540.0, 1.0e7).unwrap(),
            fov_deg: Finite32::new(1.0).unwrap(),
            ..SceneCamera::framing(canvas)
        };
        let view = camera.view(canvas).unwrap();
        let solid = geometry.solid(unit(), Some(&view)).unwrap();
        let one_px = 1.0 / 400.0;
        let a = at(solid.place([0.0, 0.0, 0.0]));
        let b = at(solid.place([one_px, 0.0, 0.0]));
        near(a, (960.0, 540.0));
        let want = f64::from(b.0) - f64::from(a.0);
        assert!(want > 0.0);
        let m = solid.clip_matrix(canvas, 0.0);
        let x = |px: f32| {
            let clip: [f32; 4] = std::array::from_fn(|r| m[0][r] * px + m[3][r]);
            f64::from((clip[0] / clip[3] + 1.0) * 0.5 * canvas.width as f32)
        };
        let got = x(one_px) - x(0.0);
        assert!((got - want).abs() < want * 0.1, "{got} against {want}");
    }

    #[test]
    fn normals_stay_perpendicular_under_stretch() {
        let geometry = item(0.0, 0.0, 400.0, 100.0);
        let mut geometry = geometry;
        geometry.fit_mode = FitMode::Stretch;
        let solid = geometry.solid(unit(), None).unwrap();
        let n = solid.normal_matrix();
        // A diagonal face's normal (1, 1, 0) leans toward y after a 4:1 stretch.
        let out = [0, 1, 2].map(|r| n[0][r] + n[1][r]);
        assert!(out[1] > out[0] * 3.9, "{out:?}");
    }
}
