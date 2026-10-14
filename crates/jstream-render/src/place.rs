//! Legacy 0.3 placement, evaluated by [`jstream_protocol::legacy_quad`].

use jstream_protocol::{
    Crop as ProtocolCrop, Finite32, GeometryError, ItemTransform, Vec2, legacy_quad,
};

use crate::error::RenderError;
use crate::scene::{Crop, Placement};

/// One drawn quad. Corners are canvas pixels, y down: top-left, top-right, bottom-right,
/// bottom-left.
pub(crate) struct Quad {
    pub corners: [[f32; 2]; 4],
    pub uvs: [[f32; 2]; 4],
}

/// The quad for one item, or `Ok(None)` when the crop or the scale leaves nothing to draw.
///
/// Corners and UVs come from [`legacy_quad`]. A non-finite component, including a product
/// that overflows, is [`RenderError::Invalid`].
///
/// # Errors
/// [`RenderError::Invalid`] when a placement component is not finite.
pub(crate) fn quad(
    source_width: u32,
    source_height: u32,
    placement: Placement,
    crop: Crop,
) -> Result<Option<Quad>, RenderError> {
    let transform = ItemTransform {
        position: finite_vec(placement.position)?,
        scale: finite_vec(placement.scale)?,
        rotation_deg: Finite32::new(placement.rotation_deg).ok_or_else(not_finite)?,
    };
    let crop = ProtocolCrop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    };
    let placed =
        legacy_quad(transform, crop, source_width, source_height).map_err(placement_error)?;
    Ok(placed.map(|quad| Quad {
        corners: quad.corners.map(|point| [point.x, point.y]),
        uvs: quad.uvs,
    }))
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

/// `legacy_quad` only returns [`GeometryError::NotFinite`] today. A later variant keeps
/// its own message instead of being reported as non-finite.
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
    use super::quad;
    use crate::scene::{Crop, Placement};

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
        let placement = Placement {
            position: [10.0, 4.0],
            scale: [2.0, 3.0],
            rotation_deg: 0.0,
        };
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
        let placement = Placement {
            position: [30.0, 30.0],
            scale: [1.0, 1.0],
            rotation_deg: 90.0,
        };
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
        let placement = Placement {
            position: [8.0, 6.0],
            scale: [-1.0, 1.0],
            rotation_deg: 0.0,
        };
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
        let flat = Placement {
            scale: [0.0, 1.0],
            ..Placement::IDENTITY
        };
        assert!(quad(4, 2, flat, Crop::NONE).expect("finite").is_none());
        assert!(
            quad(0, 2, Placement::IDENTITY, Crop::NONE)
                .expect("finite")
                .is_none()
        );
    }

    #[test]
    fn non_finite_placement_is_an_error() {
        let placement = Placement {
            position: [f32::INFINITY, 0.0],
            ..Placement::IDENTITY
        };
        assert!(quad(4, 2, placement, Crop::NONE).is_err());
    }
}
