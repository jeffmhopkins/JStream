//! Gestures on several selected items at once: moving them together, scaling them from
//! the far side of their bounds, and rotating them about the bounds' centre, as the
//! prototype's group selection does; and the box select that picks the items a marquee
//! touches.
//!
//! Like [`transform`](super::transform), everything here is pure and works on the
//! items' boxes alone.
//!
//! Docs: crates/jstream-ui/README.md#canvas

use crate::canvas::snap::{Bounds, Guide, SnapTargets};
use crate::canvas::transform::{
    Handle, Modifiers, Point, RESIZE_HANDLES, ROTATE_STEP, Viewport, normalize_deg, with_box,
};
use jstream_control::protocol::{ItemGeometry, ItemId};

/// The smallest side a group scale leaves the selection's bounds, in canvas pixels.
pub const MIN_GROUP_SIZE: f32 = 20.0;
/// How far outside the selection's bounds its dashed box is drawn, in screen pixels.
pub const GROUP_OFFSET: f32 = 4.0;
/// How far above the dashed box's top edge the group's rotate knob centre sits, in
/// screen pixels: the knob's top is 42 px above it.
pub const GROUP_ROTATE_OFFSET: f32 = 42.0 - crate::canvas::transform::KNOB_SIZE / 2.0;

/// What a gesture on the selection does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    /// Move every selected item.
    Move,
    /// Scale the selection by one of its bounds' handles.
    Scale {
        /// The handle's edge on x: -1, 0 or 1.
        x: i8,
        /// The handle's edge on y: -1, 0 or 1.
        y: i8,
    },
    /// Rotate the selection about its bounds' centre.
    Rotate,
}

impl From<Handle> for GroupKind {
    fn from(handle: Handle) -> Self {
        match handle {
            Handle::Resize { x, y } => GroupKind::Scale { x, y },
            Handle::Rotate => GroupKind::Rotate,
        }
    }
}

/// One drag on the selection: every item's geometry when it started, their bounds, and
/// where it started.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupGesture {
    /// What the drag does.
    pub kind: GroupKind,
    /// Each selected item and its geometry when the drag started.
    pub starts: Vec<(ItemId, ItemGeometry)>,
    /// The items' bounds when the drag started.
    pub bounds: Bounds,
    /// The canvas point the drag started at.
    pub origin: Point,
}

/// A group gesture's result for one pointer position.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupUpdate {
    /// Each item's geometry now, in the order of [`GroupGesture::starts`].
    pub geometries: Vec<(ItemId, ItemGeometry)>,
    /// The snap guides to draw.
    pub guides: Vec<Guide>,
}

impl GroupGesture {
    /// A gesture on `starts`, or `None` when there are no items.
    pub fn new(
        kind: GroupKind,
        starts: Vec<(ItemId, ItemGeometry)>,
        origin: Point,
    ) -> Option<Self> {
        let bounds = Bounds::around(starts.iter().map(|(_, g)| g))?;
        Some(GroupGesture {
            kind,
            starts,
            bounds,
            origin,
        })
    }

    /// The geometries for the pointer at `pointer`, as [`Gesture::update`] does for one
    /// item. A move snaps the selection's bounds, unless Ctrl is held or an item is
    /// rotated; a scale or a rotation doesn't snap.
    ///
    /// [`Gesture::update`]: crate::canvas::transform::Gesture::update
    pub fn update(
        &self,
        pointer: Point,
        modifiers: Modifiers,
        snap: Option<&SnapTargets>,
        tolerance: f32,
    ) -> Option<GroupUpdate> {
        match self.kind {
            GroupKind::Move => {
                let upright = self
                    .starts
                    .iter()
                    .all(|(_, g)| g.rotation_deg.get().rem_euclid(360.0) == 0.0);
                let snap = snap.filter(|_| upright && !modifiers.ctrl);
                self.drag(pointer, snap, tolerance)
            }
            GroupKind::Scale { x, y } => self.scale(x, y, pointer, modifiers),
            GroupKind::Rotate => self.turn(pointer, modifiers),
        }
    }

    fn drag(
        &self,
        pointer: Point,
        snap: Option<&SnapTargets>,
        tolerance: f32,
    ) -> Option<GroupUpdate> {
        let (mut dx, mut dy) = (pointer.x - self.origin.x, pointer.y - self.origin.y);
        let mut guides = Vec::new();
        if let Some(snap) = snap {
            let moved = Bounds {
                left: self.bounds.left + dx,
                top: self.bounds.top + dy,
                right: self.bounds.right + dx,
                bottom: self.bounds.bottom + dy,
            };
            let (sx, sy, lines) = snap.snap_bounds(moved, tolerance);
            dx += sx;
            dy += sy;
            guides = lines;
        }
        let geometries = self
            .starts
            .iter()
            .map(|(id, g)| {
                let moved = with_box(
                    g,
                    g.center.x.get() + dx,
                    g.center.y.get() + dy,
                    g.size.width.get(),
                    g.size.height.get(),
                    g.rotation_deg.get(),
                )?;
                Some((id.clone(), moved))
            })
            .collect::<Option<_>>()?;
        Some(GroupUpdate { geometries, guides })
    }

    fn scale(&self, sx: i8, sy: i8, pointer: Point, modifiers: Modifiers) -> Option<GroupUpdate> {
        let b = self.bounds;
        let (w0, h0) = (b.right - b.left, b.bottom - b.top);
        if w0 <= 0.0 || h0 <= 0.0 {
            return None;
        }
        let (fx, fy) = (f32::from(sx), f32::from(sy));
        let (mid_x, mid_y) = ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0);
        // The side opposite the handle stays put; on an axis the handle doesn't drag,
        // the centre does.
        let anchor_x = match sx {
            1 => b.left,
            -1 => b.right,
            _ => mid_x,
        };
        let anchor_y = match sy {
            1 => b.top,
            -1 => b.bottom,
            _ => mid_y,
        };
        // Follow the pointer's movement from the handle, not its position.
        let grabbed_x = if sx == 0 { mid_x } else { anchor_x + fx * w0 };
        let grabbed_y = if sy == 0 { mid_y } else { anchor_y + fy * h0 };
        let px = pointer.x - (self.origin.x - grabbed_x);
        let py = pointer.y - (self.origin.y - grabbed_y);
        let mut kx = if sx == 0 {
            1.0
        } else {
            fx * (px - anchor_x) / w0
        };
        let mut ky = if sy == 0 {
            1.0
        } else {
            fy * (py - anchor_y) / h0
        };
        if !modifiers.ctrl {
            // The aspect is kept: an edge sets it, a corner takes the larger change.
            let k = match (sx, sy) {
                (_, 0) => kx,
                (0, _) => ky,
                _ if (kx - 1.0).abs() >= (ky - 1.0).abs() => kx,
                _ => ky,
            };
            let k = k.max(MIN_GROUP_SIZE / w0).max(MIN_GROUP_SIZE / h0);
            (kx, ky) = (k, k);
        } else {
            kx = kx.max(MIN_GROUP_SIZE / w0);
            ky = ky.max(MIN_GROUP_SIZE / h0);
        }
        let geometries = self
            .starts
            .iter()
            .map(|(id, g)| {
                let scaled = with_box(
                    g,
                    anchor_x + (g.center.x.get() - anchor_x) * kx,
                    anchor_y + (g.center.y.get() - anchor_y) * ky,
                    g.size.width.get() * kx,
                    g.size.height.get() * ky,
                    g.rotation_deg.get(),
                )?;
                Some((id.clone(), scaled))
            })
            .collect::<Option<_>>()?;
        Some(GroupUpdate {
            geometries,
            guides: Vec::new(),
        })
    }

    fn turn(&self, pointer: Point, modifiers: Modifiers) -> Option<GroupUpdate> {
        let b = self.bounds;
        let c = Point {
            x: (b.left + b.right) / 2.0,
            y: (b.top + b.bottom) / 2.0,
        };
        let angle = |p: Point| (p.y - c.y).atan2(p.x - c.x).to_degrees();
        let mut delta = normalize_deg(angle(pointer) - angle(self.origin));
        if modifiers.ctrl {
            delta = (delta / ROTATE_STEP).round() * ROTATE_STEP;
        }
        let (sin, cos) = delta.to_radians().sin_cos();
        let geometries = self
            .starts
            .iter()
            .map(|(id, g)| {
                let (x, y) = (g.center.x.get() - c.x, g.center.y.get() - c.y);
                let rotation = normalize_deg(g.rotation_deg.get() + delta);
                let turned = with_box(
                    g,
                    c.x + x * cos - y * sin,
                    c.y + x * sin + y * cos,
                    g.size.width.get(),
                    g.size.height.get(),
                    (rotation * 10.0).round() / 10.0,
                )?;
                Some((id.clone(), turned))
            })
            .collect::<Option<_>>()?;
        Some(GroupUpdate {
            geometries,
            guides: Vec::new(),
        })
    }
}

/// The selection's dashed box: its bounds, [`GROUP_OFFSET`] screen pixels further out.
pub fn outline(bounds: Bounds, viewport: Viewport) -> Bounds {
    let pad = viewport.canvas_len(GROUP_OFFSET);
    Bounds {
        left: bounds.left - pad,
        top: bounds.top - pad,
        right: bounds.right + pad,
        bottom: bounds.bottom + pad,
    }
}

/// Where a handle of the selection's dashed box is drawn, on the canvas.
pub fn handle_point(bounds: Bounds, handle: Handle, viewport: Viewport) -> Point {
    let o = outline(bounds, viewport);
    let pick = |edge: i8, low: f32, high: f32| match edge {
        -1 => low,
        1 => high,
        _ => (low + high) / 2.0,
    };
    match handle {
        Handle::Resize { x, y } => Point {
            x: pick(x, o.left, o.right),
            y: pick(y, o.top, o.bottom),
        },
        Handle::Rotate => Point {
            x: (o.left + o.right) / 2.0,
            y: o.top - viewport.canvas_len(GROUP_ROTATE_OFFSET),
        },
    }
}

/// The handle of the selection's dashed box under `point`, if any: the rotate knob
/// first, then corners before edges.
pub fn handle_at(bounds: Bounds, point: Point, viewport: Viewport) -> Option<Handle> {
    let reach = viewport.canvas_len(crate::canvas::transform::HANDLE_RADIUS + 2.0);
    std::iter::once(Handle::Rotate)
        .chain(RESIZE_HANDLES)
        .find(|handle| {
            let at = handle_point(bounds, *handle, viewport);
            (at.x - point.x).abs() <= reach && (at.y - point.y).abs() <= reach
        })
}

/// The items whose bounds a marquee from `a` to `b` touches, in stacking order. The
/// test is on each item's axis-aligned bounds.
pub fn marquee<'a>(
    boxes: impl Iterator<Item = (&'a ItemId, &'a ItemGeometry)>,
    a: Point,
    b: Point,
) -> Vec<ItemId> {
    let area = Bounds {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x),
        bottom: a.y.max(b.y),
    };
    boxes
        .filter(|(_, g)| Bounds::of(g).meets(area))
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Canvas, Size, Vec2};

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

    /// Two boxes side by side: bounds (0, 0) to (300, 100).
    fn pair(kind: GroupKind, origin: Point) -> GroupGesture {
        GroupGesture::new(
            kind,
            vec![
                ("a".into(), geometry(50.0, 50.0, 100.0, 100.0, 0.0)),
                ("b".into(), geometry(250.0, 50.0, 100.0, 100.0, 0.0)),
            ],
            origin,
        )
        .expect("two items")
    }

    #[test]
    fn a_group_moves_together_and_snaps_by_its_bounds() {
        let gesture = pair(GroupKind::Move, Point { x: 150.0, y: 50.0 });
        let free = gesture
            .update(Point { x: 160.0, y: 70.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&free.geometries[0].1, 60.0, 70.0, 100.0, 100.0);
        assert_box(&free.geometries[1].1, 260.0, 70.0, 100.0, 100.0);
        // The bounds' left edge lands 3 px from the canvas edge and snaps to it.
        let targets = SnapTargets::new(Canvas::DEFAULT, std::iter::empty());
        let snapped = gesture
            .update(Point { x: 153.0, y: 50.0 }, NONE, Some(&targets), 5.0)
            .expect("finite");
        assert_box(&snapped.geometries[0].1, 50.0, 50.0, 100.0, 100.0);
        assert!(snapped.guides.contains(&Guide::Vertical(0.0)));
        let unsnapped = gesture
            .update(Point { x: 153.0, y: 50.0 }, CTRL, Some(&targets), 5.0)
            .expect("finite");
        assert_box(&unsnapped.geometries[0].1, 53.0, 50.0, 100.0, 100.0);
    }

    #[test]
    fn a_group_scales_from_the_far_side() {
        // The bottom-right corner, dragged 300 right: twice as wide, so twice as tall.
        let gesture = pair(
            GroupKind::Scale { x: 1, y: 1 },
            Point { x: 300.0, y: 100.0 },
        );
        let update = gesture
            .update(Point { x: 600.0, y: 100.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&update.geometries[0].1, 100.0, 100.0, 200.0, 200.0);
        assert_box(&update.geometries[1].1, 500.0, 100.0, 200.0, 200.0);
        // With Ctrl it stretches along x only.
        let stretched = gesture
            .update(Point { x: 600.0, y: 100.0 }, CTRL, None, 0.0)
            .expect("finite");
        assert_box(&stretched.geometries[1].1, 500.0, 50.0, 200.0, 100.0);
        // An edge keeps the aspect about the centre line, and the bounds stop at 20 px.
        let edge = pair(
            GroupKind::Scale { x: 0, y: 1 },
            Point { x: 150.0, y: 100.0 },
        );
        let tiny = edge
            .update(
                Point {
                    x: 150.0,
                    y: -500.0,
                },
                NONE,
                None,
                0.0,
            )
            .expect("finite");
        let shrunk = Bounds::around(tiny.geometries.iter().map(|(_, g)| g)).expect("items");
        assert!(close(shrunk.bottom - shrunk.top, MIN_GROUP_SIZE));
        assert!(close(shrunk.right - shrunk.left, 3.0 * MIN_GROUP_SIZE));
        assert!(close((shrunk.left + shrunk.right) / 2.0, 150.0));
    }

    #[test]
    fn a_group_rotates_about_its_centre() {
        // From straight above the centre (150, 50) to its right: a quarter turn.
        let gesture = pair(GroupKind::Rotate, Point { x: 150.0, y: -50.0 });
        let update = gesture
            .update(Point { x: 250.0, y: 50.0 }, NONE, None, 0.0)
            .expect("finite");
        assert_box(&update.geometries[0].1, 150.0, -50.0, 100.0, 100.0);
        assert!(close(update.geometries[0].1.rotation_deg.get(), 90.0));
        // Ctrl steps 45°: 30° rounds to 45°.
        let (s, c) = 30.0_f32.to_radians().sin_cos();
        let stepped = gesture
            .update(
                Point {
                    x: 150.0 + 100.0 * s,
                    y: 50.0 - 100.0 * c,
                },
                CTRL,
                None,
                0.0,
            )
            .expect("finite");
        assert!(close(stepped.geometries[1].1.rotation_deg.get(), 45.0));
    }

    #[test]
    fn handles_sit_outside_the_bounds() {
        let viewport = Viewport {
            scale: 0.5,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        let bounds = Bounds {
            left: 0.0,
            top: 0.0,
            right: 300.0,
            bottom: 100.0,
        };
        // 4 screen pixels out is 8 canvas pixels at half scale.
        assert_eq!(
            handle_point(bounds, Handle::Resize { x: 1, y: 1 }, viewport),
            Point { x: 308.0, y: 108.0 }
        );
        assert_eq!(
            handle_at(
                bounds,
                Point {
                    x: 150.0,
                    y: -8.0 - 70.0
                },
                viewport
            ),
            Some(Handle::Rotate)
        );
        assert_eq!(
            handle_at(bounds, Point { x: 150.0, y: 50.0 }, viewport),
            None
        );
    }

    #[test]
    fn a_marquee_selects_what_it_touches() {
        let a = geometry(50.0, 50.0, 100.0, 100.0, 0.0);
        let b = geometry(250.0, 50.0, 100.0, 100.0, 0.0);
        let (ia, ib): (ItemId, ItemId) = ("a".into(), "b".into());
        let boxes = [(&ia, &a), (&ib, &b)];
        assert_eq!(
            marquee(
                boxes.into_iter(),
                Point { x: 90.0, y: 90.0 },
                Point { x: 120.0, y: 120.0 }
            ),
            vec![ia.clone()]
        );
        assert_eq!(
            marquee(
                boxes.into_iter(),
                Point { x: 400.0, y: 0.0 },
                Point { x: 80.0, y: 10.0 }
            ),
            vec![ia.clone(), ib.clone()]
        );
        assert!(
            marquee(
                boxes.into_iter(),
                Point { x: 120.0, y: 0.0 },
                Point { x: 180.0, y: 99.0 }
            )
            .is_empty()
        );
    }
}
