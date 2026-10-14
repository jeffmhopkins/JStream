//! sRGB transfer for the clear colour. The resolve pass leaves encoding of the image to
//! the `Rgba8UnormSrgb` target.

use crate::error::RenderError;
use crate::scene::Color;

/// Converts one sRGB channel in `0..=1` to linear light.
pub(crate) fn srgb_channel_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn validate(color: Color) -> Result<(), RenderError> {
    for (name, channel) in [
        ("r", color.r),
        ("g", color.g),
        ("b", color.b),
        ("a", color.a),
    ] {
        if !channel.is_finite() || !(0.0..=1.0).contains(&channel) {
            return Err(RenderError::Invalid {
                what: "color",
                detail: format!("{name} is {channel}, not a finite value in 0..=1"),
            });
        }
    }
    Ok(())
}

/// Premultiplied linear RGBA, for clearing the FP16 target.
pub(crate) fn premultiplied_linear(color: Color) -> [f32; 4] {
    let a = color.a;
    [
        srgb_channel_to_linear(color.r) * a,
        srgb_channel_to_linear(color.g) * a,
        srgb_channel_to_linear(color.b) * a,
        a,
    ]
}

#[cfg(test)]
mod tests {
    use super::{premultiplied_linear, srgb_channel_to_linear, validate};
    use crate::scene::Color;

    #[test]
    fn endpoints_are_exact() {
        assert_eq!(srgb_channel_to_linear(0.0), 0.0);
        assert_eq!(srgb_channel_to_linear(1.0), 1.0);
    }

    #[test]
    fn mid_grey_matches_the_iec_curve() {
        let linear = srgb_channel_to_linear(0.5);
        let expected = ((0.5 + 0.055) / 1.055_f32).powf(2.4);
        assert!((linear - expected).abs() < 1e-6, "{linear} vs {expected}");
    }

    #[test]
    fn clear_premultiplies_after_the_transfer() {
        let c = premultiplied_linear(Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 0.5,
        });
        assert_eq!(c, [0.5, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn non_finite_and_out_of_range_colours_are_rejected() {
        assert!(validate(Color::rgb(0.0, 1.0, 0.25)).is_ok());
        assert!(validate(Color::rgb(f32::NAN, 0.0, 0.0)).is_err());
        assert!(
            validate(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.1
            })
            .is_err()
        );
    }
}
