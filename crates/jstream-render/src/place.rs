//! Item placement: a legacy 0.3 transform through [`jstream_protocol::legacy_quad`], or
//! an ADR-0022 geometry through [`ItemGeometry::content`] and [`ItemGeometry::place`].

use jstream_protocol::{
    Crop as ProtocolCrop, Finite32, GeometryError, ItemGeometry, ItemTransform, Placed, Vec2,
    legacy_quad,
};

use crate::error::RenderError;
use crate::scene::{Crop, LegacyTransform, Placement};

/// One drawn quad. Corners are canvas pixels, y down: top-left, top-right, bottom-right,
/// bottom-left.
pub(crate) struct Quad {
    pub corners: [[f32; 2]; 4],
    pub uvs: [[f32; 2]; 4],
}

/// The quad for one item, or `Ok(None)` when the crop or the scale leaves nothing to draw.
///
/// A non-finite component, including a product that overflows, is
/// [`RenderError::Invalid`], and so is a geometry with non-neutral 3D.
///
/// # Errors
/// [`RenderError::Invalid`] when a placement component is not finite or not drawable.
pub(crate) fn quad(
    source_width: u32,
    source_height: u32,
    placement: Placement,
    crop: Crop,
) -> Result<Option<Quad>, RenderError> {
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
) -> Result<Option<Quad>, RenderError> {
    let transform = ItemTransform {
        position: finite_vec(transform.position)?,
        scale: finite_vec(transform.scale)?,
        rotation_deg: Finite32::new(transform.rotation_deg).ok_or_else(not_finite)?,
    };
    let placed =
        legacy_quad(transform, crop, source_width, source_height).map_err(placement_error)?;
    Ok(placed.map(|quad| Quad {
        corners: quad.corners.map(|point| [point.x, point.y]),
        uvs: quad.uvs,
    }))
}

/// The content rectangle the shared formula fits into the box, clipped to the box (only
/// `fill` reaches past it), with its UVs cut to match, then placed corner by corner.
///
/// Clipping happens in the box's local frame, before rotation, so it is exact for any
/// in-plane rotation. With 3D neutral every corner is in front of the viewer and the
/// placement is affine, so linear UVs are correct. A flip arrives as the content's
/// swapped UV pair, so the same interpolation mirrors the clipped part.
fn boxed(
    source_width: u32,
    source_height: u32,
    geometry: ItemGeometry,
    crop: ProtocolCrop,
) -> Result<Option<Quad>, RenderError> {
    if !geometry.is_neutral_3d() {
        return Err(RenderError::Invalid {
            what: "placement",
            detail: "tilt, turn, depth and perspective are not drawn yet".into(),
        });
    }
    let Some(content) = geometry
        .content(source_width, source_height, crop)
        .map_err(placement_error)?
    else {
        return Ok(None);
    };
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
    Ok(Some(Quad { corners, uvs }))
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

    use super::{placement_error, quad};
    use crate::error::RenderError;
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
        let q = quad(4, 2, Placement::IDENTITY, Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(q.corners[0], [0.0, 0.0]);
        assert_eq!(q.corners[1], [4.0, 0.0]);
        assert_eq!(q.corners[2], [4.0, 2.0]);
        assert_eq!(q.corners[3], [0.0, 2.0]);
        assert_eq!(q.uvs[0], [0.0, 0.0]);
        assert_eq!(q.uvs[2], [1.0, 1.0]);
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
        let q = quad(4, 2, placement, crop).expect("finite").expect("draws");
        // Cropped size is 2x2, scaled to 4x6, top-left at (10, 4).
        assert_eq!(q.corners[0], [10.0, 4.0]);
        assert_eq!(q.corners[1], [14.0, 4.0]);
        assert_eq!(q.corners[2], [14.0, 10.0]);
        assert_eq!(q.uvs[0], [0.5, 0.0]);
        assert_eq!(q.uvs[1], [1.0, 0.0]);
    }

    #[test]
    fn ninety_degrees_clockwise_about_the_top_left() {
        let placement = Placement::Legacy(LegacyTransform {
            position: [30.0, 30.0],
            scale: [1.0, 1.0],
            rotation_deg: 90.0,
        });
        let q = quad(20, 10, placement, Crop::NONE)
            .expect("finite")
            .expect("draws");
        near(q.corners[0], [30.0, 30.0]);
        near(q.corners[1], [30.0, 50.0]);
        near(q.corners[2], [20.0, 50.0]);
        near(q.corners[3], [20.0, 30.0]);
    }

    #[test]
    fn a_negative_scale_reflects_about_the_top_left() {
        let placement = Placement::Legacy(LegacyTransform {
            position: [8.0, 6.0],
            scale: [-1.0, 1.0],
            rotation_deg: 0.0,
        });
        let q = quad(4, 2, placement, Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(q.corners[0], [8.0, 6.0]);
        assert_eq!(q.corners[1], [4.0, 6.0]);
        assert_eq!(q.corners[2], [4.0, 8.0]);
        assert_eq!(q.corners[3], [8.0, 8.0]);
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
            quad(4, 2, Placement::IDENTITY, full)
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
            quad(4, 2, Placement::IDENTITY, over)
                .expect("finite")
                .is_none()
        );
        let flat = Placement::Legacy(LegacyTransform {
            scale: [0.0, 1.0],
            ..LegacyTransform::IDENTITY
        });
        assert!(quad(4, 2, flat, Crop::NONE).expect("finite").is_none());
        assert!(
            quad(0, 2, Placement::IDENTITY, Crop::NONE)
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
        assert!(quad(4, 2, placement, Crop::NONE).is_err());
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
        let q = quad(4, 2, boxed(20.0, 20.0, FitMode::Stretch, 0.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners,
            [[40.0, 30.0], [60.0, 30.0], [60.0, 50.0], [40.0, 50.0]]
        );
        assert_eq!(q.uvs, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    #[test]
    fn fit_letterboxes_inside_the_box() {
        // A 4×2 source in a 20×20 box: 20×10, centred.
        let q = quad(4, 2, boxed(20.0, 20.0, FitMode::Fit, 0.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners,
            [[40.0, 35.0], [60.0, 35.0], [60.0, 45.0], [40.0, 45.0]]
        );
        assert_eq!(q.uvs[2], [1.0, 1.0]);
    }

    #[test]
    fn fill_is_clipped_to_the_box_and_its_uvs_cut_to_match() {
        // A 4×2 source filling a 20×20 box is 40×20; the box keeps the middle half.
        let q = quad(4, 2, boxed(20.0, 20.0, FitMode::Fill, 0.0), Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners,
            [[40.0, 30.0], [60.0, 30.0], [60.0, 50.0], [40.0, 50.0]]
        );
        assert_eq!(q.uvs, [[0.25, 0.0], [0.75, 0.0], [0.75, 1.0], [0.25, 1.0]]);
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
        let q = quad(4, 2, mirrored, Crop::NONE)
            .expect("finite")
            .expect("draws");
        assert_eq!(
            q.corners,
            [[40.0, 30.0], [60.0, 30.0], [60.0, 50.0], [40.0, 50.0]]
        );
        assert_eq!(q.uvs, [[0.75, 1.0], [0.25, 1.0], [0.25, 0.0], [0.75, 0.0]]);
    }

    #[test]
    fn a_geometry_rotates_about_its_centre_and_crops_first() {
        let crop = Crop {
            left: 2,
            ..Crop::NONE
        };
        // Cropped to 2×2, stretched to the 20×10 box, turned 90° clockwise about (50, 40).
        let q = quad(4, 2, boxed(20.0, 10.0, FitMode::Stretch, 90.0), crop)
            .expect("finite")
            .expect("draws");
        near(q.corners[0], [55.0, 30.0]);
        near(q.corners[1], [55.0, 50.0]);
        near(q.corners[2], [45.0, 50.0]);
        near(q.corners[3], [45.0, 30.0]);
        assert_eq!(q.uvs[0], [0.5, 0.0]);
        assert_eq!(q.uvs[2], [1.0, 1.0]);
    }

    #[test]
    fn a_geometry_crop_that_leaves_nothing_draws_nothing() {
        let crop = Crop {
            left: 4,
            ..Crop::NONE
        };
        assert!(
            quad(4, 2, boxed(20.0, 20.0, FitMode::Fit, 0.0), crop)
                .expect("finite")
                .is_none()
        );
    }

    #[test]
    fn non_neutral_3d_is_refused() {
        let Placement::Geometry(geometry) = boxed(20.0, 20.0, FitMode::Fit, 0.0) else {
            unreachable!("boxed is a geometry");
        };
        let tilted = Placement::Geometry(ItemGeometry {
            tilt_deg: Finite32::new(10.0).expect("finite"),
            ..geometry
        });
        assert!(matches!(
            quad(4, 2, tilted, Crop::NONE),
            Err(RenderError::Invalid {
                what: "placement",
                ..
            })
        ));
    }
}
