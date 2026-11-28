//! The canvas in a 3D scene (ADR-0094): its boxes drawn and hit through the scene's
//! camera, a drag that moves an item in its own depth plane, and the orbit and dolly
//! drags that move the camera.
//!
//! The engine draws the picture under the boxes. This module only has to agree with
//! it, and it does so through the same `jstream-protocol` functions the compositor
//! uses (`drawn_polygon_seen`, clipped to the near plane as the compositor clips it,
//! `CameraView::depth_of` and `CameraView::on_plane`). An item that shows a 3D model is
//! outlined and hit by the model's bounds instead (ADR-0107 decision 8): its box extruded
//! by the model's depth, through `ItemGeometry::solid_outline`. A 3D group (ADR-0164
//! decision 6) is outlined by the hull of its children as they are placed through the
//! group item's pose (`GroupPose`), hit inside any child's outline, and dragged by its
//! own item's geometry. The item under the pointer shows each model's bounds as a box in
//! the scene, its twelve edges through the camera (`bounds_edges`), rather than that
//! outline.
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0094-scene-cameras.md,
//! docs/adr/0111-camera-up-at-the-poles.md, docs/adr/0164-3d-groups-share-their-scenes-space.md

use crate::canvas::transform::Point;
use jstream_control::protocol::{
    CameraView, Canvas, CanvasPoint, ContentRect, Finite32, GroupPose, ItemGeometry, MAX_FOV_DEG,
    MIN_FOV_DEG, Placed, SceneCamera, SceneId, Size, SolidBounds, Vec2, Vec3,
};

/// The camera a move of a 3D scene's camera shows while it is unsaved ([`Unsaved`]), and
/// after Save until the collection has it, kept with the scene it moves, so another scene
/// is never seen through it.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// The scene the camera is that scene's.
    pub scene: SceneId,
    /// The camera shown.
    pub camera: SceneCamera,
}

impl Preview {
    /// The camera `scene` is seen through: `preview`'s when it moves that scene, otherwise
    /// the `stored` one.
    pub fn camera_in(
        preview: Option<&Preview>,
        scene: Option<&SceneId>,
        stored: Option<SceneCamera>,
    ) -> Option<SceneCamera> {
        match preview {
            Some(p) if scene == Some(&p.scene) => Some(p.camera),
            _ => stored,
        }
    }

    /// Whether to stop showing it now that the canvas shows `scene` with `stored`: the
    /// collection has its camera, or the scene's camera was removed, or another scene
    /// (or none) is shown.
    pub fn caught_up(&self, shown: Option<(Option<&SceneId>, Option<SceneCamera>)>) -> bool {
        match shown {
            Some((Some(scene), stored)) if *scene == self.scene => {
                stored.is_none_or(|c| c == self.camera)
            }
            _ => true,
        }
    }
}

/// A 3D scene's camera moved on the canvas and not saved yet (#1809). Moving the camera
/// is a look around, not an edit: the canvas shows the moved camera (a [`Preview`]) and
/// the engine draws it through one live gesture held open, until Save stores it as the
/// scene's camera (on a keyframe scene, its override) or the canvas drops it, and the
/// saved camera shows again.
#[derive(Clone, Debug, PartialEq)]
pub struct Unsaved {
    /// The scene whose camera moved.
    pub scene: SceneId,
    /// The scene's stored camera when the first move started.
    pub base: Option<SceneCamera>,
}

impl Unsaved {
    /// Whether to drop it now that the canvas shows `shown`, a scene with its stored
    /// camera: another scene (or none) is shown, so leaving a scene puts its saved camera
    /// back; or the scene's stored camera is no longer `base`, as when Properties, an undo
    /// or another client set it, which wins over a move nobody saved.
    pub fn stale(&self, shown: Option<(Option<&SceneId>, Option<SceneCamera>)>) -> bool {
        match shown {
            Some((Some(scene), stored)) if *scene == self.scene => stored != self.base,
            _ => true,
        }
    }

    /// What Save stores: the camera `preview` shows in this scene, when it differs from
    /// the one stored. `None` has nothing to save.
    pub fn to_save(&self, preview: Option<&Preview>) -> Option<SceneCamera> {
        Preview::camera_in(preview, Some(&self.scene), None).filter(|c| Some(*c) != self.base)
    }

    /// Whether a drag cancelled with Esc, back at `start`, leaves nothing unsaved: it
    /// started from the stored camera, so the move is dropped rather than shown.
    pub fn back_to_base(&self, start: SceneCamera) -> bool {
        self.base == Some(start)
    }
}

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

/// What the canvas outlines and hits an item by in a 3D scene, besides its own box.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Shape {
    /// Its box, as the compositor draws it.
    #[default]
    Box,
    /// The bounds of the 3D model it shows (ADR-0107 decision 8).
    Model(SolidBounds),
    /// A 3D group's children (ADR-0164 decisions 2 and 6): each child's geometry in the
    /// group's own pixels with its own shape, placed through the group item's pose on a
    /// group canvas of `canvas`. Never empty: a group with nothing drawn is a [`Shape::Box`].
    Group {
        /// The group's own canvas, which its fit mode scales into the item's box.
        canvas: Canvas,
        /// Its drawn children, in stacking order.
        parts: Vec<(ItemGeometry, Shape)>,
    },
}

/// The outline of one box or model as the camera shows it: see [`polygon`]. A model
/// whose bounds reach past the near plane falls back to its card box (#1601).
fn piece(
    geometry: &ItemGeometry,
    solid: Option<SolidBounds>,
    view: &CameraView,
) -> Option<Vec<Point>> {
    if let Some(hull) = solid.and_then(|bounds| geometry.solid_outline(bounds, Some(view))) {
        return Some(
            hull.into_iter()
                .map(|at| Point { x: at.x, y: at.y })
                .collect(),
        );
    }
    let width = geometry.size.width.get();
    let height = geometry.size.height.get();
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
    let polygon = geometry.drawn_polygon_seen(Some(view), &whole_box).ok()??;
    let points: Vec<Point> = polygon
        .vertices()
        .iter()
        .map(|v| Point {
            x: v.canvas.x,
            y: v.canvas.y,
        })
        .collect();
    (points.len() >= 3 && points.iter().all(|p| p.x.is_finite() && p.y.is_finite()))
        .then_some(points)
}

/// Calls `each` with every box or model the item draws, in the scene's space: the item
/// itself, or a 3D group's children placed through its pose, nested groups in turn. A
/// child whose placement overflows is left out, as the compositor leaves it out.
fn each_piece(
    geometry: &ItemGeometry,
    shape: &Shape,
    each: &mut impl FnMut(&ItemGeometry, Option<SolidBounds>),
) {
    match shape {
        Shape::Box => each(geometry, None),
        Shape::Model(bounds) => each(geometry, Some(*bounds)),
        Shape::Group { canvas, parts } => {
            let Ok(pose) = GroupPose::of(*geometry, *canvas) else {
                return;
            };
            for (child, shape) in parts {
                if let Ok(placed) = pose.place(*child) {
                    each_piece(&placed, shape, each);
                }
            }
        }
    }
}

/// Whether `point` is inside a convex `outline`, either winding (a box seen from behind
/// is mirrored).
fn inside(outline: &[Point], point: Point) -> bool {
    let len = outline.len();
    let side = |a: Point, b: Point| (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    let sides: Vec<f32> = (0..len)
        .map(|i| side(outline[i], outline[(i + 1) % len]))
        .collect();
    sides.iter().all(|s| *s >= 0.0) || sides.iter().all(|s| *s <= 0.0)
}

/// The convex hull of `points` by Andrew's monotone chain, with no collinear points.
fn hull(mut points: Vec<Point>) -> Vec<Point> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: Point, a: Point, b: Point| {
        (f64::from(a.x) - f64::from(o.x)) * (f64::from(b.y) - f64::from(o.y))
            - (f64::from(a.y) - f64::from(o.y)) * (f64::from(b.x) - f64::from(o.x))
    };
    let mut out: Vec<Point> = Vec::with_capacity(points.len() * 2);
    for pass in [false, true] {
        let start = out.len();
        let ordered: Vec<Point> = if pass {
            points.iter().rev().copied().collect()
        } else {
            points.clone()
        };
        for p in ordered {
            while out.len() >= start + 2 && cross(out[out.len() - 2], out[out.len() - 1], p) <= 0.0
            {
                out.pop();
            }
            out.push(p);
        }
        out.pop();
    }
    out
}

/// The part of the box in front of the camera, as the compositor draws it: the whole box
/// clipped to the near plane, three to five corners on the canvas (#1054). `None` when
/// none of it is in front of the camera or it is seen edge-on. Convex, as a clipped
/// rectangle is.
///
/// For an item that shows a model ([`Shape::Model`]), the outline of the model's bounds
/// as the item places them: the hull of their eight corners, `None` when one is behind
/// the camera. For a 3D group ([`Shape::Group`]), the hull of its children's outlines as
/// they are placed through the group item's pose (ADR-0164 decision 6), `None` when none
/// of them is seen.
pub fn polygon(geometry: &ItemGeometry, shape: &Shape, view: &CameraView) -> Option<Vec<Point>> {
    if !matches!(shape, Shape::Group { .. }) {
        let solid = match shape {
            Shape::Model(bounds) => Some(*bounds),
            _ => None,
        };
        return piece(geometry, solid, view);
    }
    let mut points = Vec::new();
    each_piece(geometry, shape, &mut |g, solid| {
        points.extend(piece(g, solid, view).into_iter().flatten());
    });
    let outline = hull(points);
    (outline.len() >= 3).then_some(outline)
}

/// How far along the viewing axis the item is where `point` hits it, or `None` when it
/// misses: a box or model by its outline at its own depth, a 3D group by whichever of its
/// children's outlines is nearest there, since each child is sorted on its own
/// (ADR-0164 decision 3), never by the gaps between them. With depth sorting off every
/// hit is at depth 0.
fn hit_depth(
    geometry: &ItemGeometry,
    shape: &Shape,
    view: &CameraView,
    point: Point,
) -> Option<f64> {
    let mut best: Option<f64> = None;
    each_piece(geometry, shape, &mut |g, solid| {
        if piece(g, solid, view).is_some_and(|outline| inside(&outline, point)) {
            let depth = if view.depth_sort() {
                view.depth_of(*g)
            } else {
                0.0
            };
            best = Some(best.map_or(depth, |near| near.min(depth)));
        }
    });
    best
}

/// Whether `point` is inside the box as the camera shows it: inside the part of it in
/// front of the camera, either winding (a box seen from behind is mirrored). A 3D group is
/// hit inside any of its children's outlines.
pub fn contains(geometry: &ItemGeometry, shape: &Shape, view: &CameraView, point: Point) -> bool {
    hit_depth(geometry, shape, view, point).is_some()
}

/// The box under `point` that the engine draws on top: the nearest along the viewing
/// axis when the camera sorts by depth, ties and an unsorted camera going to the later
/// in stacking order, as the compositor draws them.
pub fn topmost_at<'a, T>(
    items: impl DoubleEndedIterator<Item = (T, &'a ItemGeometry, &'a Shape)>,
    view: &CameraView,
    point: Point,
) -> Option<T> {
    let mut best: Option<(f64, T)> = None;
    for (id, geometry, shape) in items {
        let Some(depth) = hit_depth(geometry, shape, view, point) else {
            continue;
        };
        // Later in stacking order wins a tie, so `<=`.
        if best.as_ref().is_none_or(|(near, _)| depth <= *near) {
            best = Some((depth, id));
        }
    }
    best.map(|(_, id)| id)
}

/// The twelve edges of `drawn`, a box in the space of a model whose bounds are
/// `bounds`, as `view` sees that model placed at `geometry`: pairs
/// of corners of [`jstream_control::protocol::Solid::corners`] that differ in one axis.
/// `None` when a corner is behind the camera or isn't finite.
fn box_edges(
    geometry: &ItemGeometry,
    (bounds, drawn): (SolidBounds, SolidBounds),
    view: &CameraView,
) -> Option<[(Point, Point); 12]> {
    let solid = geometry.solid(bounds, Some(view)).ok()?;
    let mut corners = [Point { x: 0.0, y: 0.0 }; 8];
    for (corner, placed) in corners.iter_mut().zip(solid.corners(drawn)) {
        match placed {
            Placed::At(at) if at.x.is_finite() && at.y.is_finite() => {
                *corner = Point { x: at.x, y: at.y };
            }
            _ => return None,
        }
    }
    // Corner `i` takes x from bit 0, y from bit 1 and z from bit 2. An edge joins two
    // corners one bit apart: four along each axis.
    let mut edges = [(corners[0], corners[0]); 12];
    let mut at = 0;
    for bit in [1, 2, 4] {
        for from in (0..8).filter(|i| i & bit == 0) {
            edges[at] = (corners[from], corners[from | bit]);
            at += 1;
        }
    }
    Some(edges)
}

/// The edges of every model's bounds an item draws, as a 3D box through the camera
/// (ADR-0107 decision 8), as SVG path data: one box for a model item, one for each model
/// a 3D group places. `None` when the item draws no model, or a model's box reaches
/// behind the camera, so the caller outlines it as before.
pub fn bounds_edges(geometry: &ItemGeometry, shape: &Shape, view: &CameraView) -> Option<String> {
    let mut path = String::new();
    let mut whole = true;
    each_piece(geometry, shape, &mut |g, solid| {
        let Some(bounds) = solid else {
            return;
        };
        match box_edges(g, (bounds, bounds), view) {
            Some(edges) => {
                for (a, b) in edges {
                    path.push_str(&format!("M{},{} L{},{} ", a.x, a.y, b.x, b.y));
                }
            }
            None => whole = false,
        }
    });
    (whole && !path.is_empty()).then_some(path)
}

/// The box of one object of a model item (protocol 0.198), as SVG path data like
/// [`bounds_edges`]: `min` and `max` are fractions of the model's `bounds` on each axis,
/// as `source.sizes` lists them. `None` when the item draws no single model, or the box
/// reaches behind the camera.
pub fn object_edges(
    geometry: &ItemGeometry,
    shape: &Shape,
    (min, max): ([f32; 3], [f32; 3]),
    view: &CameraView,
) -> Option<String> {
    let Shape::Model(bounds) = shape else {
        return None;
    };
    let at = |t: f32, axis: usize| bounds.min[axis] + t * (bounds.max[axis] - bounds.min[axis]);
    let drawn = SolidBounds {
        min: [0, 1, 2].map(|axis| at(min[axis], axis)),
        max: [0, 1, 2].map(|axis| at(max[axis], axis)),
    };
    let edges = box_edges(geometry, (*bounds, drawn), view)?;
    Some(edges.iter().fold(String::new(), |mut path, (a, b)| {
        path.push_str(&format!("M{},{} L{},{} ", a.x, a.y, b.x, b.y));
        path
    }))
}

/// The outline of the part of the box in front of the camera as SVG `points`, or empty
/// when none of it is.
pub fn outline(geometry: &ItemGeometry, shape: &Shape, view: &CameraView) -> String {
    polygon(geometry, shape, view).map_or_else(String::new, |outline| {
        outline.iter().fold(String::new(), |mut points, at| {
            points.push_str(&format!("{},{} ", at.x, at.y));
            points
        })
    })
}

/// The upright box around the part of the box in front of the camera, for a marquee to
/// touch, or `None` when none of it is. A name whose top edge is in front of the camera
/// sits on that corner instead; one whose top edge is behind or edge-on uses these
/// bounds.
pub fn screen_box(
    geometry: &ItemGeometry,
    shape: &Shape,
    view: &CameraView,
) -> Option<ItemGeometry> {
    let quad = polygon(geometry, shape, view)?;
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
/// up to [`MAX_ELEVATION_DEG`], or no further than it started when a camera typed in
/// Properties is already past that. Its distance, field of view and roll stay.
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
    // Only the side a typed camera is already past widens; the other keeps the cap.
    let cap = MAX_ELEVATION_DEG.to_radians();
    let (below, above) = (cap.max(-elevation), cap.max(elevation));
    let elevation = (elevation + (dy * ORBIT_DEG_PER_PX).to_radians()).clamp(-below, above);
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

/// What a camera-mode drag does to the camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraDrag {
    /// Turns the camera about its target ([`orbit`]).
    Orbit,
    /// Turns the camera where it stands, moving what it looks at ([`look`]).
    Look,
    /// Slides the camera and its target across the picture ([`pan`]).
    Pan,
    /// Moves the camera along its line of sight ([`dolly`]).
    Dolly,
}

impl CameraDrag {
    /// What the drag is called on its toolbar button.
    pub fn label(self) -> &'static str {
        match self {
            CameraDrag::Orbit => "Orbit",
            CameraDrag::Look => "Look around",
            CameraDrag::Pan => "Move",
            CameraDrag::Dolly => "Dolly",
        }
    }
}

/// What the mouse wheel does to the camera in camera mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraWheel {
    /// Narrows or widens the field of view ([`zoom`]).
    Zoom,
    /// Moves the camera toward or away from its target ([`dolly`]).
    Dolly,
}

/// Camera mode's mouse bindings, kept per browser: what a left drag does,
/// and what the wheel does. A right or middle drag does [`Self::other`], Shift with a
/// left drag dollies, and Shift with the wheel does what the wheel otherwise doesn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraControls {
    /// What a left drag does.
    pub left: CameraDrag,
    /// What the wheel does.
    pub wheel: CameraWheel,
}

impl Default for CameraControls {
    fn default() -> Self {
        CameraControls {
            left: CameraDrag::Orbit,
            wheel: CameraWheel::Zoom,
        }
    }
}

impl CameraControls {
    /// What a right or middle drag does: moves the camera, or orbits it when a left drag
    /// already moves it.
    pub fn other(self) -> CameraDrag {
        match self.left {
            CameraDrag::Pan => CameraDrag::Orbit,
            _ => CameraDrag::Pan,
        }
    }

    /// What a drag with mouse `button` (0 left, 1 middle, 2 right) does, with Shift
    /// held or not; `None` for another button.
    pub fn drag(self, button: i16, shift: bool) -> Option<CameraDrag> {
        match button {
            0 if shift => Some(CameraDrag::Dolly),
            0 => Some(self.left),
            1 | 2 => Some(self.other()),
            _ => None,
        }
    }

    /// What the wheel does, with Shift held or not.
    pub fn wheel(self, shift: bool) -> CameraWheel {
        match (self.wheel, shift) {
            (CameraWheel::Zoom, false) | (CameraWheel::Dolly, true) => CameraWheel::Zoom,
            _ => CameraWheel::Dolly,
        }
    }

    /// The bindings stored as `text` (see [`Self::stored`]), or the defaults for what is
    /// absent or unreadable.
    pub fn from_stored(text: Option<&str>) -> CameraControls {
        let mut controls = CameraControls::default();
        let mut parts = text.unwrap_or_default().split(',');
        controls.left = match parts.next() {
            Some("look") => CameraDrag::Look,
            Some("pan") => CameraDrag::Pan,
            Some("dolly") => CameraDrag::Dolly,
            _ => CameraDrag::Orbit,
        };
        controls.wheel = match parts.next() {
            Some("dolly") => CameraWheel::Dolly,
            _ => CameraWheel::Zoom,
        };
        controls
    }

    /// The bindings as stored: what a left drag does, a comma, and what the wheel does.
    pub fn stored(self) -> String {
        let left = match self.left {
            CameraDrag::Look => "look",
            CameraDrag::Pan => "pan",
            CameraDrag::Dolly => "dolly",
            CameraDrag::Orbit => "orbit",
        };
        let wheel = match self.wheel {
            CameraWheel::Zoom => "zoom",
            CameraWheel::Dolly => "dolly",
        };
        format!("{left},{wheel}")
    }
}

/// `start` moved by a camera-mode drag of `kind`: `screen` is the drag in screen pixels,
/// which turns and dollies follow, and `canvas` the same drag in canvas pixels, which a
/// pan follows so the target's plane stays under the pointer.
pub fn drag(
    kind: CameraDrag,
    start: SceneCamera,
    screen: (f32, f32),
    canvas_delta: (f32, f32),
    canvas: Canvas,
) -> SceneCamera {
    match kind {
        CameraDrag::Orbit => orbit(start, screen.0, screen.1),
        CameraDrag::Look => look(start, screen.0, screen.1),
        CameraDrag::Pan => pan(start, canvas_delta.0, canvas_delta.1, canvas),
        CameraDrag::Dolly => dolly(start, screen.1),
    }
}

/// `start` turned where it stands by a drag of `dx`, `dy` screen pixels, as a head
/// turns: right looks right, down looks down, never past [`MAX_ELEVATION_DEG`] (or no
/// further than a typed camera already is). Its position, the distance to its target,
/// field of view and roll stay; the target moves.
pub fn look(start: SceneCamera, dx: f32, dy: f32) -> SceneCamera {
    let target = get(start.target);
    let position = get(start.position);
    let ahead = [0, 1, 2].map(|i| target[i] - position[i]);
    let distance = ahead.iter().map(|v| v * v).sum::<f32>().sqrt();
    if distance <= 0.0 {
        return start;
    }
    // Up is −y; the azimuth is measured about it from +z, toward +x.
    let elevation = (-ahead[1]).atan2(ahead[0].hypot(ahead[2]));
    let azimuth = ahead[0].atan2(ahead[2]);
    let cap = MAX_ELEVATION_DEG.to_radians();
    let (below, above) = (cap.max(-elevation), cap.max(elevation));
    let elevation = (elevation - (dy * ORBIT_DEG_PER_PX).to_radians()).clamp(-below, above);
    let azimuth = azimuth - (dx * ORBIT_DEG_PER_PX).to_radians();
    let moved = [
        position[0] + distance * elevation.cos() * azimuth.sin(),
        position[1] - distance * elevation.sin(),
        position[2] + distance * elevation.cos() * azimuth.cos(),
    ];
    match vec3(moved) {
        Some(target) => SceneCamera { target, ..start },
        None => start,
    }
}

/// `start` and its target slid across the picture by a drag of `dx`, `dy` canvas
/// pixels, so the scene at the target's distance follows the pointer, as a hand drags
/// a map. Its direction, field of view and roll stay.
pub fn pan(start: SceneCamera, dx: f32, dy: f32, canvas: Canvas) -> SceneCamera {
    let Ok(view) = start.view(canvas) else {
        return start;
    };
    let [right, down, _] = view.axes();
    let scale = f64::from(start.target_distance()) / f64::from(view.focal_px());
    let shift = [0, 1, 2]
        .map(|i| -(right[i] * f64::from(dx) + down[i] * f64::from(dy)) * scale)
        .map(|v| v as f32);
    let position = get(start.position);
    let target = get(start.target);
    match (
        vec3([0, 1, 2].map(|i| position[i] + shift[i])),
        vec3([0, 1, 2].map(|i| target[i] + shift[i])),
    ) {
        (Some(position), Some(target)) => SceneCamera {
            position,
            target,
            ..start
        },
        _ => start,
    }
}

/// How much one wheel pixel scales the field of view: `exp(px × ZOOM_PER_WHEEL_PX)`.
pub const ZOOM_PER_WHEEL_PX: f32 = 0.001;
/// How many dolly-drag pixels one wheel pixel is worth.
pub const DOLLY_PER_WHEEL_PX: f32 = 0.2;

/// `start` zoomed by a wheel turn of `delta` pixels (the browser's `deltaY`): down, or
/// back, widens the field of view, up narrows it, within the 1° to 170° a camera may
/// have. Where it is and what it looks at stay.
pub fn zoom(start: SceneCamera, delta: f32) -> SceneCamera {
    let fov =
        (start.fov_deg.get() * (delta * ZOOM_PER_WHEEL_PX).exp()).clamp(MIN_FOV_DEG, MAX_FOV_DEG);
    SceneCamera {
        fov_deg: finite(fov, start.fov_deg),
        ..start
    }
}

/// `start` changed by a wheel turn of `delta` pixels as `wheel` says.
pub fn wheel(wheel: CameraWheel, start: SceneCamera, delta: f32) -> SceneCamera {
    match wheel {
        CameraWheel::Zoom => zoom(start, delta),
        CameraWheel::Dolly => dolly(start, delta * DOLLY_PER_WHEEL_PX),
    }
}

/// How an item drag in a 3D scene changes the item, besides sliding it on its depth
/// plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pose {
    /// Moves it toward or away from the camera, along the line from the camera to it.
    Depth,
    /// Turns and tilts it about its centre.
    Turn,
}

/// Degrees an item turns or tilts per screen pixel of a Turn drag.
pub const TURN_DEG_PER_PX: f32 = 0.5;
/// Screen pixels of a Depth drag that double, or halve, an item's distance from the camera.
pub const DEPTH_DRAG_PX: f32 = 200.0;
/// The nearest a Depth drag brings an item to the camera, in scene pixels.
pub const MIN_ITEM_DISTANCE: f32 = 10.0;
/// The least distance a Depth drag scales by, in scene pixels, so an item at the camera
/// still moves.
const MIN_DEPTH_SCALE: f32 = 50.0;

/// `start` changed by a drag of `dx`, `dy` screen pixels as `pose` says. A Depth drag
/// up pushes the item away along the camera's line of sight to it, down brings it
/// closer: its distance from the camera doubles per [`DEPTH_DRAG_PX`] up and halves per
/// [`DEPTH_DRAG_PX`] down, no nearer than [`MIN_ITEM_DISTANCE`], so it stays where it is
/// on screen and never passes the camera. A Turn drag right turns its right
/// edge away (`turn_deg`), up tips its top edge away (`tilt_deg`), each kept within
/// ±180°. `None` when the result isn't a valid placement, or for a Turn of a
/// billboard, which faces the camera whatever its angles.
pub fn pose(
    pose: Pose,
    start: ItemGeometry,
    view: &CameraView,
    dx: f32,
    dy: f32,
) -> Option<ItemGeometry> {
    let mut moved = start;
    match pose {
        Pose::Depth => {
            // Along the ray from the camera through the item's centre, so it stays under
            // the pointer as it goes.
            let eye = view.position();
            let at = [
                f64::from(start.center.x.get()),
                f64::from(start.center.y.get()),
                f64::from(start.depth_px.get()),
            ];
            let ray = [0, 1, 2].map(|i| at[i] - eye[i]);
            let length = ray.iter().map(|v| v * v).sum::<f64>().sqrt();
            let (ray, length) = if length > f64::from(MIN_DEPTH_SCALE) {
                (ray.map(|v| v / length), length)
            } else {
                (view.axes()[2], f64::from(MIN_DEPTH_SCALE))
            };
            // Its distance doubles per DEPTH_DRAG_PX up and halves per DEPTH_DRAG_PX down,
            // so it never reaches the camera, let alone passes behind it.
            // No nearer than MIN_ITEM_DISTANCE, or than it started, so rounding never
            // puts it at or behind the camera.
            let floor = f64::from(MIN_ITEM_DISTANCE).min(length);
            let along = (length * f64::from(-dy / DEPTH_DRAG_PX).exp2()).max(floor) - length;
            let [x, y, z] = [0, 1, 2].map(|i| (at[i] + ray[i] * along) as f32);
            moved.center = Vec2::new(x, y)?;
            moved.depth_px = Finite32::new(z)?;
        }
        // A billboard faces the camera whatever its angles, so turning it would change
        // nothing seen.
        Pose::Turn if start.billboard => return None,
        Pose::Turn => {
            let wrap = |deg: f32| {
                let d = deg.rem_euclid(360.0);
                if d > 180.0 { d - 360.0 } else { d }
            };
            moved.turn_deg = Finite32::new(wrap(start.turn_deg.get() + dx * TURN_DEG_PER_PX))?;
            moved.tilt_deg = Finite32::new(wrap(start.tilt_deg.get() - dy * TURN_DEG_PER_PX))?;
        }
    }
    Some(moved)
}

/// The camera's numbers rounded for a readout: its position, its distance to the
/// target and its field of view.
pub fn readout(camera: SceneCamera) -> String {
    let [x, y, z] = get(camera.position);
    let target = get(camera.target);
    let distance = [x - target[0], y - target[1], z - target[2]]
        .iter()
        .map(|v| v * v)
        .sum::<f32>()
        .sqrt();
    format!(
        "Camera {:.0}, {:.0}, {:.0} · {:.0} px from target · {:.0}°",
        x,
        y,
        z,
        distance,
        camera.fov_deg.get()
    )
}

/// `value` as a finite coordinate, or `fallback` when it isn't one.
pub fn finite(value: f32, fallback: Finite32) -> Finite32 {
    Finite32::new(value).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{FitMode, Placed};

    /// The box's four corners through `view` by `project_seen`, or `None` when any is
    /// behind the camera or not finite: what `polygon` agrees with for a box wholly in
    /// front.
    fn corners(geometry: &ItemGeometry, view: &CameraView) -> Option<[Point; 4]> {
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
        assert!(contains(
            &g,
            &Shape::Box,
            &seen,
            Point { x: 650.0, y: 350.0 }
        ));
        assert!(!contains(
            &g,
            &Shape::Box,
            &seen,
            Point { x: 750.0, y: 350.0 }
        ));
    }

    #[test]
    fn the_nearer_box_is_hit_whatever_the_stacking_order() {
        let seen = framing();
        let near = item(960.0, 540.0, 300.0, 300.0, 200.0);
        let far = item(960.0, 540.0, 300.0, 300.0, -400.0);
        // The near box is first in stacking order, under the far one.
        let items = [("near", &near, &Shape::Box), ("far", &far, &Shape::Box)];
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

    fn elevation_of(c: SceneCamera) -> f32 {
        let p = get(c.position);
        let t = get(c.target);
        (t[1] - p[1])
            .atan2((p[0] - t[0]).hypot(p[2] - t[2]))
            .to_degrees()
    }

    #[test]
    fn an_orbit_from_past_the_cap_does_not_jump_to_it() {
        // A near top-down camera typed in Properties: 89° down on the target.
        let mut start = SceneCamera::framing(CANVAS);
        let t = get(start.target);
        let d = distance(start);
        let e = 89f32.to_radians();
        start.position = vec3([t[0], t[1] - d * e.sin(), t[2] + d * e.cos()]).expect("finite");
        assert!((elevation_of(start) - 89.0).abs() < 0.01);
        // Sideways, and further down, it stays where it was rather than snapping to 85°.
        assert!((elevation_of(orbit(start, 30.0, 0.0)) - 89.0).abs() < 0.01);
        assert!((elevation_of(orbit(start, 0.0, 100.0)) - 89.0).abs() < 0.01);
        // Back up, it comes down from there, and on the far side stops at the cap.
        let lowered = elevation_of(orbit(start, 0.0, -10.0));
        assert!((lowered - 86.0).abs() < 0.01, "{lowered}");
        let under = elevation_of(orbit(start, 0.0, -10_000.0));
        assert!((under + MAX_ELEVATION_DEG).abs() < 0.01, "{under}");
    }

    #[test]
    fn a_preview_is_shown_only_in_its_own_scene() {
        let a = SceneId::new("a");
        let b = SceneId::new("b");
        let stored = SceneCamera::framing(CANVAS);
        let moved = orbit(stored, 40.0, 0.0);
        let p = Preview {
            scene: a.clone(),
            camera: moved,
        };
        assert_eq!(
            Preview::camera_in(Some(&p), Some(&a), Some(stored)),
            Some(moved)
        );
        // Scene B, shown while A's commit is in flight, keeps its own camera.
        assert_eq!(
            Preview::camera_in(Some(&p), Some(&b), Some(stored)),
            Some(stored)
        );
        assert_eq!(Preview::camera_in(None, Some(&a), None), None);
        // Nor is a 2D scene shown mid-drag seen through it.
        assert_eq!(Preview::camera_in(Some(&p), Some(&b), None), None);
        // It is dropped once A has the camera, once A's camera is removed, or once
        // another scene is shown; A with its old camera still keeps it.
        assert!(p.caught_up(Some((Some(&a), Some(moved)))));
        assert!(p.caught_up(Some((Some(&a), None))));
        assert!(p.caught_up(Some((Some(&b), Some(stored)))));
        assert!(p.caught_up(None));
        assert!(!p.caught_up(Some((Some(&a), Some(stored)))));
    }

    #[test]
    fn a_moved_camera_is_saved_only_by_save_and_dropped_on_leaving_the_scene() {
        // #1809: a move of A's camera stays unsaved until Save, which stores the moved
        // camera, and leaving A, or A's camera changing elsewhere, drops it.
        let a = SceneId::new("a");
        let b = SceneId::new("b");
        let stored = SceneCamera::framing(CANVAS);
        let moved = orbit(stored, 40.0, 0.0);
        let unsaved = Unsaved {
            scene: a.clone(),
            base: Some(stored),
        };
        let p = Preview {
            scene: a.clone(),
            camera: moved,
        };
        assert_eq!(unsaved.to_save(Some(&p)), Some(moved));
        // Moved back to where it was, or shown in another scene, there is nothing to save.
        let back = Preview {
            scene: a.clone(),
            camera: stored,
        };
        assert_eq!(unsaved.to_save(Some(&back)), None);
        let elsewhere = Preview {
            scene: b.clone(),
            camera: moved,
        };
        assert_eq!(unsaved.to_save(Some(&elsewhere)), None);
        assert_eq!(unsaved.to_save(None), None);
        // It stays while A is shown with the camera it started from, however long.
        assert!(!unsaved.stale(Some((Some(&a), Some(stored)))));
        // Leaving A, or showing nothing, drops it: coming back shows the saved camera.
        assert!(unsaved.stale(Some((Some(&b), Some(stored)))));
        assert!(unsaved.stale(Some((None, None))));
        assert!(unsaved.stale(None));
        // A's camera set elsewhere, or removed, wins over the unsaved move.
        assert!(unsaved.stale(Some((Some(&a), Some(moved)))));
        assert!(unsaved.stale(Some((Some(&a), None))));
        // Esc on a drag that started at the stored camera drops the move; one that
        // started from an unsaved camera goes back to it.
        assert!(unsaved.back_to_base(stored));
        assert!(!unsaved.back_to_base(moved));
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
    fn a_look_turns_the_camera_where_it_stands() {
        let start = SceneCamera::framing(CANVAS);
        let turned = look(start, 300.0, 0.0);
        assert_eq!(turned.position, start.position);
        assert!((distance(turned) - distance(start)).abs() < 0.5);
        // Right looks right: the target moves toward +x, the camera's right.
        assert!(get(turned.target)[0] > get(start.target)[0]);
        // Down looks down (+y), and stops short of straight down.
        let down = look(start, 0.0, 10_000.0);
        let (p, t) = (get(down.position), get(down.target));
        let below = (t[1] - p[1])
            .atan2((t[0] - p[0]).hypot(t[2] - p[2]))
            .to_degrees();
        assert!((below - MAX_ELEVATION_DEG).abs() < 0.01, "{below}");
        assert!(down.view(CANVAS).is_ok());
    }

    #[test]
    fn a_pan_keeps_the_target_plane_under_the_pointer() {
        let start = SceneCamera::framing(CANVAS);
        let seen = view(Some(start), CANVAS).expect("valid");
        // A point of the target's plane, where the press was.
        let grabbed = seen
            .on_plane(CanvasPoint { x: 700.0, y: 400.0 }, 0.0)
            .expect("meets");
        let moved = pan(start, 120.0, -80.0, CANVAS);
        assert_eq!(moved.fov_deg, start.fov_deg);
        let after = view(Some(moved), CANVAS).expect("valid");
        let Placed::At(at) = after.place(vec3([grabbed.0, grabbed.1, 0.0]).expect("finite")) else {
            panic!("in front");
        };
        assert!(
            (at.x - 820.0).abs() < 0.05 && (at.y - 320.0).abs() < 0.05,
            "{at:?}"
        );
    }

    #[test]
    fn the_wheel_zooms_the_field_of_view_within_its_bounds() {
        let start = SceneCamera::framing(CANVAS);
        let narrower = zoom(start, -100.0);
        assert!(narrower.fov_deg.get() < start.fov_deg.get());
        assert_eq!(narrower.position, start.position);
        assert!((zoom(start, 1e6).fov_deg.get() - MAX_FOV_DEG).abs() < 1e-3);
        assert!((zoom(start, -1e6).fov_deg.get() - MIN_FOV_DEG).abs() < 1e-3);
        let back = wheel(CameraWheel::Dolly, start, 100.0);
        assert!(distance(back) > distance(start));
        assert_eq!(back.fov_deg, start.fov_deg);
    }

    #[test]
    fn camera_controls_bind_the_buttons_and_keep_their_choice() {
        let defaults = CameraControls::default();
        assert_eq!(defaults.drag(0, false), Some(CameraDrag::Orbit));
        assert_eq!(defaults.drag(2, false), Some(CameraDrag::Pan));
        assert_eq!(defaults.drag(1, false), Some(CameraDrag::Pan));
        assert_eq!(defaults.drag(0, true), Some(CameraDrag::Dolly));
        assert_eq!(defaults.drag(3, false), None);
        assert_eq!(defaults.wheel(false), CameraWheel::Zoom);
        assert_eq!(defaults.wheel(true), CameraWheel::Dolly);
        let moving = CameraControls {
            left: CameraDrag::Pan,
            wheel: CameraWheel::Dolly,
        };
        // A left drag that moves leaves the other buttons to orbit.
        assert_eq!(moving.drag(2, false), Some(CameraDrag::Orbit));
        assert_eq!(CameraControls::from_stored(Some(&moving.stored())), moving);
        let looking = CameraControls {
            left: CameraDrag::Look,
            ..defaults
        };
        assert_eq!(
            CameraControls::from_stored(Some(&looking.stored())),
            looking
        );
        let dollying = CameraControls {
            left: CameraDrag::Dolly,
            ..defaults
        };
        assert_eq!(
            CameraControls::from_stored(Some(&dollying.stored())),
            dollying
        );
        assert_eq!(dollying.drag(2, false), Some(CameraDrag::Pan));
        assert_eq!(CameraControls::from_stored(Some("nonsense")), defaults);
        assert_eq!(CameraControls::from_stored(None), defaults);
    }

    #[test]
    fn a_depth_drag_moves_an_item_along_the_line_of_sight() {
        let mut camera = SceneCamera::framing(CANVAS);
        camera.position = vec3([400.0, 300.0, 1100.0]).expect("finite");
        let seen = view(Some(camera), CANVAS).expect("valid");
        let g = item(900.0, 500.0, 200.0, 200.0, -300.0);
        let before = seen.depth_of(g);
        let pushed = pose(Pose::Depth, g, &seen, 0.0, -DEPTH_DRAG_PX).expect("valid");
        // Up by DEPTH_DRAG_PX doubles its distance, and it stays where it was on screen.
        assert!((seen.depth_of(pushed) / before - 2.0).abs() < 1e-3);
        let at = |g: ItemGeometry| match seen
            .place(vec3([g.center.x.get(), g.center.y.get(), g.depth_px.get()]).expect("finite"))
        {
            Placed::At(at) => at,
            _ => panic!("in front"),
        };
        let (a, b) = (at(g), at(pushed));
        assert!(
            (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() < 0.05,
            "{a:?} {b:?}"
        );
        let closer = pose(Pose::Depth, g, &seen, 0.0, DEPTH_DRAG_PX).expect("valid");
        assert!((seen.depth_of(closer) / before - 0.5).abs() < 1e-3);
        // However far down, it stays in front of the camera (#1633 round 1, B1).
        let far_down = pose(Pose::Depth, g, &seen, 0.0, 5_000.0).expect("valid");
        assert!(seen.depth_of(far_down) > 0.0);
        let (c, d) = (at(g), at(far_down));
        assert!(
            (c.x - d.x).abs() < 0.05 && (c.y - d.y).abs() < 0.05,
            "{c:?} {d:?}"
        );
    }

    #[test]
    fn a_turn_drag_turns_and_tilts_within_half_a_turn() {
        let seen = framing();
        let g = item(960.0, 540.0, 200.0, 200.0, 0.0);
        let turned = pose(Pose::Turn, g, &seen, 40.0, -20.0).expect("valid");
        assert!((turned.turn_deg.get() - 20.0).abs() < 1e-4);
        assert!((turned.tilt_deg.get() - 10.0).abs() < 1e-4);
        assert_eq!(turned.center, g.center);
        let mut facing = g;
        facing.billboard = true;
        assert_eq!(pose(Pose::Turn, facing, &seen, 40.0, -20.0), None);
        let round = pose(Pose::Turn, g, &seen, 400.0, 0.0).expect("valid");
        assert!(
            (round.turn_deg.get() + 160.0).abs() < 1e-3,
            "{}",
            round.turn_deg.get()
        );
    }

    /// #1054: an item passing beside the camera, partly behind it, is outlined, labelled
    /// and clicked by the part in front of it, which is what the compositor draws.
    #[test]
    fn an_item_partly_behind_the_camera_is_outlined_and_hit_by_its_front() {
        let seen = framing();
        let camera_z = SceneCamera::framing(CANVAS).position.z.get();
        // 200 px in front of the camera and just below its axis, tilted nearly flat and
        // long enough to reach well behind it.
        let mut g = item(960.0, 560.0, 100.0, 2000.0, camera_z - 200.0);
        g.tilt_deg = Finite32::new(80.0).expect("finite");
        assert!(
            corners(&g, &seen).is_none(),
            "a corner is behind the camera"
        );
        let front = polygon(&g, &Shape::Box, &seen).expect("its front is drawn");
        assert!((3..=5).contains(&front.len()), "{front:?}");
        let Placed::At(centre) =
            seen.place(Vec3::new(960.0, 560.0, camera_z - 200.0).expect("finite"))
        else {
            panic!("the centre is in front");
        };
        let centre = Point {
            x: centre.x,
            y: centre.y,
        };
        assert!(
            contains(&g, &Shape::Box, &seen, centre),
            "its centre, at {centre:?}, is hit"
        );
        assert!(!contains(
            &g,
            &Shape::Box,
            &seen,
            Point { x: 100.0, y: 100.0 }
        ));
        assert!(!outline(&g, &Shape::Box, &seen).is_empty());
        assert!(screen_box(&g, &Shape::Box, &seen).is_some());
    }

    #[test]
    fn a_marquee_box_bounds_the_projected_corners() {
        let seen = framing();
        let g = item(500.0, 300.0, 400.0, 200.0, 0.0);
        let bounds = screen_box(&g, &Shape::Box, &seen).expect("in front");
        assert!((bounds.center.x.get() - 500.0).abs() < 0.05);
        assert!((bounds.size.width.get() - 400.0).abs() < 0.1);
    }

    /// A 3D group over the whole canvas whose children are `parts`.
    fn group(parts: Vec<(ItemGeometry, Shape)>) -> Shape {
        Shape::Group {
            canvas: CANVAS,
            parts,
        }
    }

    /// A model under the pointer shows its bounds as a box: twelve edges, each ending at
    /// a projected corner of its bounds. A plain card, and a model reaching behind the
    /// camera, draw none, so they keep their outline.
    #[test]
    fn a_model_shows_its_bounds_as_twelve_edges() {
        let seen = framing();
        let mut g = item(960.0, 540.0, 400.0, 200.0, 0.0);
        g.turn_deg = Finite32::new(30.0).expect("finite");
        let bounds = SolidBounds::of_size(400.0, 200.0, 160.0);
        let path = bounds_edges(&g, &Shape::Model(bounds), &seen).expect("a box");
        assert_eq!(path.matches('M').count(), 12, "{path}");
        let corners: Vec<CanvasPoint> = g
            .solid(bounds, Some(&seen))
            .expect("placed")
            .corners(bounds)
            .iter()
            .map(|placed| match placed {
                Placed::At(at) => *at,
                _ => panic!("in front"),
            })
            .collect();
        let hull = g.solid_outline(bounds, Some(&seen)).expect("an outline");
        // A turned box has corners inside its outline: the edges the outline hides.
        assert!(hull.len() < corners.len());
        for corner in &corners {
            assert!(
                path.contains(&format!("{},{}", corner.x, corner.y)),
                "corner {corner:?} is an end of an edge: {path}"
            );
        }
        assert!(bounds_edges(&g, &Shape::Box, &seen).is_none());
        // A hundred times deeper than wide: its near corners land behind the camera.
        let deep = SolidBounds::of_size(1.0, 1.0, 100.0);
        let g = item(960.0, 540.0, 400.0, 400.0, 0.0);
        assert!(bounds_edges(&g, &Shape::Model(deep), &seen).is_none());
    }

    /// An object's box is its share of the model's box, placed as the model is: the
    /// whole model's fractions give the model's own box, and half of it a smaller one.
    #[test]
    fn an_objects_box_is_its_share_of_the_models() {
        let seen = framing();
        let mut g = item(960.0, 540.0, 400.0, 200.0, 0.0);
        g.turn_deg = Finite32::new(20.0).expect("finite");
        let shape = Shape::Model(SolidBounds::of_size(400.0, 200.0, 160.0));
        let whole = object_edges(&g, &shape, ([0.0; 3], [1.0; 3]), &seen).expect("a box");
        assert_eq!(Some(whole.clone()), bounds_edges(&g, &shape, &seen));
        let half = object_edges(&g, &shape, ([0.0; 3], [0.5, 1.0, 1.0]), &seen).expect("a box");
        assert_eq!(half.matches('M').count(), 12);
        assert_ne!(half, whole);
        assert!(object_edges(&g, &Shape::Box, ([0.0; 3], [1.0; 3]), &seen).is_none());
    }

    /// A 3D group shows a box for each model it places, and none for its cards.
    #[test]
    fn a_3d_group_shows_a_box_for_each_model() {
        let seen = framing();
        let group = Shape::Group {
            canvas: CANVAS,
            parts: vec![
                (
                    item(600.0, 540.0, 200.0, 200.0, 0.0),
                    Shape::Model(SolidBounds::of_size(200.0, 200.0, 200.0)),
                ),
                (item(960.0, 540.0, 200.0, 200.0, 0.0), Shape::Box),
                (
                    item(1320.0, 540.0, 200.0, 200.0, 0.0),
                    Shape::Model(SolidBounds::of_size(200.0, 200.0, 100.0)),
                ),
            ],
        };
        let whole = item(
            CANVAS.width as f32 / 2.0,
            CANVAS.height as f32 / 2.0,
            CANVAS.width as f32,
            CANVAS.height as f32,
            0.0,
        );
        let path = bounds_edges(&whole, &group, &seen).expect("boxes");
        assert_eq!(path.matches('M').count(), 24, "{path}");
    }

    /// ADR-0164 decision 6 (#1596): a 3D group is outlined by the hull of its children as
    /// its item places them, so the outline follows a drag of the item's own geometry; a
    /// point on a child hits it at that child's depth, and the gap between them falls
    /// through to what is behind.
    #[test]
    fn a_3d_group_is_outlined_hit_and_dragged_by_its_children() {
        let seen = framing();
        let rig = group(vec![
            (item(480.0, 540.0, 200.0, 200.0, 0.0), Shape::Box),
            (item(1440.0, 540.0, 200.0, 200.0, 0.0), Shape::Box),
        ]);
        let at = item(960.0, 540.0, 1920.0, 1080.0, 0.0);
        let bounds = screen_box(&at, &rig, &seen).expect("seen");
        assert!((bounds.size.width.get() - 1160.0).abs() < 0.5, "{bounds:?}");
        assert!((bounds.size.height.get() - 200.0).abs() < 0.5, "{bounds:?}");
        // Dragged right by 100 px, the children's outline moves with the item.
        let moved = item(1060.0, 540.0, 1920.0, 1080.0, 0.0);
        let after = screen_box(&moved, &rig, &seen).expect("seen");
        assert!((after.center.x.get() - bounds.center.x.get() - 100.0).abs() < 0.5);

        let wall = item(960.0, 540.0, 1920.0, 1080.0, -100.0);
        let items = [("rig", &at, &rig), ("wall", &wall, &Shape::Box)];
        let on_child = Point { x: 480.0, y: 540.0 };
        let gap = Point { x: 960.0, y: 540.0 };
        // The wall is later in stacking order but behind the child.
        assert_eq!(topmost_at(items.into_iter(), &seen, on_child), Some("rig"));
        assert_eq!(topmost_at(items.into_iter(), &seen, gap), Some("wall"));
        assert!(!contains(&at, &rig, &seen, gap));
    }

    /// A 3D group nested in another is placed inside its parent's placed space: a half-size
    /// rig in the left half of its parent puts its child's centre at a quarter.
    #[test]
    fn a_nested_3d_group_is_placed_through_both_groups() {
        let seen = framing();
        let inner = group(vec![(item(960.0, 540.0, 200.0, 200.0, 0.0), Shape::Box)]);
        let outer = group(vec![(item(480.0, 540.0, 960.0, 540.0, 0.0), inner)]);
        let at = item(960.0, 540.0, 1920.0, 1080.0, 0.0);
        let bounds = screen_box(&at, &outer, &seen).expect("seen");
        assert!((bounds.center.x.get() - 480.0).abs() < 0.5, "{bounds:?}");
        assert!((bounds.size.width.get() - 100.0).abs() < 0.5, "{bounds:?}");
        assert!(contains(&at, &outer, &seen, Point { x: 480.0, y: 540.0 }));
        assert!(!contains(&at, &outer, &seen, Point { x: 960.0, y: 540.0 }));
    }

    #[test]
    fn a_model_reaching_past_the_near_plane_keeps_its_card_outline() {
        let seen = framing();
        let g = item(960.0, 540.0, 400.0, 400.0, 0.0);
        // A hundred times deeper than wide: its near corners land behind the camera.
        let deep = SolidBounds::of_size(1.0, 1.0, 100.0);
        assert!(g.solid_outline(deep, Some(&seen)).is_none());
        let card = polygon(&g, &Shape::Box, &seen).expect("the card box is in front");
        assert_eq!(polygon(&g, &Shape::Model(deep), &seen), Some(card));
        assert!(contains(
            &g,
            &Shape::Model(deep),
            &seen,
            Point { x: 960.0, y: 540.0 }
        ));
    }
}
