//! Crop on the canvas (CV-08, ADR-0022): what an item's picture covers in its box, the
//! box and crop a crop handle's drag gives, and the uncropped extent the crop ghost
//! draws.
//!
//! The crop is in whole pixels of the source's own frame, so these need the source's
//! size (`source.sizes`, ADR-0022 decision 4). A drag moves one edge of the picture, as
//! the prototype's crop does: the box shrinks or grows on that side, the picture keeps
//! its scale, and the edge stops at the uncropped extent. The box becomes the picture's
//! rectangle, so a `fit` box that left room around its picture closes onto it, and a
//! `fill` box's clipped margins count as crop.
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0022-item-transform-and-canvas.md

use crate::canvas::transform::{MIN_SIZE, Point, from_local, rotate, with_box};
use crate::store::{Collection, Sizes};
use jstream_control::protocol::{Crop, Edit, ItemGeometry, ItemId, SceneId};

/// Four amounts, one per side of the box: its own left, top, right and bottom, which a
/// flip swaps with the source's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sides {
    /// The box's left side.
    pub left: f32,
    /// The box's top side.
    pub top: f32,
    /// The box's right side.
    pub right: f32,
    /// The box's bottom side.
    pub bottom: f32,
}

/// What an item's picture covers, in its box's local frame (from the box's centre, before
/// rotation), and the source pixels cut from each side of the box to get it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picture {
    /// The picture's edges, in canvas pixels from the box's centre.
    pub edges: Sides,
    /// Canvas pixels per source pixel, across.
    pub scale_x: f32,
    /// Canvas pixels per source pixel, down.
    pub scale_y: f32,
    /// The source pixels cut on each side of the box: the crop, plus what a `fill` box
    /// clips. Not whole numbers until [`Picture::settled`].
    pub cut: Sides,
    /// The source's size, in pixels.
    pub source: (u32, u32),
    /// Whether the box mirrors the source across (`flip_h`).
    flip_h: bool,
    /// Whether the box mirrors the source down (`flip_v`).
    flip_v: bool,
}

impl Picture {
    /// The picture `geometry` draws of a `source` sized source cropped by `crop`, or
    /// `None` when the crop leaves nothing.
    pub fn of(geometry: &ItemGeometry, source: (u32, u32), crop: Crop) -> Option<Picture> {
        let content = geometry.content(source.0, source.1, crop).ok()??;
        let cropped_w = source.0.checked_sub(crop.left)?.checked_sub(crop.right)?;
        let cropped_h = source.1.checked_sub(crop.top)?.checked_sub(crop.bottom)?;
        if cropped_w == 0 || cropped_h == 0 {
            return None;
        }
        let scale_x = content.width / cropped_w as f32;
        let scale_y = content.height / cropped_h as f32;
        let (hw, hh) = (
            geometry.size.width.get() * 0.5,
            geometry.size.height.get() * 0.5,
        );
        let drawn = Sides {
            left: content.left,
            top: content.top,
            right: content.left + content.width,
            bottom: content.top + content.height,
        };
        // A `fill` picture is clipped to the box.
        let edges = Sides {
            left: drawn.left.max(-hw),
            top: drawn.top.max(-hh),
            right: drawn.right.min(hw),
            bottom: drawn.bottom.min(hh),
        };
        let (left, right) = if geometry.flip_h {
            (crop.right, crop.left)
        } else {
            (crop.left, crop.right)
        };
        let (top, bottom) = if geometry.flip_v {
            (crop.bottom, crop.top)
        } else {
            (crop.top, crop.bottom)
        };
        let cut = Sides {
            left: left as f32 + (edges.left - drawn.left) / scale_x,
            top: top as f32 + (edges.top - drawn.top) / scale_y,
            right: right as f32 + (drawn.right - edges.right) / scale_x,
            bottom: bottom as f32 + (drawn.bottom - edges.bottom) / scale_y,
        };
        Some(Picture {
            edges,
            scale_x,
            scale_y,
            cut,
            source,
            flip_h: geometry.flip_h,
            flip_v: geometry.flip_v,
        })
    }

    /// The uncropped picture's edges, in the box's local frame: what the crop ghost
    /// outlines, and how far a crop handle can go.
    pub fn extent(&self) -> Sides {
        Sides {
            left: self.edges.left - self.cut.left * self.scale_x,
            top: self.edges.top - self.cut.top * self.scale_y,
            right: self.edges.right + self.cut.right * self.scale_x,
            bottom: self.edges.bottom + self.cut.bottom * self.scale_y,
        }
    }

    /// The same uncropped extent cut by `cut` instead, its edges moved to match.
    fn cut_to(&self, cut: Sides) -> Picture {
        let extent = self.extent();
        Picture {
            edges: Sides {
                left: extent.left + cut.left * self.scale_x,
                top: extent.top + cut.top * self.scale_y,
                right: extent.right - cut.right * self.scale_x,
                bottom: extent.bottom - cut.bottom * self.scale_y,
            },
            cut,
            ..*self
        }
    }

    /// The picture with every cut rounded to whole source pixels, each side kept to
    /// leave at least one, and its edges moved to match.
    pub fn settled(&self) -> Picture {
        let (w, h) = (self.source.0 as f32, self.source.1 as f32);
        let left = self.cut.left.round().clamp(0.0, w - 1.0);
        let right = self.cut.right.round().clamp(0.0, w - 1.0 - left);
        let top = self.cut.top.round().clamp(0.0, h - 1.0);
        let bottom = self.cut.bottom.round().clamp(0.0, h - 1.0 - top);
        self.cut_to(Sides {
            left,
            top,
            right,
            bottom,
        })
    }

    /// The crop the cuts make, in the source's own sides, rounded to whole pixels.
    pub fn crop(&self) -> Crop {
        let whole = |n: f32| n.round().max(0.0) as u32;
        let (left, right) = if self.flip_h {
            (self.cut.right, self.cut.left)
        } else {
            (self.cut.left, self.cut.right)
        };
        let (top, bottom) = if self.flip_v {
            (self.cut.bottom, self.cut.top)
        } else {
            (self.cut.top, self.cut.bottom)
        };
        Crop {
            left: whole(left),
            top: whole(top),
            right: whole(right),
            bottom: whole(bottom),
        }
    }

    /// `base`'s box moved and resized onto `edges`, keeping its rotation, fit mode,
    /// flips and 3D fields.
    fn boxed(base: &ItemGeometry, edges: Sides) -> Option<ItemGeometry> {
        let middle = from_local(
            base,
            (edges.left + edges.right) * 0.5,
            (edges.top + edges.bottom) * 0.5,
        );
        with_box(
            base,
            middle.x,
            middle.y,
            edges.right - edges.left,
            edges.bottom - edges.top,
            base.rotation_deg.get(),
        )
    }

    /// `base`'s box moved and resized onto the picture: where crop mode draws its
    /// handles.
    pub fn geometry(&self, base: &ItemGeometry) -> Option<ItemGeometry> {
        Self::boxed(base, self.edges)
    }

    /// `base`'s box moved and resized onto the uncropped extent: what Clear crop leaves.
    pub fn uncropped(&self, base: &ItemGeometry) -> Option<ItemGeometry> {
        Self::boxed(base, self.extent())
    }

    /// The uncropped extent's four corners on the canvas, top-left first and clockwise:
    /// the crop ghost.
    pub fn ghost(&self, base: &ItemGeometry) -> [Point; 4] {
        let e = self.extent();
        [
            from_local(base, e.left, e.top),
            from_local(base, e.right, e.top),
            from_local(base, e.right, e.bottom),
            from_local(base, e.left, e.bottom),
        ]
    }
}

/// One drag on a crop handle of one item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropGesture {
    /// The handle's edge on x: -1 for the left, 1 for the right, 0 for neither.
    pub x: i8,
    /// The handle's edge on y: -1 for the top, 1 for the bottom, 0 for neither.
    pub y: i8,
    /// The item's geometry when the drag started.
    pub start: ItemGeometry,
    /// Its picture when the drag started.
    pub picture: Picture,
    /// The canvas point the drag started at.
    pub origin: Point,
}

impl CropGesture {
    /// The box and crop for the pointer at `pointer`. The dragged edges follow the
    /// pointer's movement, within the uncropped extent, and leave the picture at least
    /// [`MIN_SIZE`] canvas pixels (or what it had, if less). `None` only if the
    /// arithmetic left a non-finite number.
    pub fn update(&self, pointer: Point) -> Option<(ItemGeometry, Crop)> {
        let (dx, dy) = rotate(
            pointer.x - self.origin.x,
            pointer.y - self.origin.y,
            -self.start.rotation_deg.get(),
        );
        let start = self.picture.edges;
        let extent = self.picture.extent();
        let (sx, sy) = (self.picture.scale_x, self.picture.scale_y);
        let keep_x = MIN_SIZE.min(start.right - start.left);
        let keep_y = MIN_SIZE.min(start.bottom - start.top);
        let mut cut = self.picture.cut;
        match self.x {
            -1 => {
                let left = (start.left + dx).clamp(extent.left, start.right - keep_x);
                cut.left = (left - extent.left) / sx;
            }
            1 => {
                let right = (start.right + dx).clamp(start.left + keep_x, extent.right);
                cut.right = (extent.right - right) / sx;
            }
            _ => {}
        }
        match self.y {
            -1 => {
                let top = (start.top + dy).clamp(extent.top, start.bottom - keep_y);
                cut.top = (top - extent.top) / sy;
            }
            1 => {
                let bottom = (start.bottom + dy).clamp(start.top + keep_y, extent.bottom);
                cut.bottom = (extent.bottom - bottom) / sy;
            }
            _ => {}
        }
        let picture = self.picture.cut_to(cut).settled();
        Some((picture.geometry(&self.start)?, picture.crop()))
    }
}

/// The batch Clear crop sends for `items` of `scene_id`: no crop for each cropped one,
/// and, as the prototype restores the full extent, its uncropped extent as its box
/// while its source's size is known.
pub fn clear(
    collection: &Collection,
    sizes: &Sizes,
    scene_id: &SceneId,
    items: &[ItemId],
) -> Vec<Edit> {
    let Some(scene) = collection.scene(scene_id) else {
        return Vec::new();
    };
    let cropped: Vec<_> = items
        .iter()
        .filter_map(|id| scene.items.iter().find(|i| i.item_id == *id))
        .filter(|i| i.crop != Crop::NONE)
        .collect();
    let crops = cropped.iter().map(|i| Edit::ItemSetCrop {
        item_id: i.item_id.clone(),
        scene_id: Some(scene_id.clone()),
        crop: Crop::NONE,
    });
    let boxes = cropped.iter().filter_map(|i| {
        let geometry = i.geometry?;
        let size = collection.source_size(sizes, &i.source_id)?;
        Some(Edit::ItemSetGeometry {
            item_id: i.item_id.clone(),
            scene_id: Some(scene_id.clone()),
            geometry: Picture::of(&geometry, size, i.crop)?.uncropped(&geometry)?,
        })
    });
    crops.chain(boxes).collect()
}

/// The readout while cropping: `crop l / r / t / b`, in source pixels.
pub fn readout(crop: &Crop) -> String {
    format!(
        "crop {} / {} / {} / {}",
        crop.left, crop.right, crop.top, crop.bottom
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{FitMode, Size, Vec2};

    fn geometry(cx: f32, cy: f32, w: f32, h: f32, rotation: f32) -> ItemGeometry {
        let base = ItemGeometry::try_new(
            Vec2::new(cx, cy).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("valid");
        with_box(&base, cx, cy, w, h, rotation).expect("valid")
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-2
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

    fn crop(left: u32, top: u32, right: u32, bottom: u32) -> Crop {
        Crop {
            left,
            top,
            right,
            bottom,
        }
    }

    /// A 1920×1080 source at half size, 960×540 centred on (480, 270).
    fn half_size() -> ItemGeometry {
        geometry(480.0, 270.0, 960.0, 540.0, 0.0)
    }

    fn drag(start: ItemGeometry, current: Crop, x: i8, y: i8) -> CropGesture {
        let picture = Picture::of(&start, (1920, 1080), current).expect("a picture");
        CropGesture {
            x,
            y,
            start,
            picture,
            origin: Point { x: 0.0, y: 0.0 },
        }
    }

    #[test]
    fn a_picture_that_fills_its_box_cuts_only_its_crop() {
        let start = half_size();
        let picture = Picture::of(&start, (1920, 1080), Crop::NONE).expect("a picture");
        assert!(close(picture.scale_x, 0.5) && close(picture.scale_y, 0.5));
        assert_eq!(picture.cut, Sides::default());
        assert!(close(picture.edges.left, -480.0) && close(picture.edges.bottom, 270.0));
        assert_eq!(picture.extent(), picture.edges);
    }

    #[test]
    fn dragging_the_left_edge_in_crops_the_left_and_keeps_the_scale() {
        let gesture = drag(half_size(), Crop::NONE, -1, 0);
        // 100 canvas pixels in at half size is 200 source pixels.
        let (g, c) = gesture.update(Point { x: 100.0, y: 0.0 }).expect("finite");
        assert_eq!(c, crop(200, 0, 0, 0));
        // The left edge moved to x = 100; the right stayed at 960.
        assert_box(&g, 530.0, 270.0, 860.0, 540.0);
        // The new box fits the cropped picture exactly, at the same scale.
        let after = Picture::of(&g, (1920, 1080), c).expect("a picture");
        assert!(close(after.scale_x, 0.5) && close(after.scale_y, 0.5));
    }

    #[test]
    fn an_edge_stops_at_the_uncropped_extent_and_before_the_other_edge() {
        // Cropped 200 on the right: the right edge can come back out by 100 canvas px.
        let start = geometry(430.0, 270.0, 860.0, 540.0, 0.0);
        let gesture = drag(start, crop(0, 0, 200, 0), 1, 0);
        let (g, c) = gesture.update(Point { x: 500.0, y: 0.0 }).expect("finite");
        assert_eq!(c, Crop::NONE);
        assert_box(&g, 480.0, 270.0, 960.0, 540.0);
        // Dragged far inward, it keeps the minimum width.
        let (g, c) = gesture
            .update(Point { x: -5000.0, y: 0.0 })
            .expect("finite");
        assert!(close(g.size.width.get(), MIN_SIZE));
        assert_eq!(c.right, 1900);
        assert!(
            close(g.center.x.get(), MIN_SIZE / 2.0),
            "the left edge stays"
        );
    }

    #[test]
    fn a_corner_crops_two_sides() {
        let gesture = drag(half_size(), Crop::NONE, 1, 1);
        let (g, c) = gesture
            .update(Point { x: -60.0, y: -40.0 })
            .expect("finite");
        assert_eq!(c, crop(0, 0, 120, 80));
        assert_box(&g, 450.0, 250.0, 900.0, 500.0);
    }

    #[test]
    fn a_rotated_box_crops_along_its_own_axes() {
        // Turned a quarter clockwise, the box's right edge faces down the canvas.
        let start = geometry(480.0, 270.0, 960.0, 540.0, 90.0);
        let gesture = drag(start, Crop::NONE, 1, 0);
        let (g, c) = gesture.update(Point { x: 0.0, y: -100.0 }).expect("finite");
        assert_eq!(c, crop(0, 0, 200, 0));
        // The box's centre moves 50 up the canvas, its local -x.
        assert_box(&g, 480.0, 220.0, 860.0, 540.0);
    }

    #[test]
    fn a_flipped_box_crops_the_source_side_it_shows() {
        let mut start = half_size();
        start.flip_h = true;
        let gesture = drag(start, Crop::NONE, -1, 0);
        let (_, c) = gesture.update(Point { x: 100.0, y: 0.0 }).expect("finite");
        assert_eq!(
            c,
            crop(0, 0, 200, 0),
            "the box's left shows the source's right"
        );
    }

    #[test]
    fn a_fit_box_with_room_closes_onto_its_picture() {
        // A 16:9 source in a square box: the picture is 960×540 in a 960×960 box.
        let start = geometry(480.0, 480.0, 960.0, 960.0, 0.0);
        let picture = Picture::of(&start, (1920, 1080), Crop::NONE).expect("a picture");
        assert!(close(picture.edges.top, -270.0) && close(picture.edges.bottom, 270.0));
        let handles = picture.geometry(&start).expect("finite");
        assert_box(&handles, 480.0, 480.0, 960.0, 540.0);
        let gesture = CropGesture {
            x: 0,
            y: -1,
            start,
            picture,
            origin: Point { x: 0.0, y: 0.0 },
        };
        let (g, c) = gesture.update(Point { x: 0.0, y: 27.0 }).expect("finite");
        assert_eq!(c, crop(0, 54, 0, 0));
        assert_box(&g, 480.0, 493.5, 960.0, 513.0);
    }

    #[test]
    fn a_fill_box_counts_what_it_clips_as_crop() {
        // A 16:9 source filling a square box: the picture is 1706.7 wide, clipped to 960.
        let mut start = geometry(480.0, 480.0, 960.0, 960.0, 0.0);
        start.fit_mode = FitMode::Fill;
        let picture = Picture::of(&start, (1920, 1080), Crop::NONE).expect("a picture");
        let clipped = (1920.0 - 1080.0) / 2.0;
        assert!(close(picture.cut.left, clipped) && close(picture.cut.right, clipped));
        assert_eq!(picture.cut.top, 0.0);
        assert!(close(
            picture.extent().left,
            -480.0 - clipped * picture.scale_x
        ));
        let gesture = CropGesture {
            x: 0,
            y: 1,
            start,
            picture,
            origin: Point { x: 0.0, y: 0.0 },
        };
        let (_, c) = gesture.update(Point { x: 0.0, y: -96.0 }).expect("finite");
        assert_eq!(c, crop(420, 0, 420, 108));
    }

    #[test]
    fn clearing_restores_the_uncropped_extent() {
        let start = geometry(530.0, 270.0, 860.0, 540.0, 0.0);
        let picture = Picture::of(&start, (1920, 1080), crop(200, 0, 0, 0)).expect("a picture");
        let full = picture.uncropped(&start).expect("finite");
        assert_box(&full, 480.0, 270.0, 960.0, 540.0);
        let ghost = picture.ghost(&start);
        assert!(close(ghost[0].x, 0.0) && close(ghost[2].x, 960.0));
    }

    #[test]
    fn a_crop_that_leaves_nothing_has_no_picture() {
        assert_eq!(
            Picture::of(&half_size(), (1920, 1080), crop(960, 0, 960, 0)),
            None
        );
        assert_eq!(
            Picture::of(&half_size(), (1920, 1080), crop(4000, 0, 0, 0)),
            None
        );
    }

    #[test]
    fn the_readout_lists_left_right_top_bottom() {
        assert_eq!(readout(&crop(24, 2, 30, 4)), "crop 24 / 30 / 2 / 4");
    }
}
