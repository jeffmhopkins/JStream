//! The area blur and pixelate share: the whole frame, or inside or outside a rectangle
//! given as fractions of the frame. The area's edge is hard (no feather).

use jstream_protocol::{ParamDescriptor, Settings};

use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, choice, float};
use crate::filter::{Blend, Ctx, Draw, Shader};
use crate::image::Image;

const AREA: Shader = Shader {
    label: "multipass-area",
    source: wgsl!("area.wgsl"),
    blend: Blend::Replace,
};

/// Where the effect applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Whole,
    Inside,
    Outside,
}

/// The effect's area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Area {
    pub mode: Mode,
    /// x, y, width, height, fractions of the frame from its top-left.
    pub rect: [f32; 4],
}

/// How the composite reads the effect image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sample {
    /// Bilinear, for a smooth effect such as a blur.
    Smooth,
    /// One texel per block, for pixelate.
    Blocks,
}

/// The area parameters, in the order a UI shows them.
pub(super) fn params() -> Vec<ParamDescriptor> {
    let group = |p: ParamDescriptor| ParamDescriptor {
        group: Some("Area".into()),
        ..p
    };
    vec![
        group(choice(
            "area",
            "Area",
            &[
                ("whole", "Whole frame"),
                ("inside", "Inside the rectangle"),
                ("outside", "Outside the rectangle"),
            ],
            "whole",
        )),
        group(float("area_x", "Left", 0.0, 1.0, 0.25, "")),
        group(float("area_y", "Top", 0.0, 1.0, 0.25, "")),
        group(float("area_width", "Width", 0.0, 1.0, 0.5, "")),
        group(float("area_height", "Height", 0.0, 1.0, 0.5, "")),
    ]
}

/// The area `settings` name.
pub(super) fn read(settings: &Settings) -> Area {
    let read = Read(settings);
    let mode = match read.choice("area", "whole") {
        "inside" => Mode::Inside,
        "outside" => Mode::Outside,
        _ => Mode::Whole,
    };
    Area {
        mode,
        rect: [
            read.f32("area_x", 0.25),
            read.f32("area_y", 0.25),
            read.f32("area_width", 0.5),
            read.f32("area_height", 0.5),
        ],
    }
}

/// The composite's uniform.
pub(super) fn uniforms(area: Area, sample: Sample, scale: u32) -> Vec<u8> {
    let mode = match area.mode {
        Mode::Whole => 0,
        Mode::Inside => 1,
        Mode::Outside => 2,
    };
    Uniforms::new()
        .vec4(area.rect)
        .u32(mode)
        .u32(u32::from(sample == Sample::Blocks))
        .f32(scale.max(1) as f32)
        .finish()
}

/// Draws `effect` (covering the frame, `scale` pixels per texel) where `area` applies
/// and `input` elsewhere, into `output`, the input's size.
pub(super) fn composite(
    ctx: &mut Ctx<'_>,
    input: &Image,
    effect: &Image,
    scale: u32,
    sample: Sample,
    area: Area,
    output: &Image,
) -> Result<(), RenderError> {
    let params = uniforms(area, sample, scale);
    ctx.draw(&Draw {
        shader: AREA,
        input,
        aux: Some(effect),
        params: &params,
        target: output,
    })
}

#[cfg(test)]
mod tests {
    use super::{Mode, Sample, read, uniforms};
    use crate::filter::testing::settings;
    use serde_json::json;

    #[test]
    fn the_area_reads_its_mode_and_rectangle() {
        let area = read(&settings(
            "blur",
            json!({"area": "outside", "area_x": 0.1, "area_width": 0.3}),
        ));
        assert_eq!(area.mode, Mode::Outside);
        assert_eq!(area.rect, [0.1, 0.25, 0.3, 0.5]);
        assert_eq!(read(&settings("pixelate", json!({}))).mode, Mode::Whole);
        // rect at 0, mode at 16, blocks at 20, scale at 24; 32 bytes.
        let bytes = uniforms(area, Sample::Blocks, 4);
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[16..20], &2u32.to_le_bytes());
        assert_eq!(&bytes[20..24], &1u32.to_le_bytes());
        assert_eq!(&bytes[24..28], &4.0f32.to_le_bytes());
    }
}
