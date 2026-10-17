//! `color_correction`: OBS's Color Correction filter. Gamma, contrast, brightness,
//! saturation, hue shift, a multiply colour and opacity, on straight sRGB-encoded
//! colour so OBS's numbers look the same here. Every default is neutral.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::filter::params::{Read, Uniforms, color, filter, float};
use crate::filter::{Blend, Shader};

pub(super) const KIND: &str = "color_correction";

pub(super) const SHADER: Shader = Shader {
    label: "filter-color-correction",
    source: include_str!("color_correction.wgsl"),
    blend: Blend::Replace,
};

/// BT.709 luma weights, applied to encoded values for saturation.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

pub(super) fn descriptor() -> KindDescriptor {
    let described = |text: &str, param: ParamDescriptor| ParamDescriptor {
        description: Some(text.into()),
        ..param
    };
    filter(
        KIND,
        "Color correction",
        vec![
            described(
                "Above 0 brightens the mid-tones, below 0 darkens them; black and white stay.",
                float("gamma", "Gamma", -3.0, 3.0, 0.0, ""),
            ),
            described(
                "0 is unchanged, -1 is flat grey; below -1 inverts.",
                float("contrast", "Contrast", -4.0, 4.0, 0.0, ""),
            ),
            described(
                "Added to every channel.",
                float("brightness", "Brightness", -1.0, 1.0, 0.0, ""),
            ),
            described(
                "0 is unchanged, -1 is greyscale.",
                float("saturation", "Saturation", -1.0, 5.0, 0.0, ""),
            ),
            described(
                "Rotates every colour around the grey axis; 120° turns red green.",
                float("hue_shift", "Hue shift", -180.0, 180.0, 0.0, "°"),
            ),
            described(
                "Multiplies each channel; white is unchanged. Sepia is saturation -1 \
                 times a warm colour.",
                color("multiply", "Multiply color", false, "#ffffff"),
            ),
            float("opacity", "Opacity", 0.0, 100.0, 100.0, "%"),
        ],
    )
}

/// The shader's values for a set of settings.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Correction {
    /// The affine colour matrix: `out = rows[i].xyz · in + rows[i].w`.
    pub rows: [[f32; 4]; 3],
    /// The exponent applied before the matrix.
    pub gamma: f32,
    /// Alpha's multiplier, `0..=1`.
    pub opacity: f32,
}

type M3 = [[f32; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

impl Correction {
    pub(super) fn new(settings: &Settings) -> Self {
        let read = Read(settings);
        let gamma = read.f32("gamma", 0.0);
        let gamma = if gamma < 0.0 {
            1.0 - gamma
        } else {
            1.0 / (1.0 + gamma)
        };
        let contrast = 1.0 + read.f32("contrast", 0.0);
        let offset = 0.5 - 0.5 * contrast + read.f32("brightness", 0.0);
        let saturation = 1.0 + read.f32("saturation", 0.0);
        let mut sat: M3 = [[0.0; 3]; 3];
        for (i, row) in sat.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                let identity = if i == j { 1.0 } else { 0.0 };
                *cell = (1.0 - saturation) * LUMA[j] + saturation * identity;
            }
        }
        // Rodrigues' rotation about the grey axis (1, 1, 1) / √3.
        let (sin, cos) = read.f32("hue_shift", 0.0).to_radians().sin_cos();
        let k = 1.0 / 3f32.sqrt();
        let t = (1.0 - cos) / 3.0;
        let s = sin * k;
        let hue: M3 = [
            [cos + t, t - s, t + s],
            [t + s, cos + t, t - s],
            [t - s, t + s, cos + t],
        ];
        let [mr, mg, mb] = super::encoded_rgb(&read, "multiply", [255, 255, 255]);
        let multiply: M3 = [[mr, 0.0, 0.0], [0.0, mg, 0.0], [0.0, 0.0, mb]];
        let colour = mul(&multiply, &mul(&hue, &sat));
        let mut rows = [[0.0; 4]; 3];
        for (row, m) in rows.iter_mut().zip(colour) {
            // The contrast scale comes first, so it scales the matrix; its offset goes
            // through the rest of the matrix.
            *row = [
                m[0] * contrast,
                m[1] * contrast,
                m[2] * contrast,
                (m[0] + m[1] + m[2]) * offset,
            ];
        }
        Self {
            rows,
            gamma,
            opacity: (read.f32("opacity", 100.0) / 100.0).clamp(0.0, 1.0),
        }
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        Uniforms::new()
            .vec4(self.rows[0])
            .vec4(self.rows[1])
            .vec4(self.rows[2])
            .f32(self.gamma)
            .f32(self.opacity)
            .finish()
    }

    /// The colour an sRGB-encoded `rgb` becomes, before the shader's clamp.
    #[cfg(test)]
    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let e = rgb.map(|c| c.powf(self.gamma));
        self.rows
            .map(|r| r[0] * e[0] + r[1] * e[1] + r[2] * e[2] + r[3])
    }
}

pub(super) fn uniforms(settings: &Settings) -> Vec<u8> {
    Correction::new(settings).bytes()
}

#[cfg(test)]
mod tests {
    use super::{Correction, KIND};
    use crate::filter::pack::{assert_near, run_bands};
    use crate::filter::testing::settings;
    use crate::tests::open;
    use serde_json::json;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    fn correction(overrides: serde_json::Value) -> Correction {
        Correction::new(&settings(KIND, overrides))
    }

    #[test]
    fn defaults_are_the_identity() {
        let c = correction(json!({}));
        assert_eq!(c.gamma, 1.0);
        assert_eq!(c.opacity, 1.0);
        for (i, row) in c.rows.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-6, "row {i} col {j} is {v}");
            }
        }
        assert_eq!(c.bytes().len(), 64);
    }

    #[test]
    fn settings_map_like_obs() {
        assert_eq!(correction(json!({"gamma": 1.0})).gamma, 0.5);
        assert_eq!(correction(json!({"gamma": -1.0})).gamma, 2.0);
        assert_eq!(correction(json!({"opacity": 25.0})).opacity, 0.25);
        let hue = correction(json!({"hue_shift": 120.0}));
        assert!(close(hue.apply([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
        assert!(close(hue.apply([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5]));
        let grey = correction(json!({"saturation": -1.0}));
        assert!(close(grey.apply([1.0, 0.0, 0.0]), [0.2126; 3]));
        let flat = correction(json!({"contrast": -1.0}));
        assert!(close(flat.apply([0.9, 0.1, 0.3]), [0.5; 3]));
        let up = correction(json!({"contrast": 1.0, "brightness": 0.1}));
        assert!(close(up.apply([0.5, 0.25, 0.75]), [0.6, 0.1, 1.1]));
        let warm = correction(json!({"multiply": "#ff8000"}));
        assert!(close(
            warm.apply([1.0, 1.0, 1.0]),
            [1.0, 128.0 / 255.0, 0.0]
        ));
    }

    const R: [u8; 4] = [255, 0, 0, 255];
    const G: [u8; 4] = [0, 255, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const W: [u8; 4] = [255, 255, 255, 255];
    const DARK: [u8; 4] = [64, 64, 64, 255];
    const HALF: [u8; 4] = [200, 100, 50, 128];

    #[test]
    fn defaults_pass_colours_through() {
        let Some(mut opened) = open() else { return };
        let colors = [R, G, B, W, DARK, HALF];
        let got = run_bands(&mut opened, KIND, json!({}), &colors);
        assert_near(&got, &colors, 1);
    }

    #[test]
    fn each_setting_changes_the_picture() {
        let Some(mut opened) = open() else { return };
        let mut run =
            |overrides, colors: &[[u8; 4]]| run_bands(&mut opened, KIND, overrides, colors);
        assert_near(
            &run(json!({"saturation": -1.0}), &[R, G, B]),
            &[[54, 54, 54, 255], [182, 182, 182, 255], [18, 18, 18, 255]],
            1,
        );
        assert_near(&run(json!({"hue_shift": 120.0}), &[R, G]), &[G, B], 1);
        assert_near(
            &run(json!({"gamma": 1.0}), &[DARK]),
            &[[128, 128, 128, 255]],
            1,
        );
        assert_near(
            &run(
                json!({"contrast": 1.0}),
                &[[192, 192, 192, 255], [96, 96, 96, 255]],
            ),
            &[W, [65, 65, 65, 255]],
            1,
        );
        assert_near(
            &run(json!({"brightness": 0.25}), &[DARK]),
            &[[128, 128, 128, 255]],
            1,
        );
        assert_near(
            &run(json!({"multiply": "#ff8000"}), &[W]),
            &[[255, 128, 0, 255]],
            1,
        );
        assert_near(
            &run(json!({"opacity": 50.0}), &[R, HALF]),
            &[[255, 0, 0, 128], [200, 100, 50, 64]],
            1,
        );
    }
}
