//! Snapping: while a box is dragged, its edges and centre are pulled onto the canvas's
//! edges and centre lines and onto the other items' edges and centres, and the line it
//! snapped to is drawn as a guide.
//!
//! A rotated box snaps by its axis-aligned bounds, which is what the eye lines up.
//!
//! Docs: crates/jstream-ui/README.md#canvas

use jstream_control::protocol::{Canvas, ItemGeometry};

/// How close an edge or centre snaps, in screen pixels.
pub const SNAP_DISTANCE: f32 = 6.0;

/// A snap guide: a line across the whole canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Guide {
    /// A vertical line at this x.
    Vertical(f32),
    /// A horizontal line at this y.
    Horizontal(f32),
}

/// A box's axis-aligned bounds on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// Smallest x.
    pub left: f32,
    /// Smallest y.
    pub top: f32,
    /// Largest x.
    pub right: f32,
    /// Largest y.
    pub bottom: f32,
}

impl Bounds {
    /// The bounds of a box, rotation included.
    pub fn of(geometry: &ItemGeometry) -> Bounds {
        let (sin, cos) = geometry.rotation_deg.get().to_radians().sin_cos();
        let hw = geometry.size.width.get() * 0.5;
        let hh = geometry.size.height.get() * 0.5;
        let ex = hw * cos.abs() + hh * sin.abs();
        let ey = hw * sin.abs() + hh * cos.abs();
        let cx = geometry.center.x.get();
        let cy = geometry.center.y.get();
        Bounds {
            left: cx - ex,
            top: cy - ey,
            right: cx + ex,
            bottom: cy + ey,
        }
    }

    fn xs(self) -> [f32; 3] {
        [self.left, (self.left + self.right) * 0.5, self.right]
    }

    fn ys(self) -> [f32; 3] {
        [self.top, (self.top + self.bottom) * 0.5, self.bottom]
    }
}

/// The lines a dragged box snaps to: the canvas's edges and centre, and the edges and
/// centres of the scene's other boxes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapTargets {
    xs: Vec<f32>,
    ys: Vec<f32>,
}

impl SnapTargets {
    /// The targets for a drag on `canvas` among `others` (every box but the dragged one).
    pub fn new<'a>(canvas: Canvas, others: impl Iterator<Item = &'a ItemGeometry>) -> Self {
        let width = canvas.width as f32;
        let height = canvas.height as f32;
        let mut xs = vec![0.0, width * 0.5, width];
        let mut ys = vec![0.0, height * 0.5, height];
        for other in others {
            let bounds = Bounds::of(other);
            xs.extend(bounds.xs());
            ys.extend(bounds.ys());
        }
        SnapTargets { xs, ys }
    }

    /// The x target nearest `x` within `tolerance`, if any.
    pub fn snap_x(&self, x: f32, tolerance: f32) -> Option<f32> {
        nearest(&self.xs, &[x], tolerance).map(|(_, target)| target)
    }

    /// The y target nearest `y` within `tolerance`, if any.
    pub fn snap_y(&self, y: f32, tolerance: f32) -> Option<f32> {
        nearest(&self.ys, &[y], tolerance).map(|(_, target)| target)
    }

    /// How far to shift a moved box so its nearest edge or centre lands on a target, on
    /// each axis (0 where nothing is within `tolerance`), and the guides it lands on.
    pub fn snap_move(&self, geometry: &ItemGeometry, tolerance: f32) -> (f32, f32, Vec<Guide>) {
        let bounds = Bounds::of(geometry);
        let dx = nearest(&self.xs, &bounds.xs(), tolerance).map_or(0.0, |(delta, _)| delta);
        let dy = nearest(&self.ys, &bounds.ys(), tolerance).map_or(0.0, |(delta, _)| delta);
        let snapped = Bounds {
            left: bounds.left + dx,
            top: bounds.top + dy,
            right: bounds.right + dx,
            bottom: bounds.bottom + dy,
        };
        (dx, dy, self.guides_for(snapped))
    }

    /// The guides a box's edges and centre lie on.
    pub fn guides_on(&self, geometry: &ItemGeometry) -> Vec<Guide> {
        self.guides_for(Bounds::of(geometry))
    }

    fn guides_for(&self, bounds: Bounds) -> Vec<Guide> {
        const ON: f32 = 0.01;
        let mut guides = Vec::new();
        for x in bounds.xs() {
            if let Some(&target) = self.xs.iter().find(|t| (*t - x).abs() < ON)
                && !guides.contains(&Guide::Vertical(target))
            {
                guides.push(Guide::Vertical(target));
            }
        }
        for y in bounds.ys() {
            if let Some(&target) = self.ys.iter().find(|t| (*t - y).abs() < ON)
                && !guides.contains(&Guide::Horizontal(target))
            {
                guides.push(Guide::Horizontal(target));
            }
        }
        guides
    }
}

/// The smallest move that puts one of `values` on one of `targets`, within `tolerance`:
/// the move and the target.
fn nearest(targets: &[f32], values: &[f32], tolerance: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for &value in values {
        for &target in targets {
            let delta = target - value;
            if delta.abs() <= tolerance && best.is_none_or(|(b, _)| delta.abs() < b.abs()) {
                best = Some((delta, target));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Finite32, Size, Vec2};

    fn geometry(cx: f32, cy: f32, w: f32, h: f32, rotation: f32) -> ItemGeometry {
        let mut g = ItemGeometry::try_new(
            Vec2::new(cx, cy).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("valid");
        g.rotation_deg = Finite32::new(rotation).expect("finite");
        g
    }

    #[test]
    fn a_rotated_box_snaps_by_its_bounds() {
        let bounds = Bounds::of(&geometry(100.0, 100.0, 200.0, 20.0, 90.0));
        assert!((bounds.left - 90.0).abs() < 1e-3 && (bounds.top - 0.0).abs() < 1e-3);
        assert!((bounds.right - 110.0).abs() < 1e-3 && (bounds.bottom - 200.0).abs() < 1e-3);
    }

    #[test]
    fn a_move_snaps_to_the_nearest_line_and_draws_it() {
        let other = geometry(500.0, 500.0, 100.0, 100.0, 0.0);
        let targets = SnapTargets::new(Canvas::DEFAULT, [&other].into_iter());
        // Left edge at 553, 3 px right of the other box's right edge (550); its top at
        // 453, 3 px below the other's top (450). Snapped, its bottom also lies on the
        // other's centre line, so that guide shows too.
        let moved = geometry(578.0, 478.0, 50.0, 50.0, 0.0);
        let (dx, dy, guides) = targets.snap_move(&moved, 5.0);
        assert_eq!((dx, dy), (-3.0, -3.0));
        assert_eq!(
            guides,
            vec![
                Guide::Vertical(550.0),
                Guide::Horizontal(450.0),
                Guide::Horizontal(500.0)
            ]
        );
        // Out of reach on both axes: no move and no guide.
        let far = geometry(700.0, 700.0, 50.0, 50.0, 0.0);
        assert_eq!(targets.snap_move(&far, 5.0), (0.0, 0.0, Vec::new()));
    }

    #[test]
    fn the_canvas_centre_is_a_target() {
        let targets = SnapTargets::new(Canvas::DEFAULT, std::iter::empty());
        let nearly = geometry(962.0, 538.0, 100.0, 100.0, 0.0);
        let (dx, dy, guides) = targets.snap_move(&nearly, 5.0);
        assert_eq!((dx, dy), (-2.0, 2.0));
        assert_eq!(
            guides,
            vec![Guide::Vertical(960.0), Guide::Horizontal(540.0)]
        );
        assert_eq!(targets.snap_x(1917.0, 5.0), Some(1920.0));
        assert_eq!(targets.snap_y(30.0, 5.0), None);
    }
}
