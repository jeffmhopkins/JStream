//! Scene cameras (ADR-0094, docs/adr/0094-scene-cameras.md): one 3D space per scene,
//! seen through one camera.
//!
//! The space is the canvas extended toward the viewer: canvas pixels, x right, y down,
//! z toward the viewer, with the canvas at `z = 0`. An item's [`ItemGeometry`] is its pose
//! there: `(center.x, center.y, depth_px)` and its three angles. A [`CameraView`] turns
//! that pose into ADR-0050's homogeneous plane map with `w = D / f`, so the drawn
//! polygon, the near plane, `unplace` and edge coverage work unchanged
//! ([`ItemGeometry::drawn_polygon_seen`]). A camera may also have a depth of field
//! (ADR-0108, docs/adr/0108-camera-depth-of-field.md): [`CameraView::blur_at`] is the
//! blur at a distance along its view.

use super::drawn::MAX_DRAWN_PX;
use super::{Canvas, CanvasPoint, GeometryError, ItemGeometry, Placed, ProjectedQuad};
use crate::finite::Finite32;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The smallest field of view a camera may have, in degrees.
pub const MIN_FOV_DEG: f32 = 1.0;
/// The largest field of view a camera may have, in degrees.
pub const MAX_FOV_DEG: f32 = 170.0;
/// A camera's field of view when none is given, in degrees.
pub const DEFAULT_FOV_DEG: f32 = 45.0;
/// How close a camera's position may come to its target, in scene pixels.
pub const MIN_CAMERA_DISTANCE: f32 = 0.001;
/// The largest `blur_px` a camera may have, and the largest blur radius anything is
/// drawn with, in canvas pixels (ADR-0108).
pub const MAX_BLUR_PX: f32 = 64.0;

/// Within this angle of straight up or down, a camera's picture takes world `−z` as up
/// looking down and `+z` looking up: the up a camera facing `−z` has as it tilts to that
/// pole, so such a camera's picture doesn't turn over there (ADR-0111, amending ADR-0094
/// decision 3; #1047). A camera facing another way still turns over at a pole.
const UP_FALLBACK_DEG: f64 = 0.01;

/// A point or a direction in a scene's 3D space, in scene pixels: x right, y down, z
/// toward the viewer, the canvas at `z = 0`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vec3 {
    /// Right.
    pub x: Finite32,
    /// Down.
    pub y: Finite32,
    /// Toward the viewer.
    pub z: Finite32,
}

impl Vec3 {
    /// A vector from three finite components, or `None` if any is NaN or infinite.
    pub const fn new(x: f32, y: f32, z: f32) -> Option<Vec3> {
        match (Finite32::new(x), Finite32::new(y), Finite32::new(z)) {
            (Some(x), Some(y), Some(z)) => Some(Vec3 { x, y, z }),
            _ => None,
        }
    }

    fn get(self) -> [f64; 3] {
        [
            f64::from(self.x.get()),
            f64::from(self.y.get()),
            f64::from(self.z.get()),
        ]
    }
}

fn default_fov() -> Finite32 {
    Finite32::new(DEFAULT_FOV_DEG).unwrap_or(Finite32::ONE)
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero(value: &Finite32) -> bool {
    value.get() == 0.0
}

/// A scene's camera (ADR-0094): where it is, what it looks at, and how wide it sees.
///
/// A scene with a camera is a **3D scene**: every item in it is placed in the scene's
/// space by its geometry and seen through the camera, and its own `perspective_px` is
/// ignored. On the wire, omitted `fov_deg` is 45, omitted `roll_deg` 0, omitted
/// `depth_sort` `true` and omitted `blur_px` 0, neither of which is written, and omitted
/// `focus_px` no depth of field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneCamera {
    /// Where the camera is.
    pub position: Vec3,
    /// The point it looks at. At least 0.001 px from `position`.
    pub target: Vec3,
    /// Vertical field of view, in degrees, from 1 to 170. Default 45.
    #[serde(default = "default_fov")]
    pub fov_deg: Finite32,
    /// Rotation of the picture about the viewing axis, clockwise, in degrees. Default 0.
    #[serde(default)]
    pub roll_deg: Finite32,
    /// Whether items draw far to near, by the depth of their box centre along the
    /// viewing axis, with stacking order breaking ties. `false` keeps stacking order.
    /// Default `true`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub depth_sort: bool,
    /// The distance along the viewing axis, from the camera, of the plane in focus, in
    /// scene pixels: above 0 and at most the drawable distance. Absent, everything is in
    /// focus (ADR-0108).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_px: Option<Finite32>,
    /// How soft focus gets: the blur radius, in canvas pixels, of a point infinitely far
    /// behind the focus plane, from 0 to 64. With `focus_px`, the camera has a depth of
    /// field when this is above 0. Default 0 (ADR-0108).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blur_px: Finite32,
}

impl SceneCamera {
    /// The framing camera for `canvas` (ADR-0094 decision 3): 45°, looking at the canvas
    /// centre on `z = 0` from straight in front, at the distance where an item with
    /// neutral 3D lands exactly on its 2D box.
    pub fn framing(canvas: Canvas) -> Self {
        let fov = default_fov();
        let width = canvas.width as f32;
        let height = canvas.height as f32;
        let focal = focal_length(height, fov.get());
        let finite = |value: f32| Finite32::new(value).unwrap_or(Finite32::ZERO);
        let at = |z: f32| Vec3 {
            x: finite(width * 0.5),
            y: finite(height * 0.5),
            z: finite(z),
        };
        Self {
            position: at(focal as f32),
            target: at(0.0),
            fov_deg: fov,
            roll_deg: Finite32::ZERO,
            depth_sort: true,
            focus_px: None,
            blur_px: Finite32::ZERO,
        }
    }

    /// The distance from the camera to its target: where Depth of field starts focused.
    pub fn target_distance(self) -> f32 {
        length(sub(self.target.get(), self.position.get())) as f32
    }

    /// Whether an edit may store this camera.
    ///
    /// # Errors
    /// [`GeometryError::FieldOfView`] for a field of view outside 1 to 170 degrees,
    /// [`GeometryError::TooFar`] for a coordinate past [`MAX_DRAWN_PX`], and
    /// [`GeometryError::CameraAtTarget`] when `position` is closer than 0.001 px to
    /// `target`, [`GeometryError::FocusDistance`] for a `focus_px` not above 0 or past
    /// [`MAX_DRAWN_PX`], and [`GeometryError::Blur`] for a `blur_px` outside 0 to 64.
    pub fn validate(self) -> Result<(), GeometryError> {
        let fov = self.fov_deg.get();
        if !(MIN_FOV_DEG..=MAX_FOV_DEG).contains(&fov) {
            return Err(GeometryError::FieldOfView);
        }
        if let Some(focus) = self.focus_px
            && !(focus.get() > 0.0 && focus.get() <= MAX_DRAWN_PX)
        {
            return Err(GeometryError::FocusDistance);
        }
        if !(0.0..=MAX_BLUR_PX).contains(&self.blur_px.get()) {
            return Err(GeometryError::Blur);
        }
        let near = |v: f64| v.abs() <= f64::from(MAX_DRAWN_PX);
        if !self.position.get().into_iter().all(near) || !self.target.get().into_iter().all(near) {
            return Err(GeometryError::TooFar);
        }
        let forward = sub(self.target.get(), self.position.get());
        if length(forward) < f64::from(MIN_CAMERA_DISTANCE) {
            return Err(GeometryError::CameraAtTarget);
        }
        Ok(())
    }

    /// The camera's view of a canvas: its basis and focal length, which the shared
    /// placement functions take ([`ItemGeometry::drawn_polygon_seen`]).
    ///
    /// # Errors
    /// The same as [`Self::validate`].
    pub fn view(self, canvas: Canvas) -> Result<CameraView, GeometryError> {
        self.validate()?;
        let position = self.position.get();
        let forward = normalize(sub(self.target.get(), position));
        let up = if forward[1].abs() > UP_FALLBACK_DEG.to_radians().cos() {
            [0.0, 0.0, -forward[1].signum()]
        } else {
            [0.0, -1.0, 0.0]
        };
        let right = normalize(cross(up, forward));
        let down = cross(right, forward);
        let (sin, cos) = f64::from(self.roll_deg.get()).to_radians().sin_cos();
        let rolled_right = [0, 1, 2].map(|i| right[i] * cos - down[i] * sin);
        let rolled_down = [0, 1, 2].map(|i| right[i] * sin + down[i] * cos);
        let height = canvas.height as f32;
        Ok(CameraView {
            position,
            right: rolled_right,
            down: rolled_down,
            forward,
            focal: focal_length(height, self.fov_deg.get()),
            principal: [canvas.width as f32 * 0.5, height * 0.5],
            depth_sort: self.depth_sort,
            focus: match self.focus_px {
                Some(focus) if self.blur_px.get() > 0.0 => Some(Focus {
                    distance: focus.get(),
                    blur: self.blur_px.get(),
                }),
                _ => None,
            },
        })
    }
}

/// `(height / 2) / tan(fov / 2)`, in pixels.
fn focal_length(height: f32, fov_deg: f32) -> f64 {
    f64::from(height) * 0.5 / (f64::from(fov_deg).to_radians() * 0.5).tan()
}

/// A [`SceneCamera`] seen on one canvas: its orthonormal basis, focal length and the
/// canvas centre its axis lands on. Built by [`SceneCamera::view`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    pub(super) position: [f64; 3],
    pub(super) right: [f64; 3],
    pub(super) down: [f64; 3],
    pub(super) forward: [f64; 3],
    pub(super) focal: f64,
    pub(super) principal: [f32; 2],
    depth_sort: bool,
    focus: Option<Focus>,
}

/// A camera's depth of field (ADR-0108): the distance in focus and the blur far away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    /// The focus plane's distance along the viewing axis, in scene pixels.
    pub distance: f32,
    /// The blur radius far behind it, in canvas pixels.
    pub blur: f32,
}

impl CameraView {
    /// The focal length in canvas pixels: `(canvas height / 2) / tan(fov / 2)`.
    pub fn focal_px(&self) -> f32 {
        self.focal as f32
    }

    /// Whether the scene's items draw far to near ([`SceneCamera::depth_sort`]).
    pub fn depth_sort(&self) -> bool {
        self.depth_sort
    }

    /// The camera's depth of field, or `None` when everything is in focus.
    pub fn focus(&self) -> Option<Focus> {
        self.focus
    }

    /// The blur radius, in canvas pixels, of a point `distance` along the viewing axis
    /// (ADR-0108 decision 2): `blur · |distance − focus| / distance`, at most
    /// [`MAX_BLUR_PX`]; 0 with no depth of field, and [`MAX_BLUR_PX`] at or behind the
    /// camera.
    pub fn blur_at(&self, distance: f64) -> f32 {
        let Some(focus) = self.focus else {
            return 0.0;
        };
        if distance <= 0.0 {
            return MAX_BLUR_PX;
        }
        let r = f64::from(focus.blur) * (distance - f64::from(focus.distance)).abs() / distance;
        (r as f32).min(MAX_BLUR_PX)
    }

    /// How far in front of the camera an item's box centre is, along the viewing axis,
    /// in scene pixels: what a 3D scene sorts by (ADR-0094 decision 4). Negative behind
    /// the camera.
    pub fn depth_of(&self, geometry: ItemGeometry) -> f64 {
        dot(sub(item_origin(geometry), self.position), self.forward)
    }

    /// How far in front of the camera the centre of the canvas, on `z = 0`, is: where a
    /// placement that isn't in the scene's space (a legacy 0.3 transform) sorts.
    pub fn canvas_depth(&self) -> f64 {
        let [x, y] = self.principal.map(f64::from);
        dot(sub([x, y, 0.0], self.position), self.forward)
    }

    /// Where the ray from the camera through the canvas point `at` meets the plane
    /// `z = z` of the scene's space, as that plane's `(x, y)`: what an editor drags an
    /// item at depth `z` to. `None` when the ray runs along the plane or meets it behind
    /// the camera.
    pub fn on_plane(&self, at: CanvasPoint, z: f32) -> Option<(f32, f32)> {
        let dx = f64::from(at.x) - f64::from(self.principal[0]);
        let dy = f64::from(at.y) - f64::from(self.principal[1]);
        let ray = [0, 1, 2]
            .map(|i| self.forward[i] * self.focal + self.right[i] * dx + self.down[i] * dy);
        if ray[2].abs() < 1e-9 {
            return None;
        }
        let t = (f64::from(z) - self.position[2]) / ray[2];
        if t.is_nan() || t <= 0.0 {
            return None;
        }
        let x = self.position[0] + ray[0] * t;
        let y = self.position[1] + ray[1] * t;
        (x.is_finite() && y.is_finite()).then_some((x as f32, y as f32))
    }

    /// Where a point of the scene's space lands on the canvas, or
    /// [`Placed::BehindViewer`] when it is not in front of the near plane.
    pub fn place(&self, point: Vec3) -> Placed {
        let rel = sub(point.get(), self.position);
        let distance = dot(rel, self.forward);
        if distance < self.focal * f64::from(super::drawn::NEAR_W) {
            return Placed::BehindViewer;
        }
        let scale = self.focal / distance;
        let x = f64::from(self.principal[0]) + dot(rel, self.right) * scale;
        let y = f64::from(self.principal[1]) + dot(rel, self.down) * scale;
        Placed::At(CanvasPoint {
            x: x as f32,
            y: y as f32,
        })
    }
}

/// The item's centre in the scene's space.
pub(super) fn item_origin(geometry: ItemGeometry) -> [f64; 3] {
    [
        f64::from(geometry.center.x.get()),
        f64::from(geometry.center.y.get()),
        f64::from(geometry.depth_px.get()),
    ]
}

impl ItemGeometry {
    /// The box's projected corners through `view`, or as [`Self::project`] gives them
    /// with no view. A corner in front of the camera but behind its near plane is
    /// [`Placed::BehindViewer`].
    ///
    /// # Errors
    /// The same as [`Self::place`].
    pub fn project_seen(self, view: Option<&CameraView>) -> Result<ProjectedQuad, GeometryError> {
        let Some(view) = view else {
            return self.project();
        };
        self.validate()?;
        let plane = super::drawn::Plane::seen(self, view);
        let half_w = self.size.width.get() * 0.5;
        let half_h = self.size.height.get() * 0.5;
        let corner = |x: f32, y: f32| -> Result<Placed, GeometryError> {
            let w = plane.w(x, y);
            if w < super::drawn::NEAR_W {
                return Ok(Placed::BehindViewer);
            }
            let point = plane.canvas(x, y, w);
            if point.x.is_finite() && point.y.is_finite() {
                Ok(Placed::At(point))
            } else {
                Err(GeometryError::NotFinite)
            }
        };
        Ok(ProjectedQuad {
            corners: [
                corner(-half_w, -half_h)?,
                corner(half_w, -half_h)?,
                corner(half_w, half_h)?,
                corner(-half_w, half_h)?,
            ],
        })
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(super) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let len = length(a);
    [a[0] / len, a[1] / len, a[2] / len]
}

#[cfg(test)]
mod tests;
