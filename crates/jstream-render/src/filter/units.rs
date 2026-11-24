//! A built-in filter's pixel-unit settings count in its input's extent, the size the
//! image is placed at ([`Image::extent`], ADR-0073), not in its texels (#1680,
//! docs/adr/0211-a-filters-pixel-settings-count-in-its-inputs-extent.md). An image can hold fewer
//! texels than its extent (a model's layer drawn smaller while the GPU is short of time,
//! ADR-0197) or more (a source drawn sharp for an item that shows it enlarged), so before
//! a pass draws, each such setting is multiplied by the input's texels per extent pixel
//! along its axis, and the pass sees settings in texels. Its reach ([`super::Pass::reach`])
//! and everything it draws then count in texels too.
//!
//! Which settings: every `px` and `px/s` parameter of the kinds [`super::descriptors`]
//! lists, except those [`UNSCALED`] names. Depth-aware kinds count in their scene's
//! pixels and draw over its canvas-sized composite, so they are not listed; nor are an
//! effect pack's filters and shader sources, whose parameters the engine can't tell
//! apart.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use jstream_protocol::{ParamType, Settings};
use serde_json::Value;

use crate::image::Image;

/// Pixel-unit parameters that count in something other than the input's pixels, by kind
/// and parameter ID.
const UNSCALED: &[(&str, &str)] = &[
    // A frame image's 9-slice corner, in the frame image's own pixels.
    ("border", "frame_slice"),
];

/// Parameters that run along one axis though their ID doesn't end in `_x` or `_y`.
const ALONG: &[(&str, &str, Axis)] = &[
    // Horizontal lines, spaced and rolled down the frame.
    ("scanlines", "spacing", Axis::Y),
    ("scanlines", "speed", Axis::Y),
    ("hologram", "spacing", Axis::Y),
    ("hologram", "speed", Axis::Y),
    // Red and blue read to the left and the right.
    ("scanlines", "rgb_offset", Axis::X),
    ("hologram", "fringe", Axis::X),
];

/// Which of the input's scales a parameter takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
    /// A length in any direction (a radius, a width): the geometric mean of the two,
    /// which is either when the image is scaled alike both ways.
    Both,
}

/// One parameter to scale.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scaled {
    pub(crate) id: String,
    pub(crate) axis: Axis,
    /// An `int` parameter, rounded to a whole number once scaled.
    pub(crate) whole: bool,
}

/// The parameters of `kind_id` that count in its input's pixels; empty for a kind with
/// none, or one [`super::descriptors`] doesn't list.
pub(crate) fn of(kind_id: &str) -> &'static [Scaled] {
    static TABLE: OnceLock<BTreeMap<String, Vec<Scaled>>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            super::descriptors()
                .into_iter()
                .map(|kind| {
                    let scaled = kind
                        .params
                        .iter()
                        .filter_map(|param| {
                            let (unit, whole) = match &param.ty {
                                ParamType::Float { unit, .. } => (unit.as_deref(), false),
                                ParamType::Int { unit, .. } => (unit.as_deref(), true),
                                _ => return None,
                            };
                            let pixels = matches!(unit, Some("px" | "px/s"));
                            let unscaled = UNSCALED.contains(&(&kind.kind_id, &param.id));
                            (pixels && !unscaled).then(|| Scaled {
                                axis: axis_of(&kind.kind_id, &param.id),
                                id: param.id.clone(),
                                whole,
                            })
                        })
                        .collect();
                    (kind.kind_id, scaled)
                })
                .collect()
        })
        .get(kind_id)
        .map_or(&[], Vec::as_slice)
}

fn axis_of(kind_id: &str, id: &str) -> Axis {
    if let Some((_, _, axis)) = ALONG.iter().find(|(k, p, _)| *k == kind_id && *p == id) {
        *axis
    } else if id.ends_with("_x") {
        Axis::X
    } else if id.ends_with("_y") {
        Axis::Y
    } else {
        Axis::Both
    }
}

/// `input`'s texels per extent pixel, across and down: 1 each way for an image placed
/// at its own size, and for a side whose extent is 0.
pub(crate) fn scale_of(input: &Image) -> [f32; 2] {
    let (width, height) = (input.width(), input.height());
    let (across, down) = input.extent();
    let ratio = |texels: u32, extent: u32| match extent {
        0 => 1.0,
        extent => texels as f32 / extent as f32,
    };
    [ratio(width, across), ratio(height, down)]
}

/// `settings` with each of `scaled` multiplied by `scale` along its axis: what a pass
/// drawing over texels reads. A setting that is missing or not a number stays as it is,
/// and the pass reads it as it would. A value past the pass's own cap is held there by
/// the pass, which bounds its cost.
pub(crate) fn apply(settings: &Settings, scaled: &[Scaled], scale: [f32; 2]) -> Settings {
    let mut out = settings.clone();
    let both = (f64::from(scale[0]) * f64::from(scale[1])).sqrt();
    for param in scaled {
        let Some(value) = out.0.get_mut(&param.id) else {
            continue;
        };
        let Some(number) = value.as_f64() else {
            continue;
        };
        let by = match param.axis {
            Axis::X => f64::from(scale[0]),
            Axis::Y => f64::from(scale[1]),
            Axis::Both => both,
        };
        let moved = number * by;
        *value = if param.whole {
            Value::from(moved.round() as i64)
        } else {
            Value::from(moved)
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ALONG, Axis, UNSCALED, apply, of};
    use crate::filter::descriptors;
    use jstream_protocol::{ParamType, Settings};
    use serde_json::json;

    /// Every pixel-unit parameter of every listed kind is scaled, or named as counting
    /// in something else, and every name in the two tables is a real parameter, so a
    /// renamed or new one can't slip past.
    #[test]
    fn every_pixel_parameter_is_scaled_or_named() {
        for kind in descriptors() {
            let scaled = of(&kind.kind_id);
            for param in &kind.params {
                let unit = match &param.ty {
                    ParamType::Float { unit, .. } | ParamType::Int { unit, .. } => unit.as_deref(),
                    _ => None,
                };
                let pixels = matches!(unit, Some("px" | "px/s"));
                let named = UNSCALED.contains(&(kind.kind_id.as_str(), param.id.as_str()));
                let found = scaled.iter().any(|s| s.id == param.id);
                assert_eq!(
                    found,
                    pixels && !named,
                    "{}.{} ({unit:?})",
                    kind.kind_id,
                    param.id
                );
            }
        }
        for (kind, id) in UNSCALED
            .iter()
            .map(|(k, p)| (k, p))
            .chain(ALONG.iter().map(|(k, p, _)| (k, p)))
        {
            let listed = descriptors()
                .into_iter()
                .find(|d| d.kind_id == *kind)
                .is_some_and(|d| d.param(id).is_some());
            assert!(listed, "{kind}.{id} is not a parameter");
        }
        let shadow = of("drop_shadow");
        let axis = |id: &str| shadow.iter().find(|s| s.id == id).map(|s| s.axis);
        assert_eq!(axis("offset_x"), Some(Axis::X));
        assert_eq!(axis("offset_y"), Some(Axis::Y));
        assert_eq!(axis("blur"), Some(Axis::Both));
        assert!(of("border").iter().all(|s| s.id != "frame_slice"));
        assert!(
            of("ambient_occlusion").is_empty(),
            "depth kinds aren't scaled"
        );
    }

    #[test]
    fn settings_scale_along_their_axis_and_whole_numbers_round() {
        let settings = Settings(
            [
                ("offset_x".to_owned(), json!(8.0)),
                ("offset_y".to_owned(), json!(8.0)),
                ("blur".to_owned(), json!(10.0)),
                ("color".to_owned(), json!("#000000")),
            ]
            .into_iter()
            .collect(),
        );
        let out = apply(&settings, of("drop_shadow"), [0.5, 2.0]);
        assert_eq!(out.0["offset_x"], json!(4.0));
        assert_eq!(out.0["offset_y"], json!(16.0));
        assert_eq!(out.0["blur"], json!(10.0));
        assert_eq!(out.0["color"], json!("#000000"));
        let block = Settings([("block_size".to_owned(), json!(15))].into_iter().collect());
        let out = apply(&block, of("pixelate"), [0.5, 0.5]);
        assert_eq!(out.0["block_size"], json!(8));
    }
}
