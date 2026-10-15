//! `image_mask` and `scene_mask`: cut the input out by another image's coverage.
//!
//! The mask (a decoded image asset, or the referenced scene's composited layer) is
//! stretched over the whole frame, whatever its size or aspect. The input, premultiplied,
//! is scaled by the coverage, so colour and alpha fall off together:
//!
//! - `alpha`: the mask's alpha.
//! - `luma`: BT.709 luma of the mask's sRGB-encoded colour times its alpha, so white is
//!   kept, black and transparent are cut, and `#808080` keeps about half.
//! - `clip` (scene only): a hard clip, kept wherever the scene's alpha is at least one
//!   half and cut elsewhere, so translucent parts of the scene keep the input whole.
//!
//! `invert` uses one minus the coverage. With no mask image, or a `null` reference
//! setting, the input passes through unchanged.

use std::sync::Arc;

use jstream_protocol::{KindDescriptor, ParamType, Settings};

use super::{Reference, asset_param, reference_param};
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, boolean, choice, filter};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const IMAGE_KIND: &str = "image_mask";
pub(super) const SCENE_KIND: &str = "scene_mask";

/// Image types a mask asset may be; the runtime decodes them.
const IMAGE_TYPES: [&str; 6] = ["png", "jpg", "jpeg", "webp", "gif", "bmp"];

const SHADER: Shader = Shader {
    label: "assets-mask",
    source: include_str!("mask.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn image_descriptor() -> KindDescriptor {
    filter(
        IMAGE_KIND,
        "Image mask",
        vec![
            asset_param("image", "Mask image", &IMAGE_TYPES),
            choice(
                "mode",
                "Mode",
                &[("alpha", "Alpha mask"), ("luma", "Luma mask")],
                "alpha",
            ),
            boolean("invert", "Invert", false),
        ],
    )
}

pub(super) fn scene_descriptor() -> KindDescriptor {
    filter(
        SCENE_KIND,
        "Scene mask",
        vec![
            reference_param("scene", "Mask scene", ParamType::Scene),
            choice(
                "mode",
                "Mode",
                &[
                    ("alpha", "Alpha mask"),
                    ("luma", "Luma matte"),
                    ("clip", "Clip to scene"),
                ],
                "alpha",
            ),
            boolean("invert", "Invert", false),
        ],
    )
}

/// Where a mask comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// An image asset, setting `image`.
    Image,
    /// A scene, setting `scene`.
    Scene,
}

impl Source {
    fn setting(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Scene => "scene",
        }
    }
}

/// How the coverage is read; the shader's `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Alpha = 1,
    Luma = 2,
    Clip = 3,
}

pub(super) struct Mask {
    source: Source,
    reference: Reference,
    mode: Mode,
    invert: bool,
}

impl Mask {
    pub(super) fn new(source: Source, settings: &Settings) -> Self {
        let mut pass = Self {
            source,
            reference: Reference::default(),
            mode: Mode::Alpha,
            invert: false,
        };
        pass.update(settings);
        pass
    }
}

/// The uniform: `mode` 0 passes through.
fn params(mode: Option<Mode>, invert: bool) -> Vec<u8> {
    Uniforms::new()
        .u32(mode.map_or(0, |m| m as u32))
        .u32(u32::from(invert))
        .finish()
}

impl Pass for Mask {
    fn update(&mut self, settings: &Settings) {
        let read = Read(settings);
        self.reference.update(settings, self.source.setting());
        self.mode = match (read.choice("mode", "alpha"), self.source) {
            ("luma", _) => Mode::Luma,
            ("clip", Source::Scene) => Mode::Clip,
            _ => Mode::Alpha,
        };
        self.invert = read.bool("invert", false);
    }

    fn set_reference(&mut self, image: Option<Arc<Image>>) {
        self.reference.image = image;
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let aux = self.reference.get(ctx, input)?;
        ctx.draw(&Draw {
            shader: SHADER,
            input,
            aux,
            params: &params(aux.map(|_| self.mode), self.invert),
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::{Mode, params};
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{near, open};

    fn asset() -> Value {
        json!({"type": "asset", "root": "app", "path": "mask.png"})
    }

    fn scene() -> Value {
        json!({"type": "scene", "scene_id": "scn_mask"})
    }

    const IN: [u8; 4] = [200, 100, 50, 255];
    const CUT: [u8; 4] = [0, 0, 0, 0];

    /// An 8x2 mask stretched over an 8x8 input: pairs of columns of opaque white, opaque
    /// mid grey, half-transparent white and transparent, both rows the same, so the
    /// samples below land on mask texel centres.
    fn mask(opened: &crate::tests::Opened) -> crate::Image {
        let row = [
            [255, 255, 255, 255],
            [128, 128, 128, 255],
            [255, 255, 255, 128],
            [0, 0, 0, 0],
        ];
        let row: Vec<[u8; 4]> = row.iter().flat_map(|&c| [c, c]).collect();
        image(opened, 8, &[row.clone(), row].concat())
    }

    /// The input sampled at the centre of each mask column, at `x` 1, 3, 5, 7.
    fn columns(out: &crate::Snapshot, expected: [[u8; 4]; 4]) {
        for (i, e) in expected.into_iter().enumerate() {
            near(out, 2 * i as u32 + 1, 4, e, 3);
        }
    }

    #[test]
    fn params_map_mode_and_invert() {
        assert_eq!(&params(None, true)[0..8], &[0, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&params(Some(Mode::Clip), false)[0..4], &3u32.to_le_bytes());
    }

    #[test]
    fn an_image_mask_cuts_by_alpha_or_luma_and_inverts() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = crate::tests::solid(&opened.compositor, 8, 8, IN);
        let half = [200, 100, 50, 128];
        let cases = [
            ("alpha", false, [IN, IN, half, CUT]),
            ("alpha", true, [CUT, CUT, [200, 100, 50, 127], IN]),
            ("luma", false, [IN, [200, 100, 50, 128], half, CUT]),
            (
                "luma",
                true,
                [CUT, [200, 100, 50, 127], [200, 100, 50, 127], IN],
            ),
        ];
        for (mode, invert, expected) in cases {
            let mut f = filter(
                "image_mask",
                json!({ "image": asset(), "mode": mode, "invert": invert }),
            );
            f.set_reference(Some(Arc::new(mask(&opened))));
            let out = run(&mut opened, &mut f, &input, 0.0);
            columns(&out, expected);
        }
    }

    #[test]
    fn a_scene_mask_clips_hard_and_mattes_by_luma() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = crate::tests::solid(&opened.compositor, 8, 8, IN);
        let cases = [
            ("clip", false, [IN, IN, IN, CUT]),
            ("clip", true, [CUT, CUT, CUT, IN]),
            (
                "luma",
                false,
                [IN, [200, 100, 50, 128], [200, 100, 50, 128], CUT],
            ),
            ("alpha", false, [IN, IN, [200, 100, 50, 128], CUT]),
        ];
        for (mode, invert, expected) in cases {
            let mut f = filter(
                "scene_mask",
                json!({ "scene": scene(), "mode": mode, "invert": invert }),
            );
            f.set_reference(Some(Arc::new(mask(&opened))));
            let out = run(&mut opened, &mut f, &input, 0.0);
            columns(&out, expected);
        }
    }

    #[test]
    fn without_a_mask_the_input_passes_through() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = crate::tests::solid(&opened.compositor, 8, 8, IN);
        for kind in ["image_mask", "scene_mask"] {
            let mut f = filter(kind, json!({ "invert": true }));
            let out = run(&mut opened, &mut f, &input, 0.0);
            columns(&out, [IN; 4]);
            // An image but a null setting still passes through.
            f.set_reference(Some(Arc::new(mask(&opened))));
            let out = run(&mut opened, &mut f, &input, 0.0);
            columns(&out, [IN; 4]);
            // Clearing the image after a mask was set passes through again.
            let key = if kind == "image_mask" {
                "image"
            } else {
                "scene"
            };
            let reference = if kind == "image_mask" {
                asset()
            } else {
                scene()
            };
            f.set_settings(&crate::filter::testing::settings(
                kind,
                json!({ key: reference, "invert": true }),
            ));
            let out = run(&mut opened, &mut f, &input, 0.0);
            columns(&out, [CUT, CUT, [200, 100, 50, 127], IN]);
            f.set_reference(None);
            let out = run(&mut opened, &mut f, &input, 0.0);
            columns(&out, [IN; 4]);
        }
    }
}
