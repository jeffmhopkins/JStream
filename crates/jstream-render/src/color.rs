//! sRGB transfer for the clear colour. The resolve pass leaves encoding of the image to
//! the `Rgba8UnormSrgb` target.

use crate::error::RenderError;
use crate::scene::Color;
use jstream_protocol::Blending;

/// Converts one sRGB channel in `0..=1` to linear light.
pub(crate) fn srgb_channel_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Converts one linear channel in `0..=1` to sRGB, the inverse of
/// [`srgb_channel_to_linear`].
pub(crate) fn linear_channel_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Straight sRGB from premultiplied linear RGBA, the inverse of
/// [`premultiplied_linear`]. Transparent is [`Color::TRANSPARENT`].
pub(crate) fn from_premultiplied_linear([r, g, b, a]: [f32; 4]) -> Color {
    if a <= 0.0 {
        return Color::TRANSPARENT;
    }
    let channel = |c: f32| linear_channel_to_srgb((c / a).clamp(0.0, 1.0));
    Color {
        r: channel(r),
        g: channel(g),
        b: channel(b),
        a: a.min(1.0),
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

/// `color` premultiplied, its straight sRGB values kept: the clear colour of a target
/// that holds sRGB-encoded colour (ADR-0126 §3).
pub(crate) fn premultiplied_srgb(color: Color) -> [f32; 4] {
    let a = color.a;
    [color.r * a, color.g * a, color.b * a, a]
}

/// Straight sRGB from premultiplied sRGB-encoded RGBA, the inverse of
/// [`premultiplied_srgb`]. Transparent is [`Color::TRANSPARENT`].
pub(crate) fn from_premultiplied_srgb([r, g, b, a]: [f32; 4]) -> Color {
    if a <= 0.0 {
        return Color::TRANSPARENT;
    }
    let channel = |c: f32| (c / a).clamp(0.0, 1.0);
    Color {
        r: channel(r),
        g: channel(g),
        b: channel(b),
        a: a.min(1.0),
    }
}

/// `color` premultiplied in the `blending` space the compositor mixes in.
pub(crate) fn premultiplied_in(color: Color, blending: Blending) -> [f32; 4] {
    if blending == Blending::Srgb {
        premultiplied_srgb(color)
    } else {
        premultiplied_linear(color)
    }
}

/// Straight sRGB from premultiplied RGBA in the `blending` space, the inverse of
/// [`premultiplied_in`].
pub(crate) fn from_premultiplied_in(color: [f32; 4], blending: Blending) -> Color {
    if blending == Blending::Srgb {
        from_premultiplied_srgb(color)
    } else {
        from_premultiplied_linear(color)
    }
}

#[cfg(test)]
mod tests {
    use super::{linear_channel_to_srgb, premultiplied_linear, srgb_channel_to_linear, validate};
    use crate::scene::Color;
    use jstream_protocol::Blending;

    fn close(a: Color, b: Color) -> bool {
        [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a]
            .iter()
            .all(|d| d.abs() < 1e-4)
    }

    #[test]
    fn the_transfer_round_trips() {
        for c in [0.0, 0.002, 0.04, 0.2, 0.5, 0.9, 1.0] {
            let back = linear_channel_to_srgb(srgb_channel_to_linear(c));
            assert!((back - c).abs() < 1e-5, "{c} came back as {back}");
        }
    }

    #[test]
    fn a_background_over_black_is_opaque_and_darkened_in_the_blending_space() {
        let red = Color::rgb(1.0, 0.0, 0.0);
        for blending in Blending::ALL {
            assert!(close(red.over_black(blending), red));
            assert!(close(Color::TRANSPARENT.over_black(blending), Color::BLACK));
        }
        let half = Color { a: 0.5, ..red };
        assert_eq!(half.over_black(Blending::Linear).a, 1.0);
        assert!(close(
            half.over_black(Blending::Linear),
            Color::rgb(linear_channel_to_srgb(0.5), 0.0, 0.0)
        ));
        // Browser-style, the encoded value is darkened as a browser would: half.
        assert!(close(
            half.over_black(Blending::Srgb),
            Color::rgb(0.5, 0.0, 0.0)
        ));
    }

    #[test]
    fn colours_mix_premultiplied_in_the_blending_space() {
        let red = Color::rgb(1.0, 0.0, 0.0);
        for blending in Blending::ALL {
            assert!(close(red.mix(Color::BLACK, 0.0, blending), red));
            assert!(close(red.mix(Color::BLACK, 1.0, blending), Color::BLACK));
            // Fading out to transparent keeps the colour and lowers the alpha.
            let fading = red.mix(Color::TRANSPARENT, 0.25, blending);
            assert!(close(fading, Color { a: 0.75, ..red }));
            assert!(close(
                red.mix(Color::BLACK, f32::NAN, blending),
                Color::BLACK
            ));
        }
        // Halfway from red to black: half the light, or half the encoded value.
        assert!(close(
            red.mix(Color::BLACK, 0.5, Blending::Linear),
            Color::rgb(linear_channel_to_srgb(0.5), 0.0, 0.0)
        ));
        assert!(close(
            red.mix(Color::BLACK, 0.5, Blending::Srgb),
            Color::rgb(0.5, 0.0, 0.0)
        ));
        assert!(close(
            Color::from_rgba8([255, 0, 0, 51]),
            Color { a: 0.2, ..red }
        ));
    }

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
