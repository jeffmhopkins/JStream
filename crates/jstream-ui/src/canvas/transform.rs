//! Pointer gestures on item boxes (ADR-0022): mapping the page to canvas pixels, hit
//! tests, and the move, resize and rotate that a drag applies to an item's geometry.
//!
//! Everything here works on the box alone (`center`, `size`, `rotation_deg`), which
//! ADR-0022 says is enough for handles, hit tests and snapping: no source size is
//! needed. Rotation is clockwise in the y-down canvas, as in `jstream-protocol`'s
//! placement formula, so the overlay and the compositor agree.
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0022-item-transform-and-canvas.md

use crate::canvas::snap::{Guide, SnapTargets};
use jstream_control::protocol::{Canvas, Finite32, ItemGeometry, Size, Vec2};

/// The smallest box side a resize leaves, in canvas pixels.
pub const MIN_SIZE: f32 = 10.0;
/// The side of a drawn resize handle, in screen pixels.
pub const HANDLE_SIZE: f32 = 10.0;
/// The diameter of the drawn rotate knob, in screen pixels.
pub const KNOB_SIZE: f32 = 14.0;
/// Half the side of the square a press on a handle hits, in screen pixels, before
/// [`handle_at`]'s 2 px of slack.
pub const HANDLE_RADIUS: f32 = 6.0;
/// How far above the box's top edge the rotate knob's centre sits, in screen pixels:
/// the knob's top is 38 px above the edge.
pub const ROTATE_OFFSET: f32 = 38.0 - KNOB_SIZE / 2.0;
/// How long the stem from the top edge towards the knob is, in screen pixels.
pub const STEM_LENGTH: f32 = 30.0;
/// The rotation step with Ctrl held, in degrees.
pub const ROTATE_STEP: f32 = 45.0;
/// How close to a multiple of 90° a rotation snaps to it, in degrees.
pub const ROTATE_SNAP: f32 = 3.0;
/// How fast the pointer moves the item with Shift held.
pub const SLOW_SPEED: f32 = 0.5;

/// A point in canvas pixels, x right and y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
}

/// How the canvas is drawn in the stage element: scaled to fit inside it, centred
/// (SVG's `xMidYMid meet`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Screen pixels per canvas pixel.
    pub scale: f32,
    /// Where the canvas's left edge is in the element, in screen pixels.
    pub offset_x: f32,
    /// Where the canvas's top edge is in the element, in screen pixels.
    pub offset_y: f32,
}

impl Viewport {
    /// The fit of `canvas` in an element `width` by `height` screen pixels, or `None`
    /// while the element has no size (not laid out yet).
    pub fn fit(canvas: Canvas, width: f32, height: f32) -> Option<Viewport> {
        if width <= 0.0 || height <= 0.0 || canvas.width == 0 || canvas.height == 0 {
            return None;
        }
        let canvas_w = canvas.width as f32;
        let canvas_h = canvas.height as f32;
        let scale = (width / canvas_w).min(height / canvas_h);
        Some(Viewport {
            scale,
            offset_x: (width - canvas_w * scale) * 0.5,
            offset_y: (height - canvas_h * scale) * 0.5,
        })
    }

    /// The canvas point under a screen point given relative to the element.
    pub fn to_canvas(self, x: f32, y: f32) -> Point {
        Point {
            x: (x - self.offset_x) / self.scale,
            y: (y - self.offset_y) / self.scale,
        }
    }

    /// A length in screen pixels, in canvas pixels.
    pub fn canvas_len(self, screen: f32) -> f32 {
        screen / self.scale
    }
}

/// A handle on the selected item's box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// A resize handle. `x` and `y` are -1, 0 or 1: which edge of the box, on each axis,
    /// it drags (0 for an edge handle's other axis). The opposite edge stays put.
    Resize {
        /// -1 for the left edge, 1 for the right, 0 for neither.
        x: i8,
        /// -1 for the top edge, 1 for the bottom, 0 for neither.
        y: i8,
    },
    /// The rotate handle, above the top edge.
    Rotate,
}

/// The eight resize handles: corners, then edges.
pub const RESIZE_HANDLES: [Handle; 8] = [
    Handle::Resize { x: -1, y: -1 },
    Handle::Resize { x: 1, y: -1 },
    Handle::Resize { x: 1, y: 1 },
    Handle::Resize { x: -1, y: 1 },
    Handle::Resize { x: 0, y: -1 },
    Handle::Resize { x: 1, y: 0 },
    Handle::Resize { x: 0, y: 1 },
    Handle::Resize { x: -1, y: 0 },
];

/// Rotates a vector clockwise (in the y-down canvas) by `deg`.
fn rotate(x: f32, y: f32, deg: f32) -> (f32, f32) {
    let (sin, cos) = deg.to_radians().sin_cos();
    (x * cos - y * sin, x * sin + y * cos)
}

fn center(geometry: &ItemGeometry) -> Point {
    Point {
        x: geometry.center.x.get(),
        y: geometry.center.y.get(),
    }
}

fn half(geometry: &ItemGeometry) -> (f32, f32) {
    (
        geometry.size.width.get() * 0.5,
        geometry.size.height.get() * 0.5,
    )
}

/// `point` in the box's local frame: from the centre, before rotation.
fn to_local(geometry: &ItemGeometry, point: Point) -> (f32, f32) {
    let c = center(geometry);
    rotate(point.x - c.x, point.y - c.y, -geometry.rotation_deg.get())
}

/// A local-frame point back on the canvas.
fn from_local(geometry: &ItemGeometry, x: f32, y: f32) -> Point {
    let c = center(geometry);
    let (dx, dy) = rotate(x, y, geometry.rotation_deg.get());
    Point {
        x: c.x + dx,
        y: c.y + dy,
    }
}

/// The box's four corners on the canvas: top-left, top-right, bottom-right and
/// bottom-left of the unrotated box, as `jstream-protocol`'s `project` orders them.
pub fn corners(geometry: &ItemGeometry) -> [Point; 4] {
    let (hw, hh) = half(geometry);
    [
        from_local(geometry, -hw, -hh),
        from_local(geometry, hw, -hh),
        from_local(geometry, hw, hh),
        from_local(geometry, -hw, hh),
    ]
}

/// Whether `point` is inside the box.
pub fn contains(geometry: &ItemGeometry, point: Point) -> bool {
    let (x, y) = to_local(geometry, point);
    let (hw, hh) = half(geometry);
    x.abs() <= hw && y.abs() <= hh
}

/// The topmost box under `point`: the last in stacking order that contains it.
pub fn topmost_at<'a, T>(
    items: impl DoubleEndedIterator<Item = (T, &'a ItemGeometry)>,
    point: Point,
) -> Option<T> {
    items
        .rev()
        .find(|(_, geometry)| contains(geometry, point))
        .map(|(id, _)| id)
}

/// Where a handle is drawn, on the canvas. The rotate handle's distance from the box
/// is in screen pixels, so it needs the viewport.
pub fn handle_point(geometry: &ItemGeometry, handle: Handle, viewport: Viewport) -> Point {
    let (hw, hh) = half(geometry);
    match handle {
        Handle::Resize { x, y } => from_local(geometry, f32::from(x) * hw, f32::from(y) * hh),
        Handle::Rotate => from_local(geometry, 0.0, -hh - viewport.canvas_len(ROTATE_OFFSET)),
    }
}

/// The handle of the selected box under `point`, if any. The rotate handle is tried
/// first, then corners before edges, so a small box's corners stay reachable.
pub fn handle_at(geometry: &ItemGeometry, point: Point, viewport: Viewport) -> Option<Handle> {
    let reach = viewport.canvas_len(HANDLE_RADIUS + 2.0);
    std::iter::once(Handle::Rotate)
        .chain(RESIZE_HANDLES)
        .find(|handle| {
            let at = handle_point(geometry, *handle, viewport);
            (at.x - point.x).abs() <= reach && (at.y - point.y).abs() <= reach
        })
}

/// What a gesture does to the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureKind {
    /// Drag the whole box.
    Move,
    /// Drag one resize handle.
    Resize {
        /// The handle's edge on x: -1, 0 or 1.
        x: i8,
        /// The handle's edge on y: -1, 0 or 1.
        y: i8,
    },
    /// Turn the box about its centre.
    Rotate,
}

impl From<Handle> for GestureKind {
    fn from(handle: Handle) -> Self {
        match handle {
            Handle::Resize { x, y } => GestureKind::Resize { x, y },
            Handle::Rotate => GestureKind::Rotate,
        }
    }
}

/// The modifier keys a gesture reads. Shift's half speed is [`Pointer`]'s, not the
/// gesture's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Ctrl (or Cmd): a move doesn't snap, a resize stops keeping the aspect ratio, and
    /// rotation moves in 45° steps.
    pub ctrl: bool,
}

/// The pointer a gesture follows: the real one, except that while Shift is held it
/// moves at [`SLOW_SPEED`]. It follows the real pointer's movement, so pressing or
/// releasing Shift mid-drag doesn't make the item jump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointer {
    /// Where the real pointer was last.
    last: Point,
    /// Where the gesture's pointer is.
    at: Point,
}

impl Pointer {
    /// A pointer pressed at `point`.
    pub fn new(point: Point) -> Pointer {
        Pointer {
            last: point,
            at: point,
        }
    }

    /// Follows the real pointer to `point`, at half speed when `slow`, and returns where
    /// the gesture's pointer is now.
    pub fn follow(&mut self, point: Point, slow: bool) -> Point {
        let speed = if slow { SLOW_SPEED } else { 1.0 };
        self.at = Point {
            x: self.at.x + (point.x - self.last.x) * speed,
            y: self.at.y + (point.y - self.last.y) * speed,
        };
        self.last = point;
        self.at
    }
}

/// One pointer drag on one item: the geometry it started from and where it started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gesture {
    /// What the drag does.
    pub kind: GestureKind,
    /// The item's geometry when the drag started.
    pub start: ItemGeometry,
    /// The canvas point the drag started at.
    pub origin: Point,
}

/// A gesture's result for one pointer position.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    /// The item's geometry now.
    pub geometry: ItemGeometry,
    /// The snap guides to draw.
    pub guides: Vec<Guide>,
}

impl Gesture {
    /// The geometry for the pointer at `pointer`. `snap` is `None` when snapping is
    /// off; `tolerance` is how close, in canvas pixels, an edge or centre snaps.
    /// Returns `None` only if the arithmetic left a non-finite number, which a finite
    /// pointer and box never do.
    pub fn update(
        &self,
        pointer: Point,
        modifiers: Modifiers,
        snap: Option<&SnapTargets>,
        tolerance: f32,
    ) -> Option<Update> {
        match self.kind {
            // Ctrl moves without snapping. A rotated box doesn't snap either: its edges
            // aren't the canvas's axes.
            GestureKind::Move => {
                let upright = self.start.rotation_deg.get().rem_euclid(360.0) == 0.0;
                let snap = snap.filter(|_| upright && !modifiers.ctrl);
                self.drag(pointer, snap, tolerance)
            }
            GestureKind::Resize { x, y } => self.resize(x, y, pointer, modifiers, snap, tolerance),
            GestureKind::Rotate => self.turn(pointer, modifiers, snap.is_some()),
        }
    }

    fn drag(&self, pointer: Point, snap: Option<&SnapTargets>, tolerance: f32) -> Option<Update> {
        let c = center(&self.start);
        let moved = with_box(
            &self.start,
            c.x + pointer.x - self.origin.x,
            c.y + pointer.y - self.origin.y,
            self.start.size.width.get(),
            self.start.size.height.get(),
            self.start.rotation_deg.get(),
        )?;
        let Some(snap) = snap else {
            return Some(Update {
                geometry: moved,
                guides: Vec::new(),
            });
        };
        let (dx, dy, guides) = snap.snap_move(&moved, tolerance);
        let mc = center(&moved);
        let geometry = with_box(
            &moved,
            mc.x + dx,
            mc.y + dy,
            moved.size.width.get(),
            moved.size.height.get(),
            moved.rotation_deg.get(),
        )?;
        Some(Update { geometry, guides })
    }

    fn resize(
        &self,
        sx: i8,
        sy: i8,
        pointer: Point,
        modifiers: Modifiers,
        snap: Option<&SnapTargets>,
        tolerance: f32,
    ) -> Option<Update> {
        let start = &self.start;
        let rotation = start.rotation_deg.get();
        // Snapping a resized edge is only defined for an upright box, whose edges are
        // the canvas's axes.
        let upright = rotation.rem_euclid(360.0) == 0.0;
        let snap = snap.filter(|_| upright);
        let (fx, fy) = (f32::from(sx), f32::from(sy));
        let (hw, hh) = half(start);
        // Follow the pointer's movement, not its position: a press anywhere within the
        // handle's reach grabs it, and the edge must not jump to the pointer.
        let grabbed = from_local(start, fx * hw, fy * hh);
        let mut pointer = Point {
            x: pointer.x - (self.origin.x - grabbed.x),
            y: pointer.y - (self.origin.y - grabbed.y),
        };
        let (mut snapped_x, mut snapped_y) = (false, false);
        if let Some(snap) = snap {
            if sx != 0
                && let Some(x) = snap.snap_x(pointer.x, tolerance)
            {
                pointer.x = x;
                snapped_x = true;
            }
            if sy != 0
                && let Some(y) = snap.snap_y(pointer.y, tolerance)
            {
                pointer.y = y;
                snapped_y = true;
            }
        }
        let (lx, ly) = to_local(start, pointer);
        let (anchor_x, anchor_y) = (-fx * hw, -fy * hh);
        let start_w = start.size.width.get();
        let start_h = start.size.height.get();
        let mut width = if sx == 0 {
            start_w
        } else {
            (fx * (lx - anchor_x)).max(MIN_SIZE)
        };
        let mut height = if sy == 0 {
            start_h
        } else {
            (fy * (ly - anchor_y)).max(MIN_SIZE)
        };
        if !modifiers.ctrl {
            // The aspect ratio is kept. An edge sets it, and the other axis grows about
            // the centre; at a corner the axis that snapped sets it, else the one whose
            // size changed more, so a drag straight inward along one axis shrinks the box.
            let (kx, ky) = (width / start_w, height / start_h);
            let k = match (sx, sy) {
                (_, 0) => kx,
                (0, _) => ky,
                _ if snapped_x != snapped_y => {
                    if snapped_x {
                        kx
                    } else {
                        ky
                    }
                }
                _ if (kx - 1.0).abs() >= (ky - 1.0).abs() => kx,
                _ => ky,
            };
            let k = k.max(MIN_SIZE / start_w).max(MIN_SIZE / start_h);
            width = start_w * k;
            height = start_h * k;
        }
        let local_x = if sx == 0 {
            0.0
        } else {
            anchor_x + fx * width * 0.5
        };
        let local_y = if sy == 0 {
            0.0
        } else {
            anchor_y + fy * height * 0.5
        };
        let c = from_local(start, local_x, local_y);
        let geometry = with_box(start, c.x, c.y, width, height, rotation)?;
        let guides = match snap {
            Some(snap) => snap.guides_on(&geometry),
            None => Vec::new(),
        };
        Some(Update { geometry, guides })
    }

    fn turn(&self, pointer: Point, modifiers: Modifiers, snapping: bool) -> Option<Update> {
        let c = center(&self.start);
        let angle = |p: Point| (p.y - c.y).atan2(p.x - c.x).to_degrees();
        let raw = self.start.rotation_deg.get() + angle(pointer) - angle(self.origin);
        let rotation = snap_angle(normalize_deg(raw), modifiers.ctrl, snapping);
        let geometry = with_box(
            &self.start,
            c.x,
            c.y,
            self.start.size.width.get(),
            self.start.size.height.get(),
            rotation,
        )?;
        Some(Update {
            geometry,
            guides: Vec::new(),
        })
    }
}

/// An angle in degrees brought into (-180, 180].
pub fn normalize_deg(deg: f32) -> f32 {
    let wrapped = (deg + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

/// A rotation after snapping: to 45° steps with `step` (Ctrl), else to the nearest
/// multiple of 90° within [`ROTATE_SNAP`] when `snapping`, else rounded to 0.1°.
pub fn snap_angle(deg: f32, step: bool, snapping: bool) -> f32 {
    if step {
        return normalize_deg((deg / ROTATE_STEP).round() * ROTATE_STEP);
    }
    let right = (deg / 90.0).round() * 90.0;
    if snapping && (deg - right).abs() <= ROTATE_SNAP {
        normalize_deg(right)
    } else {
        normalize_deg((deg * 10.0).round() / 10.0)
    }
}

/// `start` moved by `dx`, `dy` canvas pixels: what an arrow key does.
pub fn nudge(start: &ItemGeometry, dx: f32, dy: f32) -> Option<ItemGeometry> {
    let c = center(start);
    with_box(
        start,
        c.x + dx,
        c.y + dy,
        start.size.width.get(),
        start.size.height.get(),
        start.rotation_deg.get(),
    )
}

/// `base` with a new box, keeping its fit mode and 3D fields.
fn with_box(
    base: &ItemGeometry,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    rotation: f32,
) -> Option<ItemGeometry> {
    Some(ItemGeometry {
        center: Vec2::new(x, y)?,
        size: Size::try_new(width, height).ok()?,
        rotation_deg: Finite32::new(rotation)?,
        ..*base
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::snap::SnapTargets;

    fn geometry(cx: f32, cy: f32, w: f32, h: f32, rotation: f32) -> ItemGeometry {
        let base = ItemGeometry::try_new(
            Vec2::new(cx, cy).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("valid");
        with_box(&base, cx, cy, w, h, rotation).expect("valid")
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn assert_box(g: &ItemGeometry, cx: f32, cy: f32, w: f32, h: f32) {
        assert!(
            close(g.center.x.get(), cx)
                && close(g.center.y.get(), cy)
                && close(g.size.width.get(), w)
                && close(g.size.height.get(), h),
            "got centre ({}, {}) size {}×{}, want ({cx}, {cy}) {w}×{h}",
            g.center.x.get(),
            g.center.y.get(),
            g.size.width.get(),
            g.size.height.get(),
        );
    }

    const NONE: Modifiers = Modifiers { ctrl: false };
    const CTRL: Modifiers = Modifiers { ctrl: true };

    #[test]
    fn the_viewport_fits_and_centres_the_canvas() {
        let canvas = Canvas::DEFAULT;
        let wide = Viewport::fit(canvas, 1000.0, 400.0).expect("laid out");
        assert!(close(wide.scale, 400.0 / 1080.0));
        assert!(close(wide.offset_y, 0.0));
        assert!(close(wide.offset_x, (1000.0 - 1920.0 * wide.scale) / 2.0));
        let p = wide.to_canvas(500.0, 200.0);
        assert!(close(p.x, 960.0) && close(p.y, 540.0));
        assert_eq!(Viewport::fit(canvas, 0.0, 400.0), None);
    }

    #[test]
    fn the_box_matches_the_protocols_placement() {
        let g = geometry(400.0, 300.0, 200.0, 100.0, 30.0);
        let quad = g.project().expect("projects");
        for (mine, theirs) in corners(&g).iter().zip(quad.corners) {
            let jstream_control::protocol::Placed::At(theirs) = theirs else {
                panic!("orthographic corners are always placed");
            };
            assert!(close(mine.x, theirs.x) && close(mine.y, theirs.y));
        }
    }

    #[test]
    fn hit_tests_follow_rotation_and_stacking() {
        let tilted = geometry(100.0, 100.0, 200.0, 20.0, 90.0);
        // Turned a quarter, the 200-wide box is now tall.
        assert!(contains(&tilted, Point { x: 100.0, y: 190.0 }));
        assert!(!contains(&tilted, Point { x: 190.0, y: 100.0 }));
        let below = geometry(100.0, 100.0, 50.0, 50.0, 0.0);
        let above = geometry(110.0, 100.0, 50.0, 50.0, 0.0);
        let items = [("below", &below), ("above", &above)];
        assert_eq!(
            topmost_at(items.into_iter(), Point { x: 100.0, y: 100.0 }),
            Some("above")
        );
        assert_eq!(
            topmost_at(items.into_iter(), Point { x: 80.0, y: 100.0 }),
            Some("below")
        );
        assert_eq!(
            topmost_at(items.into_iter(), Point { x: 0.0, y: 0.0 }),
            None
        );
    }

    #[test]
    fn handles_are_found_in_screen_pixels() {
        let viewport = Viewport {
            scale: 0.5,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        let g = geometry(500.0, 500.0, 200.0, 100.0, 0.0);
        assert_eq!(
            handle_at(&g, Point { x: 605.0, y: 555.0 }, viewport),
            Some(Handle::Resize { x: 1, y: 1 })
        );
        // The rotate knob's centre is 31 screen pixels (62 canvas pixels) above the top
        // edge.
        assert_eq!(
            handle_at(&g, Point { x: 500.0, y: 388.0 }, viewport),
            Some(Handle::Rotate)
        );
        assert_eq!(handle_at(&g, Point { x: 500.0, y: 500.0 }, viewport), None);
    }

    #[test]
    fn a_move_follows_the_pointer() {
        let gesture = Gesture {
            kind: GestureKind::Move,
            start: geometry(100.0, 100.0, 50.0, 50.0, 10.0),
            origin: Point { x: 100.0, y: 100.0 },
        };
        let update = gesture
            .update(Point { x: 130.0, y: 80.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&update.geometry, 130.0, 80.0, 50.0, 50.0);
        assert_eq!(update.geometry.rotation_deg.get(), 10.0);
        assert!(update.guides.is_empty());
    }

    #[test]
    fn a_corner_resize_keeps_the_aspect_and_the_opposite_corner() {
        let start = geometry(100.0, 100.0, 200.0, 100.0, 0.0);
        let gesture = Gesture {
            kind: GestureKind::Resize { x: 1, y: 1 },
            start,
            origin: Point { x: 200.0, y: 150.0 },
        };
        // Dragged right by 200: twice as wide, so twice as tall, top-left fixed at (0, 50).
        let update = gesture
            .update(Point { x: 400.0, y: 150.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&update.geometry, 200.0, 150.0, 400.0, 200.0);
        // With Ctrl the aspect is free.
        let free = gesture
            .update(Point { x: 400.0, y: 150.0 }, CTRL, None, 0.0)
            .expect("finite");
        assert_box(&free.geometry, 200.0, 100.0, 400.0, 100.0);
        // Straight inward along x shrinks it: half as wide, so half as tall.
        let shrunk = gesture
            .update(Point { x: 100.0, y: 150.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&shrunk.geometry, 50.0, 75.0, 100.0, 50.0);
    }

    #[test]
    fn a_resize_follows_the_pointers_movement_not_its_position() {
        let start = geometry(100.0, 100.0, 200.0, 100.0, 0.0);
        // Grabbed 5 px left of the right edge's handle (x = 200).
        let gesture = Gesture {
            kind: GestureKind::Resize { x: 1, y: 0 },
            start,
            origin: Point { x: 195.0, y: 100.0 },
        };
        let still = gesture
            .update(Point { x: 195.0, y: 100.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&still.geometry, 100.0, 100.0, 200.0, 100.0);
        // An edge keeps the aspect too: the other axis grows about the centre.
        let moved = gesture
            .update(Point { x: 215.0, y: 100.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&moved.geometry, 110.0, 100.0, 220.0, 110.0);
        let stretched = gesture
            .update(Point { x: 215.0, y: 100.0 }, CTRL, None, 0.0)
            .expect("finite");
        assert_box(&stretched.geometry, 110.0, 100.0, 220.0, 100.0);
    }

    #[test]
    fn an_edge_resize_changes_one_side_of_a_rotated_box() {
        let start = geometry(100.0, 100.0, 200.0, 100.0, 90.0);
        // The right edge of a box turned 90° clockwise faces down the canvas.
        let gesture = Gesture {
            kind: GestureKind::Resize { x: 1, y: 0 },
            start,
            origin: handle_point(
                &start,
                Handle::Resize { x: 1, y: 0 },
                Viewport {
                    scale: 1.0,
                    offset_x: 0.0,
                    offset_y: 0.0,
                },
            ),
        };
        let update = gesture
            .update(Point { x: 100.0, y: 300.0 }, NONE, None, 0.0)
            .expect("finite");
        // The left edge (now the top, at y = 0) stays; the box is 300 long, and half as
        // wide again about its centre line.
        assert_box(&update.geometry, 100.0, 150.0, 300.0, 150.0);
    }

    #[test]
    fn a_resize_never_flips_or_vanishes() {
        let start = geometry(100.0, 100.0, 200.0, 100.0, 0.0);
        let gesture = Gesture {
            kind: GestureKind::Resize { x: 1, y: 0 },
            start,
            origin: Point { x: 200.0, y: 100.0 },
        };
        let update = gesture
            .update(
                Point {
                    x: -500.0,
                    y: 100.0,
                },
                NONE,
                None,
                0.0,
            )
            .expect("finite");
        // The shorter side stops at the minimum, and the aspect holds.
        assert_box(&update.geometry, MIN_SIZE, 100.0, 2.0 * MIN_SIZE, MIN_SIZE);
        // The left edge stayed at x = 0.
        let stretched = gesture
            .update(
                Point {
                    x: -500.0,
                    y: 100.0,
                },
                CTRL,
                None,
                0.0,
            )
            .expect("finite");
        assert_box(&stretched.geometry, MIN_SIZE / 2.0, 100.0, MIN_SIZE, 100.0);
    }

    #[test]
    fn rotation_follows_the_pointer_and_snaps() {
        let start = geometry(0.0, 0.0, 100.0, 100.0, 0.0);
        let gesture = Gesture {
            kind: GestureKind::Rotate,
            start,
            origin: Point { x: 0.0, y: -100.0 },
        };
        // From straight up to the right: a quarter turn clockwise.
        let update = gesture
            .update(Point { x: 100.0, y: 0.0 }, NONE, None, 0.0)
            .expect("finite");
        assert!(close(update.geometry.rotation_deg.get(), 90.0));
        // 33° snaps to 45° in Ctrl's steps.
        let (s, c) = 33.0_f32.to_radians().sin_cos();
        let pointer = Point {
            x: 100.0 * s,
            y: -100.0 * c,
        };
        let stepped = gesture.update(pointer, CTRL, None, 0.0).expect("finite");
        assert!(close(stepped.geometry.rotation_deg.get(), 45.0));
        let free = gesture.update(pointer, NONE, None, 0.0).expect("finite");
        assert!(close(free.geometry.rotation_deg.get(), 33.0));
        assert!(close(snap_angle(-178.0, false, true), 180.0));
        assert!(close(snap_angle(93.0, false, true), 90.0));
        assert!(close(snap_angle(93.5, false, true), 93.5));
        assert!(close(snap_angle(-170.0, false, true), -170.0));
        assert!(close(snap_angle(2.0, false, false), 2.0));
        assert!(close(snap_angle(12.34, false, false), 12.3));
        assert!(close(normalize_deg(270.0), -90.0));
    }

    #[test]
    fn ctrl_or_a_rotation_moves_without_snapping() {
        let targets = SnapTargets::new(Canvas::DEFAULT, std::iter::empty());
        let gesture = Gesture {
            kind: GestureKind::Move,
            start: geometry(100.0, 100.0, 50.0, 50.0, 0.0),
            origin: Point { x: 100.0, y: 100.0 },
        };
        // The left edge lands 2 px from the canvas edge.
        let pointer = Point { x: 27.0, y: 100.0 };
        let snapped = gesture
            .update(pointer, NONE, Some(&targets), 5.0)
            .expect("finite");
        assert!(close(snapped.geometry.center.x.get(), 25.0));
        let free = gesture
            .update(pointer, CTRL, Some(&targets), 5.0)
            .expect("finite");
        assert!(close(free.geometry.center.x.get(), 27.0));
        let tilted = Gesture {
            start: geometry(100.0, 100.0, 50.0, 50.0, 10.0),
            ..gesture
        };
        let unsnapped = tilted
            .update(pointer, NONE, Some(&targets), 5.0)
            .expect("finite");
        assert!(close(unsnapped.geometry.center.x.get(), 27.0));
    }

    #[test]
    fn shift_follows_the_pointer_at_half_speed() {
        let mut pointer = Pointer::new(Point { x: 100.0, y: 100.0 });
        assert_eq!(
            pointer.follow(Point { x: 120.0, y: 100.0 }, false),
            Point { x: 120.0, y: 100.0 }
        );
        // Shift pressed mid-drag: the next 20 px move only 10, with no jump.
        assert_eq!(
            pointer.follow(Point { x: 140.0, y: 120.0 }, true),
            Point { x: 130.0, y: 110.0 }
        );
        assert_eq!(
            pointer.follow(Point { x: 150.0, y: 120.0 }, false),
            Point { x: 140.0, y: 110.0 }
        );
    }

    #[test]
    fn a_nudge_keeps_everything_but_the_centre() {
        let mut start = geometry(10.0, 10.0, 20.0, 20.0, 45.0);
        start.fit_mode = jstream_control::protocol::FitMode::Fill;
        let moved = nudge(&start, -1.0, 10.0).expect("finite");
        assert_box(&moved, 9.0, 20.0, 20.0, 20.0);
        assert_eq!(moved.fit_mode, start.fit_mode);
        assert_eq!(moved.rotation_deg, start.rotation_deg);
    }
}
