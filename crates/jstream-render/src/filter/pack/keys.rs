//! The keys: `chroma_key`, `color_key` and `luma_key`, after OBS's v2 filters. Each
//! works on straight sRGB-encoded colour and multiplies the input's alpha by its mask,
//! so a key after another key (or over a transparent source) only removes more.
//! Similarity, smoothness and spill use OBS's 1–1000 scale (thousandths), so OBS
//! settings carry over.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings, VisibleIf};

use crate::filter::params::{Read, Uniforms, choice, color, filter, float, int};
use crate::filter::{Blend, Shader};

pub(super) const CHROMA_KIND: &str = "chroma_key";
pub(super) const COLOR_KIND: &str = "color_key";
pub(super) const LUMA_KIND: &str = "luma_key";

pub(super) const CHROMA_SHADER: Shader = Shader {
    label: "filter-chroma-key",
    source: concat!(
        include_str!("keys.wgsl"),
        "\n",
        include_str!("chroma_key.wgsl")
    ),
    blend: Blend::Replace,
};

pub(super) const COLOR_SHADER: Shader = Shader {
    label: "filter-color-key",
    source: concat!(
        include_str!("keys.wgsl"),
        "\n",
        include_str!("color_key.wgsl")
    ),
    blend: Blend::Replace,
};

pub(super) const LUMA_SHADER: Shader = Shader {
    label: "filter-luma-key",
    source: concat!(
        include_str!("keys.wgsl"),
        "\n",
        include_str!("luma_key.wgsl")
    ),
    blend: Blend::Replace,
};

/// The chroma key's presets, and the colour each stands for.
const CHROMA_PRESETS: [(&str, &str, [u8; 3]); 3] = [
    ("green", "Green", [0, 255, 0]),
    ("blue", "Blue", [0, 0, 255]),
    ("magenta", "Magenta", [255, 0, 255]),
];

/// BT.709 limited-range Cb and Cr rows, the same as `chroma_key.wgsl`'s.
const CB: [f32; 3] = [-0.100_644, -0.338_572, 0.439_216];
const CR: [f32; 3] = [0.439_216, -0.398_942, -0.040_274];

fn thousandths(id: &str, label: &str, default: i64, text: &str) -> ParamDescriptor {
    ParamDescriptor {
        description: Some(text.into()),
        ..int(id, label, 1, 1000, default, "")
    }
}

pub(super) fn chroma_descriptor() -> KindDescriptor {
    let mut presets: Vec<(&str, &str)> = CHROMA_PRESETS.iter().map(|(v, l, _)| (*v, *l)).collect();
    presets.push(("custom", "Custom"));
    filter(
        CHROMA_KIND,
        "Chroma key",
        vec![
            choice("key_color_type", "Key color type", &presets, "green"),
            ParamDescriptor {
                visible_if: Some(VisibleIf {
                    param: "key_color_type".into(),
                    equals: "custom".into(),
                }),
                ..color("key_color", "Key color", false, "#00ff00")
            },
            thousandths(
                "similarity",
                "Similarity",
                400,
                "How far in chroma from the key colour is removed, in thousandths.",
            ),
            thousandths(
                "smoothness",
                "Smoothness",
                80,
                "How far past the similarity the edge fades back in, in thousandths.",
            ),
            thousandths(
                "spill",
                "Key color spill reduction",
                100,
                "How far past the similarity kept colour is desaturated, removing the \
                 key colour's cast from edges.",
            ),
        ],
    )
}

pub(super) fn color_descriptor() -> KindDescriptor {
    filter(
        COLOR_KIND,
        "Color key",
        vec![
            color("key_color", "Key color", false, "#00ff00"),
            thousandths(
                "similarity",
                "Similarity",
                80,
                "How far in RGB from the key colour is removed, in thousandths.",
            ),
            thousandths(
                "smoothness",
                "Smoothness",
                50,
                "How far past the similarity the edge fades back in, in thousandths.",
            ),
        ],
    )
}

pub(super) fn luma_descriptor() -> KindDescriptor {
    filter(
        LUMA_KIND,
        "Luma key",
        vec![
            ParamDescriptor {
                description: Some("Brighter than this is removed.".into()),
                ..float("luma_max", "Luma max", 0.0, 1.0, 1.0, "")
            },
            float("luma_max_smooth", "Luma max smooth", 0.0, 1.0, 0.0, ""),
            ParamDescriptor {
                description: Some("Darker than this is removed.".into()),
                ..float("luma_min", "Luma min", 0.0, 1.0, 0.0, "")
            },
            float("luma_min_smooth", "Luma min smooth", 0.0, 1.0, 0.0, ""),
        ],
    )
}

fn per_mille(read: &Read<'_>, id: &str, default: i64) -> f32 {
    read.i64(id, default).clamp(1, 1000) as f32 / 1000.0
}

/// An sRGB-encoded colour's (Cb, Cr), without the offset.
fn chroma(rgb: [f32; 3]) -> [f32; 2] {
    let dot = |row: [f32; 3]| row.iter().zip(rgb).map(|(a, b)| a * b).sum();
    [dot(CB), dot(CR)]
}

/// The chroma key's colour, sRGB-encoded.
fn chroma_key_color(read: &Read<'_>) -> [f32; 3] {
    let preset = read.choice("key_color_type", "green");
    match CHROMA_PRESETS.iter().find(|(value, _, _)| *value == preset) {
        Some((_, _, rgb)) => rgb.map(|c| f32::from(c) / 255.0),
        None => super::encoded_rgb(read, "key_color", [0, 255, 0]),
    }
}

pub(super) fn chroma_uniforms(settings: &Settings) -> Vec<u8> {
    let read = Read(settings);
    Uniforms::new()
        .vec2(chroma(chroma_key_color(&read)))
        .f32(per_mille(&read, "similarity", 400))
        .f32(per_mille(&read, "smoothness", 80))
        .f32(per_mille(&read, "spill", 100))
        .finish()
}

pub(super) fn color_uniforms(settings: &Settings) -> Vec<u8> {
    let read = Read(settings);
    let [r, g, b] = super::encoded_rgb(&read, "key_color", [0, 255, 0]);
    Uniforms::new()
        .vec4([r, g, b, 0.0])
        .f32(per_mille(&read, "similarity", 80))
        .f32(per_mille(&read, "smoothness", 50))
        .finish()
}

pub(super) fn luma_uniforms(settings: &Settings) -> Vec<u8> {
    let read = Read(settings);
    Uniforms::new()
        .f32(read.f32("luma_min", 0.0))
        .f32(read.f32("luma_min_smooth", 0.0))
        .f32(read.f32("luma_max", 1.0))
        .f32(read.f32("luma_max_smooth", 0.0))
        .finish()
}

#[cfg(test)]
mod tests {
    use super::{CHROMA_KIND, COLOR_KIND, LUMA_KIND, Read, chroma, chroma_key_color};
    use crate::filter::pack::{assert_near, run_bands};
    use crate::filter::testing::settings;
    use crate::tests::open;
    use serde_json::json;

    fn f32s(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    #[test]
    fn the_chroma_key_colour_follows_the_preset() {
        let key = |overrides| chroma_key_color(&Read(&settings(CHROMA_KIND, overrides)));
        assert_eq!(key(json!({})), [0.0, 1.0, 0.0]);
        assert_eq!(key(json!({"key_color_type": "blue"})), [0.0, 0.0, 1.0]);
        // The custom colour is used only when the type says so.
        assert_eq!(key(json!({"key_color": "#ff0000"})), [0.0, 1.0, 0.0]);
        let custom = key(json!({"key_color_type": "custom", "key_color": "#ff8000"}));
        assert!((custom[1] - 128.0 / 255.0).abs() < 1e-4, "{custom:?}");
        // Grey has no chroma.
        let grey = chroma([0.5; 3]);
        assert!(grey[0].abs() < 1e-6 && grey[1].abs() < 1e-6);
    }

    #[test]
    fn settings_map_to_uniforms() {
        let chroma = f32s(&super::chroma_uniforms(&settings(
            CHROMA_KIND,
            json!({"similarity": 300}),
        )));
        assert_eq!(chroma.len(), 8);
        assert_eq!(&chroma[2..5], &[0.3, 0.08, 0.1]);
        let color = f32s(&super::color_uniforms(&settings(
            COLOR_KIND,
            json!({"key_color": "#0000ff"}),
        )));
        let want = [0.0, 0.0, 1.0, 0.0, 0.08, 0.05];
        assert!(
            color.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-5),
            "{color:?}"
        );
        let luma = f32s(&super::luma_uniforms(&settings(
            LUMA_KIND,
            json!({"luma_min": 0.25}),
        )));
        assert_eq!(luma, vec![0.25, 0.0, 1.0, 0.0]);
    }

    const R: [u8; 4] = [255, 0, 0, 255];
    const G: [u8; 4] = [0, 255, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const W: [u8; 4] = [255, 255, 255, 255];
    const K: [u8; 4] = [0, 0, 0, 255];
    const GONE: [u8; 4] = [0, 0, 0, 0];
    const HALF_BLUE: [u8; 4] = [0, 0, 255, 128];

    #[test]
    fn the_chroma_key_removes_green_and_its_spill() {
        let Some(mut opened) = open() else { return };
        let colors = [
            G,
            B,
            R,
            W,
            HALF_BLUE,
            [120, 160, 120, 255],
            [140, 160, 140, 255],
        ];
        let got = run_bands(&mut opened, CHROMA_KIND, json!({}), &colors);
        assert_near(&got[..5], &[GONE, B, R, W, HALF_BLUE], 1);
        // 0.441 from green in chroma: 0.041 into the smoothness (alpha 0.37) and the
        // spill range (0.26 of the colour kept, the rest grey).
        assert_near(&got[5..6], &[[141, 152, 141, 94]], 3);
        // 0.482: past the smoothness, opaque, but still inside the spill range.
        assert_near(&got[6..], &[[144, 159, 144, 255]], 2);

        let blue = run_bands(
            &mut opened,
            CHROMA_KIND,
            json!({"key_color_type": "blue"}),
            &[G, B],
        );
        assert_near(&blue, &[G, GONE], 1);
        let custom = run_bands(
            &mut opened,
            CHROMA_KIND,
            json!({"key_color_type": "custom", "key_color": "#ff0000"}),
            &[G, R],
        );
        assert_near(&custom, &[G, GONE], 1);
    }

    #[test]
    fn the_color_key_removes_one_colour() {
        let Some(mut opened) = open() else { return };
        let colors = [G, R, B, [0, 230, 0, 255]];
        let got = run_bands(&mut opened, COLOR_KIND, json!({}), &colors);
        // (0, 230, 0) is 25/255 from green: (0.098 - 0.08) / 0.05 of the way back in.
        assert_near(&got, &[GONE, R, B, [0, 230, 0, 92]], 2);
        let red = run_bands(
            &mut opened,
            COLOR_KIND,
            json!({"key_color": "#ff0000"}),
            &[G, R],
        );
        assert_near(&red, &[G, GONE], 1);
    }

    #[test]
    fn the_luma_key_keeps_a_band_of_brightness() {
        let Some(mut opened) = open() else { return };
        let grey = |v: u8| [v, v, v, 255];
        let colors = [K, grey(64), grey(128), grey(192), W];
        let all = run_bands(&mut opened, LUMA_KIND, json!({}), &colors);
        assert_near(&all, &colors, 1);
        let band = run_bands(
            &mut opened,
            LUMA_KIND,
            json!({"luma_min": 0.3, "luma_max": 0.7}),
            &colors,
        );
        assert_near(&band, &[GONE, GONE, grey(128), GONE, GONE], 1);
        // 128 is 0.502: (0.502 - 0.4) / 0.2 = 0.51 up the smoothstep, 0.515.
        let soft = run_bands(
            &mut opened,
            LUMA_KIND,
            json!({"luma_min": 0.4, "luma_min_smooth": 0.2}),
            &[grey(128), W],
        );
        assert_near(&soft, &[[128, 128, 128, 131], W], 2);
    }
}
