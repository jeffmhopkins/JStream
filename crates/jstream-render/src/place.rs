//! Legacy 0.3 placement: crop, then scale, then rotate clockwise about the top-left.

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
/// # Errors
/// [`RenderError::Invalid`] when a placement component is not finite.
pub(crate) fn quad(
    source_width: u32,
    source_height: u32,
    placement: Placement,
    crop: Crop,
) -> Result<Option<Quad>, RenderError> {
    if !placement.position.iter().all(|v| v.is_finite())
        || !placement.scale.iter().all(|v| v.is_finite())
        || !placement.rotation_deg.is_finite()
    {
        return Err(RenderError::Invalid {
            what: "placement",
            detail: "a component is not finite".into(),
        });
    }
    if source_width == 0 || source_height == 0 {
        return Ok(None);
    }
    let Some(cropped_w) = source_width
        .checked_sub(crop.left)
        .and_then(|w| w.checked_sub(crop.right))
        .filter(|w| *w > 0)
    else {
        return Ok(None);
    };
    let Some(cropped_h) = source_height
        .checked_sub(crop.top)
        .and_then(|h| h.checked_sub(crop.bottom))
        .filter(|h| *h > 0)
    else {
        return Ok(None);
    };
    let dw = cropped_w as f32 * placement.scale[0];
    let dh = cropped_h as f32 * placement.scale[1];
    if dw == 0.0 || dh == 0.0 {
        return Ok(None);
    }

    let sw = source_width as f32;
    let sh = source_height as f32;
    let u0 = crop.left as f32 / sw;
    let v0 = crop.top as f32 / sh;
    let u1 = (crop.left + cropped_w) as f32 / sw;
    let v1 = (crop.top + cropped_h) as f32 / sh;
    let uvs = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];

    let (sin, cos) = placement.rotation_deg.to_radians().sin_cos();
    let px = placement.position[0];
    let py = placement.position[1];
    let rot =
        |dx: f32, dy: f32| -> [f32; 2] { [px + dx * cos - dy * sin, py + dx * sin + dy * cos] };
    Ok(Some(Quad {
        corners: [rot(0.0, 0.0), rot(dw, 0.0), rot(dw, dh), rot(0.0, dh)],
        uvs,
    }))
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
            .unwrap()
            .unwrap();
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
        let q = quad(4, 2, placement, crop).unwrap().unwrap();
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
        let q = quad(20, 10, placement, Crop::NONE).unwrap().unwrap();
        near(q.corners[0], [30.0, 30.0]);
        near(q.corners[1], [30.0, 50.0]);
        near(q.corners[2], [20.0, 50.0]);
        near(q.corners[3], [20.0, 30.0]);
    }

    #[test]
    fn a_crop_or_scale_that_leaves_nothing_draws_nothing() {
        let full = Crop {
            left: 2,
            top: 0,
            right: 2,
            bottom: 0,
        };
        assert!(quad(4, 2, Placement::IDENTITY, full).unwrap().is_none());
        let over = Crop {
            left: 5,
            top: 0,
            right: 0,
            bottom: 0,
        };
        assert!(quad(4, 2, Placement::IDENTITY, over).unwrap().is_none());
        let flat = Placement {
            scale: [0.0, 1.0],
            ..Placement::IDENTITY
        };
        assert!(quad(4, 2, flat, Crop::NONE).unwrap().is_none());
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
