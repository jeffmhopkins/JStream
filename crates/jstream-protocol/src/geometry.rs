//! Item geometry: the box, how a cropped source sits in it, and the legacy 0.3
//! evaluation (ADR-0022, docs/adr/0022-item-transform-and-canvas.md).
//!
//! Canvas coordinates have x to the right and y down. The box's local frame has its
//! origin at the box centre, the same axis directions, and z toward the viewer.
//!
//! Docs: docs/adr/0022-item-transform-and-canvas.md

use crate::command::{Crop, ItemTransform, Vec2};
use crate::finite::Finite32;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How the cropped source fills the item's box.
///
/// The three values are the complete set. A new one is a string-enum addition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FitMode {
    /// Fill the box exactly, ignoring the source's aspect ratio.
    Stretch,
    /// The largest rectangle of the source's aspect ratio that fits inside the box,
    /// centred. The rest of the box stays transparent.
    #[default]
    Fit,
    /// The smallest rectangle of the source's aspect ratio that covers the box,
    /// centred. The part outside the box is clipped by the compositor.
    Fill,
}

/// A width and height, in canvas pixels.
///
/// Both components are [`Finite32`]. A non-positive component is rejected by
/// [`ItemGeometry::validate`], not stored as a flip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Size {
    /// Horizontal extent. Must be greater than zero for a placement to be valid.
    pub width: Finite32,
    /// Vertical extent. Must be greater than zero for a placement to be valid.
    pub height: Finite32,
}

impl Size {
    /// A size from two finite, positive components.
    ///
    /// # Errors
    /// [`GeometryError::NotFinite`] if either component is NaN or infinite, and
    /// [`GeometryError::NonPositiveSize`] if either is zero or negative.
    pub fn try_new(width: f32, height: f32) -> Result<Self, GeometryError> {
        let width = Finite32::new(width).ok_or(GeometryError::NotFinite)?;
        let height = Finite32::new(height).ok_or(GeometryError::NotFinite)?;
        let size = Size { width, height };
        size.check()?;
        Ok(size)
    }

    fn check(self) -> Result<(), GeometryError> {
        if self.width.get() > 0.0 && self.height.get() > 0.0 {
            Ok(())
        } else {
            Err(GeometryError::NonPositiveSize)
        }
    }
}

/// Where a scene item sits on the canvas: its box, the fit of the source inside that
/// box, and the 3D placement ADR-0022 defines.
///
/// `center`, `size` and `rotation_deg` alone define the box, so selection handles need
/// no source size. The 3D fields default to neutral (no tilt, turn or depth, and no
/// perspective). The formula below still evaluates a non-neutral value; drawing it is
/// the compositor's M3 work, and until then the engine rejects it. [`Self::is_neutral_3d`]
/// is that check. A negative `size` is rejected, not read as a flip.
///
/// Every number is a [`Finite32`]. On the wire, omitted `fit_mode` and angles mean the
/// defaults below, and omitted `perspective_px` means orthographic.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ItemGeometry {
    /// Centre of the box, in canvas pixels.
    pub center: Vec2,
    /// Width and height of the box, in canvas pixels. Each must be greater than zero.
    pub size: Size,
    /// How the cropped source fills the box. Default [`FitMode::Fit`].
    #[serde(default)]
    pub fit_mode: FitMode,
    /// Clockwise rotation in the canvas plane, in degrees, about [`Self::center`].
    /// Default 0.
    #[serde(default)]
    pub rotation_deg: Finite32,
    /// Rotation about the box's horizontal axis through the centre, in degrees.
    /// Positive moves the top edge away from the viewer. Default 0.
    #[serde(default)]
    pub tilt_deg: Finite32,
    /// Rotation about the box's vertical axis through the centre, in degrees.
    /// Positive moves the right edge away from the viewer. Default 0.
    #[serde(default)]
    pub turn_deg: Finite32,
    /// Offset along the viewing axis, in pixels. Positive is toward the viewer.
    /// Default 0.
    #[serde(default)]
    pub depth_px: Finite32,
    /// Distance from the viewer to the canvas plane, for this item only. Absent means
    /// orthographic. When present it must be greater than zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perspective_px: Option<Finite32>,
}

/// The collection's canvas: every scene's coordinate space, in whole pixels.
///
/// x points right and y down, with (0, 0) at the top-left corner. A `scene` source's
/// native size is the canvas size, and a mix renders the canvas fitted inside its
/// resolution. Missing (from a server older than 0.10) parses as the 1920×1080 default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Canvas {
    /// Width in pixels, greater than zero.
    pub width: u32,
    /// Height in pixels, greater than zero.
    pub height: u32,
}

impl Canvas {
    /// ADR-0022's default canvas, 1920×1080.
    pub const DEFAULT: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };
}

impl Default for Canvas {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Which of the two forms an item's stored placement has (ADR-0022 decision 5).
///
/// The two values are the complete set until 1.0, which removes `legacy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlacementForm {
    /// A 0.3 `ItemTransform`, evaluated every frame against the source's current size.
    /// What a server older than 0.10 stores, so a missing field parses as this.
    #[default]
    Legacy,
    /// An [`ItemGeometry`]: a box on the canvas.
    Geometry,
}

/// Why a geometry could not be placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GeometryError {
    /// `size.width` or `size.height` is zero or negative. A negative size is not a flip.
    #[error("item size must be greater than zero")]
    NonPositiveSize,
    /// `perspective_px` is present and is zero or negative.
    #[error("perspective_px must be greater than zero when present")]
    NonPositivePerspective,
    /// An input or a computed canvas coordinate is NaN or infinite.
    #[error("a placement coordinate is not finite")]
    NotFinite,
}

/// A point in canvas pixels, x right and y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasPoint {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
}

/// One corner of a projected box.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Placed {
    /// The corner is in front of the viewer.
    At(CanvasPoint),
    /// `z` is at or beyond `perspective_px`, so the corner is behind the viewer.
    BehindViewer,
}

/// The box's four corners after ADR-0022's placement formula
/// (`docs/adr/0022-item-transform-and-canvas.md`).
///
/// Order is the unrotated box, y down: top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectedQuad {
    /// The placed corners, in the order above.
    pub corners: [Placed; 4],
}

/// Where the cropped source sits in the box's local frame, before rotation and 3D.
///
/// The origin is the box centre, x right, y down. `left` and `top` are the edges of
/// the content rectangle. For [`FitMode::Fill`] the rectangle may extend past the box;
/// the compositor clips it. `u0, v0` and `u1, v1` are the cropped source's texture
/// coordinates, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentRect {
    /// Left edge, local x.
    pub left: f32,
    /// Top edge, local y.
    pub top: f32,
    /// Width of the content rectangle.
    pub width: f32,
    /// Height of the content rectangle.
    pub height: f32,
    /// Left texture coordinate of the crop, in `0..=1` of the source.
    pub u0: f32,
    /// Top texture coordinate of the crop.
    pub v0: f32,
    /// Right texture coordinate of the crop.
    pub u1: f32,
    /// Bottom texture coordinate of the crop.
    pub v1: f32,
}

/// The box a legacy 0.3 transform draws, plus whether a negative scale flipped an axis.
///
/// [`ItemGeometry`] has no flip field. Projecting `geometry` matches the drawn outline
/// and, when either flag is set, not the reflected corner order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyBox {
    /// `fit_mode` is [`FitMode::Stretch`] and the 3D fields are neutral. Size components
    /// are the absolute scaled crop.
    pub geometry: ItemGeometry,
    /// `scale.x` was negative.
    pub flip_x: bool,
    /// `scale.y` was negative.
    pub flip_y: bool,
}

/// The quad a legacy 0.3 transform draws, including a negative scale's reflection.
///
/// Corners are canvas pixels in the same order as [`ProjectedQuad`]: the unrotated
/// destination's top-left, top-right, bottom-right and bottom-left, rotated clockwise
/// about the top-left. A negative destination size swaps that edge across the pivot.
/// `uvs` are `[u, v]` in that same corner order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyQuad {
    /// Canvas-pixel corners.
    pub corners: [CanvasPoint; 4],
    /// Texture coordinates for `corners`.
    pub uvs: [[f32; 2]; 4],
}

impl ItemGeometry {
    /// A box at `center` with `size`, fit mode [`FitMode::Fit`], and neutral 3D.
    ///
    /// # Errors
    /// [`GeometryError::NonPositiveSize`] when `size` is not positive.
    pub fn try_new(center: Vec2, size: Size) -> Result<Self, GeometryError> {
        let geometry = ItemGeometry {
            center,
            size,
            fit_mode: FitMode::Fit,
            rotation_deg: Finite32::ZERO,
            tilt_deg: Finite32::ZERO,
            turn_deg: Finite32::ZERO,
            depth_px: Finite32::ZERO,
            perspective_px: None,
        };
        geometry.validate()?;
        Ok(geometry)
    }

    /// The default placement from protocol 1.0: the box is the whole canvas, centred,
    /// with [`FitMode::Fit`] and neutral 3D.
    ///
    /// `width` and `height` are the canvas in whole pixels.
    ///
    /// # Errors
    /// [`GeometryError::NonPositiveSize`] when either dimension is zero.
    pub fn covering_canvas(width: u32, height: u32) -> Result<Self, GeometryError> {
        let width = width as f32;
        let height = height as f32;
        let center = Vec2::new(width * 0.5, height * 0.5).ok_or(GeometryError::NotFinite)?;
        Self::try_new(center, Size::try_new(width, height)?)
    }

    /// Whether tilt, turn, depth and perspective are all at their neutral defaults.
    ///
    /// The engine rejects a geometry for which this is false until the compositor
    /// draws 3D. The placement functions still evaluate those fields.
    pub fn is_neutral_3d(self) -> bool {
        self.tilt_deg.get() == 0.0
            && self.turn_deg.get() == 0.0
            && self.depth_px.get() == 0.0
            && self.perspective_px.is_none()
    }

    /// Whether `size` is positive and `perspective_px`, if present, is positive.
    ///
    /// # Errors
    /// [`GeometryError::NonPositiveSize`] or [`GeometryError::NonPositivePerspective`].
    pub fn validate(self) -> Result<(), GeometryError> {
        self.size.check()?;
        if self
            .perspective_px
            .is_some_and(|perspective| perspective.get() <= 0.0)
        {
            return Err(GeometryError::NonPositivePerspective);
        }
        Ok(())
    }

    /// Place one local-frame point. `local_x` and `local_y` are pixels from the box
    /// centre, y down, before rotation.
    ///
    /// The point is rotated about z by `rotation_deg`, then about y by `turn_deg`,
    /// then about x by `tilt_deg` (CSS `rotateZ`, `rotateY`, `rotateX` in a y-down
    /// frame). `depth_px` is added to z. If `perspective_px` is `p`, x and y are
    /// scaled by `p / (p − z)`; a point with `z ≥ p` is [`Placed::BehindViewer`].
    /// `center` is added last.
    ///
    /// # Errors
    /// [`GeometryError::NonPositiveSize`], [`GeometryError::NonPositivePerspective`],
    /// or [`GeometryError::NotFinite`] when an input or the canvas point is not finite.
    pub fn place(self, local_x: f32, local_y: f32) -> Result<Placed, GeometryError> {
        self.validate()?;
        if !local_x.is_finite() || !local_y.is_finite() {
            return Err(GeometryError::NotFinite);
        }
        let (x, y, z) = rotate_z(local_x, local_y, 0.0, self.rotation_deg.get());
        let (x, y, z) = rotate_y(x, y, z, self.turn_deg.get());
        let (x, y, z) = rotate_x(x, y, z, self.tilt_deg.get());
        let z = z + self.depth_px.get();
        let (x, y) = if let Some(perspective) = self.perspective_px {
            let p = perspective.get();
            if z >= p {
                return Ok(Placed::BehindViewer);
            }
            let scale = p / (p - z);
            (x * scale, y * scale)
        } else {
            (x, y)
        };
        let x = self.center.x.get() + x;
        let y = self.center.y.get() + y;
        if !x.is_finite() || !y.is_finite() {
            return Err(GeometryError::NotFinite);
        }
        Ok(Placed::At(CanvasPoint { x, y }))
    }

    /// The box's projected corners. Needs no source size.
    ///
    /// # Errors
    /// The same as [`Self::place`].
    pub fn project(self) -> Result<ProjectedQuad, GeometryError> {
        let half_w = self.size.width.get() * 0.5;
        let half_h = self.size.height.get() * 0.5;
        // validate runs inside place; a non-positive size fails before a corner is used.
        Ok(ProjectedQuad {
            corners: [
                self.place(-half_w, -half_h)?,
                self.place(half_w, -half_h)?,
                self.place(half_w, half_h)?,
                self.place(-half_w, half_h)?,
            ],
        })
    }

    /// The cropped source's rectangle in the box's local frame, or `Ok(None)` when the
    /// crop leaves nothing to draw.
    ///
    /// Crop is in whole source pixels and is applied before fitting. The rectangle is
    /// centred in the box for every fit mode. It does not depend on rotation or the 3D
    /// fields: those are applied by [`Self::place`] to the rectangle's corners.
    ///
    /// # Errors
    /// The same as [`Self::validate`], or [`GeometryError::NotFinite`] if the fitted
    /// size is not finite.
    pub fn content(
        self,
        source_width: u32,
        source_height: u32,
        crop: Crop,
    ) -> Result<Option<ContentRect>, GeometryError> {
        self.validate()?;
        let Some((cropped_w, cropped_h)) = cropped_size(source_width, source_height, crop) else {
            return Ok(None);
        };
        let box_w = self.size.width.get();
        let box_h = self.size.height.get();
        let (width, height) = match self.fit_mode {
            FitMode::Stretch => (box_w, box_h),
            FitMode::Fit => fitted(cropped_w, cropped_h, box_w, box_h, true),
            FitMode::Fill => fitted(cropped_w, cropped_h, box_w, box_h, false),
        };
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(GeometryError::NotFinite);
        }
        let source_w = source_width as f32;
        let source_h = source_height as f32;
        Ok(Some(ContentRect {
            left: -width * 0.5,
            top: -height * 0.5,
            width,
            height,
            u0: crop.left as f32 / source_w,
            v0: crop.top as f32 / source_h,
            u1: (crop.left + cropped_w) as f32 / source_w,
            v1: (crop.top + cropped_h) as f32 / source_h,
        }))
    }

    /// The deprecated 0.3 view of this geometry, which `ItemInfo.transform` reports for
    /// a geometry item: `position` is the box's top-left corner after rotation about
    /// `center`, `rotation_deg` is the box's rotation, and `scale` is (1, 1), as if the
    /// source were exactly the box's size. It depends on no source size. The 3D fields
    /// and the fit mode are not in it.
    ///
    /// # Errors
    /// [`GeometryError::NotFinite`] when the corner is not finite.
    pub fn transform_view(self) -> Result<ItemTransform, GeometryError> {
        let half_w = self.size.width.get() * 0.5;
        let half_h = self.size.height.get() * 0.5;
        let (dx, dy, _) = rotate_z(-half_w, -half_h, 0.0, self.rotation_deg.get());
        let position = Vec2::new(self.center.x.get() + dx, self.center.y.get() + dy)
            .ok_or(GeometryError::NotFinite)?;
        Ok(ItemTransform {
            position,
            scale: ItemTransform::IDENTITY.scale,
            rotation_deg: self.rotation_deg,
        })
    }

    /// The box a 0.3 transform draws for this source, or `Ok(None)` when the crop or
    /// the scale leaves nothing.
    ///
    /// The cropped source is scaled, its top-left is `position`, and it rotates
    /// clockwise about that corner. The returned geometry has [`FitMode::Stretch`] and
    /// neutral 3D, so a non-uniform scale stays exact. A negative scale is not refused:
    /// there is no flip field yet, so the size is the absolute value and the matching
    /// flag records that the reflection is not stored. Projecting the box matches the
    /// outline, not the reflected corner order. [`legacy_quad`] keeps that reflection.
    ///
    /// # Errors
    /// [`GeometryError::NotFinite`] when the scaled size or the centre is not finite.
    pub fn from_legacy(
        transform: ItemTransform,
        crop: Crop,
        source_width: u32,
        source_height: u32,
    ) -> Result<Option<LegacyBox>, GeometryError> {
        let Some((dest_w, dest_h)) = scaled_crop(transform, crop, source_width, source_height)?
        else {
            return Ok(None);
        };
        let rotation = transform.rotation_deg.get();
        let (mid_x, mid_y, _) = rotate_z(dest_w * 0.5, dest_h * 0.5, 0.0, rotation);
        let center = Vec2::new(
            transform.position.x.get() + mid_x,
            transform.position.y.get() + mid_y,
        )
        .ok_or(GeometryError::NotFinite)?;
        let geometry = ItemGeometry {
            center,
            size: Size::try_new(dest_w.abs(), dest_h.abs())?,
            fit_mode: FitMode::Stretch,
            rotation_deg: transform.rotation_deg,
            tilt_deg: Finite32::ZERO,
            turn_deg: Finite32::ZERO,
            depth_px: Finite32::ZERO,
            perspective_px: None,
        };
        Ok(Some(LegacyBox {
            geometry,
            flip_x: dest_w < 0.0,
            flip_y: dest_h < 0.0,
        }))
    }
}

/// The quad a 0.3 transform draws, or `Ok(None)` when the crop or a zero scale leaves
/// nothing.
///
/// This is the evaluation the compositor uses for a legacy placement: crop, then
/// scale (a negative scale reflects), top-left at `position`, clockwise about that
/// corner. It matches [`ItemGeometry::from_legacy`]'s projected corners when neither
/// scale is negative.
///
/// # Errors
/// [`GeometryError::NotFinite`] when a corner is not finite.
pub fn legacy_quad(
    transform: ItemTransform,
    crop: Crop,
    source_width: u32,
    source_height: u32,
) -> Result<Option<LegacyQuad>, GeometryError> {
    let Some((dest_w, dest_h)) = scaled_crop(transform, crop, source_width, source_height)? else {
        return Ok(None);
    };
    let (cropped_w, cropped_h) = cropped_size(source_width, source_height, crop)
        .expect("invariant: a destination size means the crop left pixels");
    let source_w = source_width as f32;
    let source_h = source_height as f32;
    let u0 = crop.left as f32 / source_w;
    let v0 = crop.top as f32 / source_h;
    let u1 = (crop.left + cropped_w) as f32 / source_w;
    let v1 = (crop.top + cropped_h) as f32 / source_h;
    let (sin, cos) = transform.rotation_deg.get().to_radians().sin_cos();
    let px = transform.position.x.get();
    let py = transform.position.y.get();
    let rot = |dx: f32, dy: f32| -> Result<CanvasPoint, GeometryError> {
        let x = px + dx * cos - dy * sin;
        let y = py + dx * sin + dy * cos;
        if x.is_finite() && y.is_finite() {
            Ok(CanvasPoint { x, y })
        } else {
            Err(GeometryError::NotFinite)
        }
    };
    Ok(Some(LegacyQuad {
        corners: [
            rot(0.0, 0.0)?,
            rot(dest_w, 0.0)?,
            rot(dest_w, dest_h)?,
            rot(0.0, dest_h)?,
        ],
        uvs: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
    }))
}

/// Cropped source size in whole pixels, or `None` when nothing remains.
fn cropped_size(source_width: u32, source_height: u32, crop: Crop) -> Option<(u32, u32)> {
    let width = source_width
        .checked_sub(crop.left)
        .and_then(|width| width.checked_sub(crop.right))
        .filter(|width| *width > 0)?;
    let height = source_height
        .checked_sub(crop.top)
        .and_then(|height| height.checked_sub(crop.bottom))
        .filter(|height| *height > 0)?;
    Some((width, height))
}

/// Destination size after crop and scale. `Ok(None)` draws nothing. A non-finite
/// product is an error. Zero on either axis draws nothing; negative is a flip.
fn scaled_crop(
    transform: ItemTransform,
    crop: Crop,
    source_width: u32,
    source_height: u32,
) -> Result<Option<(f32, f32)>, GeometryError> {
    let Some((cropped_w, cropped_h)) = cropped_size(source_width, source_height, crop) else {
        return Ok(None);
    };
    let dest_w = cropped_w as f32 * transform.scale.x.get();
    let dest_h = cropped_h as f32 * transform.scale.y.get();
    if !dest_w.is_finite() || !dest_h.is_finite() {
        return Err(GeometryError::NotFinite);
    }
    if dest_w == 0.0 || dest_h == 0.0 {
        return Ok(None);
    }
    Ok(Some((dest_w, dest_h)))
}

/// Fitted content size. `inside` is fit; otherwise fill.
///
/// The aspect comparison is a cross-multiply, so equal aspects take the width-limited
/// branch and still produce the box exactly.
fn fitted(cropped_w: u32, cropped_h: u32, box_w: f32, box_h: f32, inside: bool) -> (f32, f32) {
    let cropped_w = cropped_w as f32;
    let cropped_h = cropped_h as f32;
    let source_wider = cropped_w * box_h >= cropped_h * box_w;
    let limit_by_width = if inside { source_wider } else { !source_wider };
    if limit_by_width {
        (box_w, box_w * cropped_h / cropped_w)
    } else {
        (box_h * cropped_w / cropped_h, box_h)
    }
}

/// Clockwise about z in a y-down frame. CSS `rotateZ`: positive degrees move +x toward +y.
fn rotate_z(x: f32, y: f32, z: f32, degrees: f32) -> (f32, f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x * cos - y * sin, x * sin + y * cos, z)
}

/// CSS `rotateY`. Positive degrees move the +x edge toward negative z (away).
fn rotate_y(x: f32, y: f32, z: f32, degrees: f32) -> (f32, f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x * cos + z * sin, y, -x * sin + z * cos)
}

/// CSS `rotateX`. Positive degrees move the −y edge (the top) toward negative z (away).
fn rotate_x(x: f32, y: f32, z: f32, degrees: f32) -> (f32, f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x, y * cos - z * sin, y * sin + z * cos)
}

#[cfg(test)]
mod tests {
    use super::{
        Canvas, CanvasPoint, FitMode, GeometryError, ItemGeometry, LegacyBox, Placed,
        PlacementForm, Size, legacy_quad,
    };
    use crate::command::{Crop, ItemTransform, Vec2};
    use crate::finite::Finite32;

    fn finite(value: f32) -> Finite32 {
        Finite32::new(value).expect("finite")
    }

    fn vec2(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y).expect("finite")
    }

    fn size(width: f32, height: f32) -> Size {
        Size::try_new(width, height).expect("positive size")
    }

    fn box_at(x: f32, y: f32, width: f32, height: f32) -> ItemGeometry {
        ItemGeometry::try_new(vec2(x, y), size(width, height)).expect("geometry")
    }

    fn xy(placed: Placed) -> (f32, f32) {
        match placed {
            Placed::At(CanvasPoint { x, y }) => (x, y),
            Placed::BehindViewer => panic!("corner is behind the viewer"),
        }
    }

    fn near(got: (f32, f32), expected: (f32, f32)) {
        assert!(
            (got.0 - expected.0).abs() < 1e-3 && (got.1 - expected.1).abs() < 1e-3,
            "{got:?} vs {expected:?}"
        );
    }

    fn near_point(got: CanvasPoint, expected: (f32, f32)) {
        near((got.x, got.y), expected);
    }

    #[test]
    fn an_unrotated_box_is_centred_on_its_centre() {
        let quad = box_at(100.0, 50.0, 40.0, 20.0).project().expect("projects");
        assert_eq!(xy(quad.corners[0]), (80.0, 40.0));
        assert_eq!(xy(quad.corners[1]), (120.0, 40.0));
        assert_eq!(xy(quad.corners[2]), (120.0, 60.0));
        assert_eq!(xy(quad.corners[3]), (80.0, 60.0));
    }

    #[test]
    fn rotation_is_clockwise_about_the_centre() {
        let mut geometry = box_at(100.0, 50.0, 40.0, 20.0);
        geometry.rotation_deg = finite(90.0);
        let quad = geometry.project().expect("projects");
        // Local (±20, ±10) rotated 90° clockwise, then the centre.
        near(xy(quad.corners[0]), (110.0, 30.0));
        near(xy(quad.corners[1]), (110.0, 70.0));
        near(xy(quad.corners[2]), (90.0, 70.0));
        near(xy(quad.corners[3]), (90.0, 30.0));
    }

    #[test]
    fn neutral_3d_does_not_move_the_box() {
        let mut geometry = box_at(10.0, 20.0, 8.0, 4.0);
        geometry.rotation_deg = finite(90.0);
        assert!(geometry.is_neutral_3d());
        let planar = geometry.project().expect("projects");
        geometry.tilt_deg = finite(0.0);
        geometry.turn_deg = finite(0.0);
        geometry.depth_px = finite(0.0);
        geometry.perspective_px = Some(finite(500.0));
        // z is 0, so perspective scale is 1 and the quad matches.
        let with_perspective = geometry.project().expect("projects");
        assert_eq!(planar, with_perspective);
        assert!(!geometry.is_neutral_3d());
    }

    #[test]
    fn depth_scales_about_the_centre_when_perspective_is_set() {
        let mut geometry = box_at(100.0, 200.0, 20.0, 10.0);
        geometry.depth_px = finite(50.0);
        geometry.perspective_px = Some(finite(100.0));
        let quad = geometry.project().expect("projects");
        // scale = 100 / (100 − 50) = 2, about the centre.
        assert_eq!(xy(quad.corners[0]), (80.0, 190.0));
        assert_eq!(xy(quad.corners[1]), (120.0, 190.0));
        assert_eq!(xy(quad.corners[2]), (120.0, 210.0));
        assert_eq!(xy(quad.corners[3]), (80.0, 210.0));
    }

    #[test]
    fn a_point_at_or_beyond_the_viewer_is_clipped() {
        let mut geometry = box_at(0.0, 0.0, 10.0, 10.0);
        geometry.depth_px = finite(100.0);
        geometry.perspective_px = Some(finite(100.0));
        let quad = geometry.project().expect("projects");
        assert!(
            quad.corners
                .iter()
                .all(|corner| matches!(corner, Placed::BehindViewer))
        );
    }

    #[test]
    fn positive_turn_moves_the_right_edge_away() {
        let mut geometry = box_at(0.0, 0.0, 100.0, 10.0);
        geometry.turn_deg = finite(30.0);
        geometry.perspective_px = Some(finite(1000.0));
        let quad = geometry.project().expect("projects");
        let (left, _) = xy(quad.corners[0]);
        let (right, _) = xy(quad.corners[1]);
        assert!(left < 0.0 && right > 0.0, "left {left}, right {right}");
        // The right edge is farther from the viewer, so perspective pulls it in more.
        assert!(left.abs() > right.abs(), "left {left}, right {right}");
    }

    #[test]
    fn positive_tilt_moves_the_top_edge_away() {
        let mut geometry = box_at(0.0, 0.0, 10.0, 10.0);
        geometry.tilt_deg = finite(30.0);
        geometry.perspective_px = Some(finite(1000.0));
        let quad = geometry.project().expect("projects");
        let (_, top) = xy(quad.corners[0]);
        let (_, bottom) = xy(quad.corners[3]);
        assert!(top < 0.0 && bottom > 0.0, "top {top}, bottom {bottom}");
        assert!(top.abs() < bottom.abs(), "top {top}, bottom {bottom}");
    }

    #[test]
    fn orthographic_depth_does_not_change_the_plane() {
        let mut geometry = box_at(5.0, 6.0, 4.0, 2.0);
        let before = geometry.project().expect("projects");
        geometry.depth_px = finite(40.0);
        assert!(!geometry.is_neutral_3d());
        assert_eq!(geometry.project().expect("projects"), before);
    }

    #[test]
    fn non_positive_size_and_perspective_are_rejected() {
        let error = Size::try_new(10.0, 0.0).expect_err("zero height");
        assert_eq!(error, GeometryError::NonPositiveSize);
        assert_eq!(
            Size::try_new(-4.0, 4.0).expect_err("negative"),
            GeometryError::NonPositiveSize
        );
        assert_eq!(
            Size::try_new(f32::NAN, 1.0).expect_err("nan"),
            GeometryError::NotFinite
        );
        let mut geometry = box_at(0.0, 0.0, 10.0, 10.0);
        geometry.perspective_px = Some(Finite32::ZERO);
        assert_eq!(
            geometry.project().expect_err("perspective"),
            GeometryError::NonPositivePerspective
        );
        geometry.perspective_px = Some(finite(-2.0));
        assert_eq!(
            geometry.validate().expect_err("negative perspective"),
            GeometryError::NonPositivePerspective
        );
    }

    #[test]
    fn stretch_fit_and_fill() {
        let mut geometry = box_at(0.0, 0.0, 100.0, 100.0);
        geometry.fit_mode = FitMode::Stretch;
        // Source 4×2 is wider than the square box.
        let stretch = geometry
            .content(4, 2, Crop::NONE)
            .expect("content")
            .expect("drawn");
        assert_eq!(
            (stretch.left, stretch.top, stretch.width, stretch.height),
            (-50.0, -50.0, 100.0, 100.0)
        );

        geometry.fit_mode = FitMode::Fit;
        let fit = geometry
            .content(4, 2, Crop::NONE)
            .expect("content")
            .expect("drawn");
        assert_eq!(
            (fit.left, fit.top, fit.width, fit.height),
            (-50.0, -25.0, 100.0, 50.0)
        );

        geometry.fit_mode = FitMode::Fill;
        let fill = geometry
            .content(4, 2, Crop::NONE)
            .expect("content")
            .expect("drawn");
        assert_eq!(
            (fill.left, fill.top, fill.width, fill.height),
            (-100.0, -50.0, 200.0, 100.0)
        );

        // A taller source swaps the limited axis.
        geometry.fit_mode = FitMode::Fit;
        let tall = geometry
            .content(2, 4, Crop::NONE)
            .expect("content")
            .expect("drawn");
        assert_eq!((tall.width, tall.height), (50.0, 100.0));
        geometry.fit_mode = FitMode::Fill;
        let tall_fill = geometry
            .content(2, 4, Crop::NONE)
            .expect("content")
            .expect("drawn");
        assert_eq!((tall_fill.width, tall_fill.height), (100.0, 200.0));
    }

    #[test]
    fn equal_aspect_fit_and_fill_are_the_box() {
        let mut geometry = box_at(0.0, 0.0, 200.0, 100.0);
        for mode in [FitMode::Stretch, FitMode::Fit, FitMode::Fill] {
            geometry.fit_mode = mode;
            let rect = geometry
                .content(4, 2, Crop::NONE)
                .expect("content")
                .expect("drawn");
            assert_eq!((rect.width, rect.height), (200.0, 100.0), "{mode:?}");
        }
    }

    #[test]
    fn content_uses_the_crop_and_ignores_rotation() {
        let mut geometry = box_at(0.0, 0.0, 100.0, 100.0);
        geometry.fit_mode = FitMode::Fit;
        let crop = Crop {
            left: 2,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // 4×2 cropped to 2×2, fitted in the square: the whole box.
        let rect = geometry
            .content(4, 2, crop)
            .expect("content")
            .expect("drawn");
        assert_eq!((rect.width, rect.height), (100.0, 100.0));
        assert_eq!((rect.u0, rect.v0, rect.u1, rect.v1), (0.5, 0.0, 1.0, 1.0));

        geometry.rotation_deg = finite(45.0);
        geometry.tilt_deg = finite(20.0);
        let rotated = geometry
            .content(4, 2, crop)
            .expect("content")
            .expect("drawn");
        assert_eq!(rotated, rect);
    }

    #[test]
    fn an_empty_crop_or_source_draws_nothing() {
        let geometry = box_at(0.0, 0.0, 10.0, 10.0);
        assert!(
            geometry
                .content(0, 10, Crop::NONE)
                .expect("content")
                .is_none()
        );
        let eaten = Crop {
            left: 2,
            top: 0,
            right: 2,
            bottom: 0,
        };
        assert!(geometry.content(4, 2, eaten).expect("content").is_none());
        let over = Crop {
            left: 5,
            top: 0,
            right: 0,
            bottom: 0,
        };
        assert!(geometry.content(4, 2, over).expect("content").is_none());
    }

    #[test]
    fn legacy_quad_matches_the_0_3_corner_rule() {
        let identity = legacy_quad(ItemTransform::IDENTITY, Crop::NONE, 4, 2)
            .expect("finite")
            .expect("drawn");
        assert_eq!(identity.corners[0], CanvasPoint { x: 0.0, y: 0.0 });
        assert_eq!(identity.corners[1], CanvasPoint { x: 4.0, y: 0.0 });
        assert_eq!(identity.corners[2], CanvasPoint { x: 4.0, y: 2.0 });
        assert_eq!(identity.uvs[0], [0.0, 0.0]);
        assert_eq!(identity.uvs[2], [1.0, 1.0]);

        let transform = ItemTransform {
            position: vec2(10.0, 4.0),
            scale: vec2(2.0, 3.0),
            rotation_deg: Finite32::ZERO,
        };
        let crop = Crop {
            left: 2,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let scaled = legacy_quad(transform, crop, 4, 2)
            .expect("finite")
            .expect("drawn");
        // Cropped 2×2, scaled to 4×6, top-left at (10, 4).
        assert_eq!(scaled.corners[0], CanvasPoint { x: 10.0, y: 4.0 });
        assert_eq!(scaled.corners[1], CanvasPoint { x: 14.0, y: 4.0 });
        assert_eq!(scaled.corners[2], CanvasPoint { x: 14.0, y: 10.0 });
        assert_eq!(scaled.uvs[0], [0.5, 0.0]);
        assert_eq!(scaled.uvs[1], [1.0, 0.0]);

        let turned = ItemTransform {
            position: vec2(30.0, 30.0),
            scale: vec2(1.0, 1.0),
            rotation_deg: finite(90.0),
        };
        let quad = legacy_quad(turned, Crop::NONE, 20, 10)
            .expect("finite")
            .expect("drawn");
        near_point(quad.corners[0], (30.0, 30.0));
        near_point(quad.corners[1], (30.0, 50.0));
        near_point(quad.corners[2], (20.0, 50.0));
        near_point(quad.corners[3], (20.0, 30.0));
    }

    #[test]
    fn from_legacy_projects_to_the_same_corners_when_scale_is_positive() {
        let transform = ItemTransform {
            position: vec2(30.0, 30.0),
            scale: vec2(1.0, 1.0),
            rotation_deg: finite(90.0),
        };
        let LegacyBox {
            geometry,
            flip_x,
            flip_y,
        } = ItemGeometry::from_legacy(transform, Crop::NONE, 20, 10)
            .expect("finite")
            .expect("drawn");
        assert!(!flip_x && !flip_y);
        assert_eq!(geometry.fit_mode, FitMode::Stretch);
        assert!(geometry.is_neutral_3d());
        assert_eq!(geometry.size, size(20.0, 10.0));
        let projected = geometry.project().expect("projects");
        let legacy = legacy_quad(transform, Crop::NONE, 20, 10)
            .expect("finite")
            .expect("drawn");
        for (placed, corner) in projected.corners.iter().zip(legacy.corners) {
            near(xy(*placed), (corner.x, corner.y));
        }
        let rect = geometry
            .content(20, 10, Crop::NONE)
            .expect("content")
            .expect("drawn");
        assert_eq!((rect.u0, rect.v0, rect.u1, rect.v1), (0.0, 0.0, 1.0, 1.0));
        assert_eq!((rect.width, rect.height), (20.0, 10.0));
    }

    #[test]
    fn a_negative_scale_flips_the_quad_and_is_flagged_on_the_box() {
        let transform = ItemTransform {
            position: Vec2::new(0.0, 0.0).expect("finite"),
            scale: vec2(-1.0, 1.0),
            ..ItemTransform::IDENTITY
        };
        let quad = legacy_quad(transform, Crop::NONE, 4, 2)
            .expect("finite")
            .expect("drawn");
        assert_eq!(quad.corners[0], CanvasPoint { x: 0.0, y: 0.0 });
        assert_eq!(quad.corners[1], CanvasPoint { x: -4.0, y: 0.0 });
        assert_eq!(quad.corners[2], CanvasPoint { x: -4.0, y: 2.0 });

        let legacy = ItemGeometry::from_legacy(transform, Crop::NONE, 4, 2)
            .expect("finite")
            .expect("drawn");
        assert!(legacy.flip_x && !legacy.flip_y);
        assert_eq!(legacy.geometry.size, size(4.0, 2.0));
        let projected = legacy.geometry.project().expect("projects");
        // The outline matches; the corner order does not, because geometry cannot flip.
        let mut from_quad: Vec<(i32, i32)> = quad
            .corners
            .iter()
            .map(|corner| (corner.x.round() as i32, corner.y.round() as i32))
            .collect();
        let mut from_box: Vec<(i32, i32)> = projected
            .corners
            .iter()
            .map(|corner| {
                let (x, y) = xy(*corner);
                (x.round() as i32, y.round() as i32)
            })
            .collect();
        from_quad.sort();
        from_box.sort();
        assert_eq!(from_quad, from_box);
        assert_ne!(
            xy(projected.corners[0]),
            (quad.corners[0].x, quad.corners[0].y)
        );
    }

    #[test]
    fn a_legacy_placement_that_leaves_nothing_draws_nothing() {
        let flat = ItemTransform {
            scale: vec2(0.0, 1.0),
            ..ItemTransform::IDENTITY
        };
        assert!(
            legacy_quad(flat, Crop::NONE, 4, 2)
                .expect("finite")
                .is_none()
        );
        assert!(
            ItemGeometry::from_legacy(flat, Crop::NONE, 4, 2)
                .expect("finite")
                .is_none()
        );
        assert!(
            legacy_quad(ItemTransform::IDENTITY, Crop::NONE, 0, 2)
                .expect("finite")
                .is_none()
        );
    }

    #[test]
    fn covering_canvas_is_the_1_0_default() {
        let geometry = ItemGeometry::covering_canvas(1920, 1080).expect("canvas");
        assert_eq!(geometry.center, vec2(960.0, 540.0));
        assert_eq!(geometry.size, size(1920.0, 1080.0));
        assert_eq!(geometry.fit_mode, FitMode::Fit);
        assert!(geometry.is_neutral_3d());
        assert_eq!(
            ItemGeometry::covering_canvas(0, 1080).expect_err("empty"),
            GeometryError::NonPositiveSize
        );
    }

    #[test]
    fn wire_defaults_and_round_trip() {
        let parsed: ItemGeometry = serde_json::from_str(
            r#"{"center":{"x":1.0,"y":2.0},"size":{"width":3.0,"height":4.0}}"#,
        )
        .expect("parses");
        assert_eq!(parsed.fit_mode, FitMode::Fit);
        assert_eq!(parsed.rotation_deg, Finite32::ZERO);
        assert_eq!(parsed.tilt_deg, Finite32::ZERO);
        assert_eq!(parsed.turn_deg, Finite32::ZERO);
        assert_eq!(parsed.depth_px, Finite32::ZERO);
        assert_eq!(parsed.perspective_px, None);
        assert!(parsed.is_neutral_3d());
        let json = serde_json::to_value(parsed).expect("serializes");
        assert!(json.get("perspective_px").is_none());
        assert_eq!(json["fit_mode"], "fit");

        let mut full = parsed;
        full.fit_mode = FitMode::Fill;
        full.rotation_deg = finite(12.5);
        full.tilt_deg = finite(1.0);
        full.turn_deg = finite(-2.0);
        full.depth_px = finite(3.0);
        full.perspective_px = Some(finite(800.0));
        let text = serde_json::to_string(&full).expect("serializes");
        let back: ItemGeometry = serde_json::from_str(&text).expect("parses");
        assert_eq!(back, full);
        assert!(serde_json::from_str::<FitMode>("\"cover\"").is_err());
        assert!(
            serde_json::from_str::<ItemGeometry>(
                r#"{"center":{"x":1e39,"y":0},"size":{"width":1,"height":1}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn the_transform_view_is_the_rotated_top_left_at_unit_scale() {
        let unrotated = box_at(100.0, 50.0, 40.0, 20.0)
            .transform_view()
            .expect("finite");
        assert_eq!(unrotated.position, Vec2::new(80.0, 40.0).expect("finite"));
        assert_eq!(unrotated.scale, ItemTransform::IDENTITY.scale);
        assert_eq!(unrotated.rotation_deg, Finite32::ZERO);

        // 90° clockwise about (100, 50): the top-left (-20, -10) goes to (10, -20).
        let turned = ItemGeometry {
            rotation_deg: finite(90.0),
            ..box_at(100.0, 50.0, 40.0, 20.0)
        };
        let view = turned.transform_view().expect("finite");
        near(
            (view.position.x.get(), view.position.y.get()),
            (110.0, 30.0),
        );
        assert_eq!(view.rotation_deg, finite(90.0));
        // The view's corner is the projected box's first corner.
        let quad = turned.project().expect("finite");
        near(xy(quad.corners[0]), (110.0, 30.0));
    }

    #[test]
    fn canvas_and_placement_form_parse_their_pre_0_10_defaults() {
        assert_eq!(
            Canvas::default(),
            Canvas {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(PlacementForm::default(), PlacementForm::Legacy);
        assert_eq!(
            serde_json::to_value(PlacementForm::Geometry).expect("serializes"),
            "geometry"
        );
        assert!(serde_json::from_str::<PlacementForm>("\"box\"").is_err());
    }
}
