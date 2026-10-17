//! `aspect_ratio`: gives the frame a target shape by cropping it, padding it, or
//! stretching it.
//!
//! **Size rule.** Crop and pad keep one input dimension and change the other: crop
//! shrinks the one that is too long, pad grows the one that is too short. Stretch keeps
//! the input's larger dimension as the output's larger one (a 1920x1080 frame stretched
//! to 9:16 is 1080x1920) and derives the other from the ratio. Every derived dimension is
//! rounded to the nearest whole pixel (halves up) and is never 0; the math is in `u64`
//! and saturates at `u32::MAX`, so a size past the adapter's limit reaches layer creation
//! and fails there as `TooLarge`.
//!
//! **Sampling.** Crop and pad map output pixels 1:1 onto input pixels (no scaling), so
//! the anchor picks which pixels survive or where the input sits, and resampling has no
//! effect. Stretch resamples: bilinear through the shared sampler, bicubic as
//! Catmull-Rom (4x4 texel loads), or Lanczos-3 (6x6 texel loads). The kernels are not
//! widened when downscaling, so a large shrink along one axis can alias.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings, VisibleIf};
use serde_json::json;

use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, choice, color, filter, int};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "aspect_ratio";

/// The largest part of a custom ratio.
const MAX_PART: i64 = 10_000;

const SHADER: Shader = Shader {
    label: "filter-aspect-ratio",
    source: include_str!("ratio.wgsl"),
    blend: Blend::Replace,
};

const RATIOS: &[(&str, &str)] = &[
    ("16:9", "16:9 (widescreen)"),
    ("9:16", "9:16 (vertical)"),
    ("4:3", "4:3 (standard)"),
    ("1:1", "1:1 (square)"),
    ("21:9", "21:9 (ultrawide)"),
    ("custom", "Custom"),
];

/// Each anchor's position along x and y, in halves: 0 start, 1 centre, 2 end.
const ANCHORS: &[(&str, &str, [u64; 2])] = &[
    ("center", "Centre", [1, 1]),
    ("top", "Top", [1, 0]),
    ("bottom", "Bottom", [1, 2]),
    ("left", "Left", [0, 1]),
    ("right", "Right", [2, 1]),
    ("top_left", "Top left", [0, 0]),
    ("top_right", "Top right", [2, 0]),
    ("bottom_left", "Bottom left", [0, 2]),
    ("bottom_right", "Bottom right", [2, 2]),
];

fn shown_if(param: ParamDescriptor, on: &str, equals: &str, description: &str) -> ParamDescriptor {
    ParamDescriptor {
        visible_if: Some(VisibleIf {
            param: on.into(),
            equals: json!(equals),
        }),
        ..described(param, description)
    }
}

fn described(param: ParamDescriptor, description: &str) -> ParamDescriptor {
    ParamDescriptor {
        description: Some(description.into()),
        ..param
    }
}

pub(super) fn descriptor() -> KindDescriptor {
    let anchors: Vec<(&str, &str)> = ANCHORS.iter().map(|(v, l, _)| (*v, *l)).collect();
    filter(
        KIND,
        "Aspect ratio",
        vec![
            described(
                choice("ratio", "Ratio", RATIOS, "16:9"),
                "The shape to give the frame, width:height.",
            ),
            shown_if(
                int("custom_width", "Custom width", 1, MAX_PART, 4, ""),
                "ratio",
                "custom",
                "The width part of a custom ratio.",
            ),
            shown_if(
                int("custom_height", "Custom height", 1, MAX_PART, 5, ""),
                "ratio",
                "custom",
                "The height part of a custom ratio.",
            ),
            described(
                choice(
                    "mode",
                    "Mode",
                    &[
                        ("crop", "Crop to fill"),
                        ("pad", "Pad (letterbox or pillarbox)"),
                        ("stretch", "Stretch"),
                    ],
                    "crop",
                ),
                "Crop cuts the long side down, pad adds bars to the short side, stretch \
                 resamples the whole frame.",
            ),
            described(
                choice("anchor", "Anchor", &anchors, "center"),
                "Crop: the part of the frame kept. Pad: where the frame sits among the \
                 bars. Unused by stretch.",
            ),
            shown_if(
                color("pad_color", "Pad colour", true, "#000000ff"),
                "mode",
                "pad",
                "The colour of the bars; may be transparent.",
            ),
            shown_if(
                choice(
                    "resampling",
                    "Resampling",
                    &[
                        ("bilinear", "Bilinear"),
                        ("bicubic", "Bicubic"),
                        ("lanczos", "Lanczos"),
                    ],
                    "bicubic",
                ),
                "mode",
                "stretch",
                "How stretch scales: bilinear, bicubic (Catmull-Rom) or Lanczos-3.",
            ),
        ],
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Crop,
    Pad,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resampling {
    Bilinear,
    Bicubic,
    Lanczos,
}

/// The `aspect_ratio` pass.
#[derive(Clone, Debug)]
pub(super) struct AspectRatio {
    /// Width and height parts, both at least 1.
    ratio: [u64; 2],
    mode: Mode,
    /// In halves, as [`ANCHORS`].
    anchor: [u64; 2],
    /// Premultiplied linear.
    pad: [f32; 4],
    resampling: Resampling,
}

impl AspectRatio {
    pub(super) fn new(settings: &Settings) -> Self {
        let read = Read(settings);
        let part = |id, default| read.i64(id, default).clamp(1, MAX_PART).unsigned_abs();
        let ratio = match read.choice("ratio", "16:9") {
            "9:16" => [9, 16],
            "4:3" => [4, 3],
            "1:1" => [1, 1],
            "21:9" => [21, 9],
            "custom" => [part("custom_width", 4), part("custom_height", 5)],
            _ => [16, 9],
        };
        let mode = match read.choice("mode", "crop") {
            "pad" => Mode::Pad,
            "stretch" => Mode::Stretch,
            _ => Mode::Crop,
        };
        let name = read.choice("anchor", "center");
        let anchor = ANCHORS
            .iter()
            .find(|(v, _, _)| *v == name)
            .map_or([1, 1], |(_, _, a)| *a);
        let [r, g, b, a] = read.color_linear("pad_color", [0.0, 0.0, 0.0, 1.0]);
        let resampling = match read.choice("resampling", "bicubic") {
            "bilinear" => Resampling::Bilinear,
            "lanczos" => Resampling::Lanczos,
            _ => Resampling::Bicubic,
        };
        Self {
            ratio,
            mode,
            anchor,
            pad: [r * a, g * a, b * a, a],
            resampling,
        }
    }

    /// Input pixel = output pixel + this, for crop and pad.
    fn offset(&self, input: (u32, u32), output: (u32, u32)) -> [i32; 2] {
        let axis = |i: u32, o: u32, halves: u64| -> i32 {
            let (i, o) = (u64::from(i), u64::from(o));
            // At most u32::MAX: the difference of two u32s, times at most 2, halved.
            let shift = |d: u64| i64::try_from(d * halves / 2).unwrap_or(i64::MAX);
            let signed = if i >= o { shift(i - o) } else { -shift(o - i) };
            i32::try_from(signed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
                .unwrap_or_default()
        };
        [
            axis(input.0, output.0, self.anchor[0]),
            axis(input.1, output.1, self.anchor[1]),
        ]
    }

    fn uniforms(&self, input: (u32, u32), output: (u32, u32)) -> Vec<u8> {
        let [x, y] = match self.mode {
            Mode::Stretch => [0, 0],
            Mode::Crop | Mode::Pad => self.offset(input, output),
        };
        Uniforms::new()
            .vec4(self.pad)
            .i32(x)
            .i32(y)
            .u32(u32::from(self.mode == Mode::Stretch))
            .u32(match self.resampling {
                Resampling::Bilinear => 0,
                Resampling::Bicubic => 1,
                Resampling::Lanczos => 2,
            })
            .finish()
    }
}

/// `n / d` rounded to the nearest whole number (halves up), at least 1, saturating at
/// `u32::MAX`. `d` is at least 1.
fn scaled(n: u64, d: u64) -> u32 {
    u32::try_from((n + d / 2) / d).unwrap_or(u32::MAX).max(1)
}

impl Pass for AspectRatio {
    fn update(&mut self, settings: &Settings) {
        *self = Self::new(settings);
    }

    fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        let (width, height) = (width.max(1), height.max(1));
        let (w, h) = (u64::from(width), u64::from(height));
        let [rw, rh] = self.ratio;
        // Width for a given height, and height for a given width, at the target ratio.
        let width_for = |h: u64| scaled(h * rw, rh);
        let height_for = |w: u64| scaled(w * rh, rw);
        // The input is wider than the target when w / h > rw / rh.
        let wider = w * rh > h * rw;
        match self.mode {
            Mode::Crop if wider => (width_for(h).min(width), height),
            Mode::Crop => (width, height_for(w).min(height)),
            Mode::Pad if wider => (width, height_for(w).max(height)),
            Mode::Pad => (width_for(h).max(width), height),
            Mode::Stretch => {
                let long = w.max(h);
                let long32 = width.max(height);
                if rw >= rh {
                    (long32, height_for(long))
                } else {
                    (width_for(long), long32)
                }
            }
        }
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let params = self.uniforms(
            (input.width(), input.height()),
            (output.width(), output.height()),
        );
        ctx.draw(&Draw {
            shader: SHADER,
            input,
            aux: None,
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AspectRatio, KIND, Mode};
    use crate::error::RenderError;
    use crate::filter::FilterClock;
    use crate::filter::Pass;
    use crate::filter::testing::{filter, image, run, settings};
    use crate::tests::{bands, near, open};
    use serde_json::{Value, json};

    fn pass(overrides: Value) -> AspectRatio {
        AspectRatio::new(&settings(KIND, overrides))
    }

    fn size(overrides: Value, w: u32, h: u32) -> (u32, u32) {
        pass(overrides).output_size(w, h)
    }

    #[test]
    fn crop_shrinks_the_long_side() {
        let crop = |ratio: &str, w, h| size(json!({"ratio": ratio, "mode": "crop"}), w, h);
        assert_eq!(crop("16:9", 640, 480), (640, 360));
        assert_eq!(crop("9:16", 1920, 1080), (608, 1080)); // 607.5 rounds up
        assert_eq!(crop("4:3", 1920, 1080), (1440, 1080));
        assert_eq!(crop("1:1", 1920, 1080), (1080, 1080));
        assert_eq!(crop("21:9", 1920, 1080), (1920, 823)); // 822.86
        assert_eq!(crop("16:9", 1920, 1080), (1920, 1080));
        assert_eq!(crop("16:9", 1, 1), (1, 1)); // 0.5625 rounds to 1
        assert_eq!(crop("9:16", 3, 1), (1, 1)); // 0.5625, never 0
    }

    #[test]
    fn pad_grows_the_short_side() {
        let pad = |ratio: &str, w, h| size(json!({"ratio": ratio, "mode": "pad"}), w, h);
        assert_eq!(pad("16:9", 640, 480), (853, 480)); // 853.33
        assert_eq!(pad("9:16", 1920, 1080), (1920, 3413)); // 3413.33
        assert_eq!(pad("4:3", 1080, 1920), (2560, 1920));
        assert_eq!(pad("1:1", 1920, 1080), (1920, 1920));
        assert_eq!(pad("16:9", 1920, 1080), (1920, 1080));
        assert_eq!(pad("21:9", 1, 1), (2, 1)); // 2.33
        assert_eq!(pad("16:9", 7, 5), (9, 5)); // 8.89
    }

    #[test]
    fn stretch_keeps_the_larger_dimension() {
        let stretch = |ratio: &str, w, h| size(json!({"ratio": ratio, "mode": "stretch"}), w, h);
        assert_eq!(stretch("9:16", 1920, 1080), (1080, 1920));
        assert_eq!(stretch("16:9", 640, 480), (640, 360));
        assert_eq!(stretch("16:9", 1080, 1920), (1920, 1080));
        assert_eq!(stretch("1:1", 1920, 1080), (1920, 1920));
        assert_eq!(stretch("4:3", 1, 1), (1, 1)); // 0.75
        assert_eq!(stretch("21:9", 1, 1), (1, 1)); // 0.43, never 0
    }

    #[test]
    fn custom_ratios_and_huge_sizes_never_overflow() {
        let custom = |mode: &str, w: i64, h: i64| {
            pass(json!({"ratio": "custom", "custom_width": w, "custom_height": h, "mode": mode}))
        };
        assert_eq!(custom("crop", 4, 5).output_size(1000, 1000), (800, 1000));
        assert_eq!(custom("crop", 1920, 1080).output_size(640, 480), (640, 360));
        let max = u32::MAX;
        assert_eq!(custom("pad", 10_000, 1).output_size(max, max), (max, max));
        assert_eq!(custom("pad", 1, 10_000).output_size(max, 1), (max, max));
        assert_eq!(custom("pad", 10_000, 1).output_size(16_384, 1), (16_384, 2)); // 1.64
        assert_eq!(custom("pad", 10_000, 1).output_size(1, 1), (10_000, 1));
        assert_eq!(
            custom("stretch", 1, 10_000).output_size(max, 1),
            (429_497, max)
        );
        assert_eq!(
            custom("crop", 1, 10_000).output_size(max, max),
            (429_497, max)
        );
        // Out-of-range parts (the graph rejects them) clamp to 1..=10000.
        assert_eq!(custom("crop", 0, -3).output_size(8, 4), (4, 4));
        // A zero input dimension reads as 1.
        assert_eq!(custom("crop", 1, 1).output_size(0, 0), (1, 1));
    }

    #[test]
    fn anchors_and_settings_map_to_uniforms() {
        let p = pass(json!({"mode": "crop", "anchor": "center"}));
        assert_eq!(p.offset((9, 4), (4, 4)), [2, 0]); // odd difference floors
        let p = pass(json!({"mode": "crop", "anchor": "bottom_right"}));
        assert_eq!(p.offset((9, 10), (4, 4)), [5, 6]);
        let p = pass(json!({"mode": "pad", "anchor": "top_left"}));
        assert_eq!(p.offset((4, 4), (9, 4)), [0, 0]);
        let p = pass(json!({"mode": "pad", "anchor": "right"}));
        assert_eq!(p.offset((4, 4), (9, 8)), [-5, -2]);
        let p = pass(json!({"mode": "pad", "pad_color": "#ffffff80"}));
        assert_eq!(p.mode, Mode::Pad);
        let bytes = p.uniforms((4, 4), (8, 4));
        assert_eq!(bytes.len(), 32);
        let f = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4"));
        let i = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().expect("4"));
        assert!((f(12) - 128.0 / 255.0).abs() < 1e-6);
        assert!(
            (f(0) - f(12)).abs() < 1e-6,
            "white is premultiplied by alpha"
        );
        assert_eq!((i(16), i(20), i(24), i(28)), (-2, 0, 0, 1));
        let p = pass(json!({"mode": "stretch", "resampling": "lanczos"}));
        let bytes = p.uniforms((4, 4), (8, 4));
        let i = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().expect("4"));
        assert_eq!((i(16), i(20), i(24), i(28)), (0, 0, 1, 2));
    }

    const R: [u8; 4] = [255, 0, 0, 255];
    const G: [u8; 4] = [0, 255, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const W: [u8; 4] = [255, 255, 255, 255];

    /// Row `y` as one letter per pixel (R, G, B, W, or `?`).
    fn row(snapshot: &crate::Snapshot, y: u32) -> String {
        (0..snapshot.width())
            .map(|x| match snapshot.pixel(x, y).expect("inside") {
                [255, 0, 0, 255] => 'R',
                [0, 255, 0, 255] => 'G',
                [0, 0, 255, 255] => 'B',
                [255, 255, 255, 255] => 'W',
                _ => '?',
            })
            .collect()
    }

    #[test]
    fn crop_keeps_the_anchored_part() {
        let Some(mut opened) = open() else {
            return;
        };
        // 16x9: four 4-pixel bands, cropped to 9x9.
        let input = bands(&opened.compositor, 4, 9, &[R, G, B, W]);
        for (anchor, expected) in [
            ("center", "RGGGGBBBB"),
            ("left", "RRRRGGGGB"),
            ("right", "GBBBBWWWW"),
        ] {
            let mut f = filter(KIND, json!({"ratio": "1:1", "anchor": anchor}));
            let snapshot = run(&mut opened, &mut f, &input, 0.0);
            assert_eq!((snapshot.width(), snapshot.height()), (9, 9));
            assert_eq!(row(&snapshot, 4), expected, "{anchor}");
        }
        // 4x8: four 2-row bands, cropped to 4x4.
        let rows: Vec<[u8; 4]> = [R, G, B, W]
            .iter()
            .flat_map(|c| std::iter::repeat_n(*c, 8))
            .collect();
        let input = image(&opened, 4, &rows);
        for (anchor, expected) in [("top", "RRGG"), ("center", "GGBB"), ("bottom", "BBWW")] {
            let mut f = filter(KIND, json!({"ratio": "1:1", "anchor": anchor}));
            let snapshot = run(&mut opened, &mut f, &input, 0.0);
            assert_eq!((snapshot.width(), snapshot.height()), (4, 4));
            let column: String = (0..4).map(|y| row(&snapshot, y).remove(1)).collect();
            assert_eq!(column, expected, "{anchor}");
        }
    }

    #[test]
    fn pad_draws_bars_at_the_expected_pixels() {
        let Some(mut opened) = open() else {
            return;
        };
        // 12x9 (4:3) red into 16:9: 16x9 with two 2-pixel bars when centred.
        let input = crate::tests::solid(&opened.compositor, 12, 9, R);
        for (anchor, expected) in [
            ("center", "GGRRRRRRRRRRRRGG"),
            ("left", "RRRRRRRRRRRRGGGG"),
            ("bottom_right", "GGGGRRRRRRRRRRRR"),
        ] {
            let mut f = filter(
                KIND,
                json!({"mode": "pad", "anchor": anchor, "pad_color": "#00ff00ff"}),
            );
            let snapshot = run(&mut opened, &mut f, &input, 0.0);
            assert_eq!((snapshot.width(), snapshot.height()), (16, 9));
            for y in [0, 4, 8] {
                assert_eq!(row(&snapshot, y), expected, "{anchor} row {y}");
            }
        }
        // A translucent pad colour; 12x9 into 1:1 is 12x12, bars top and bottom.
        let mut f = filter(
            KIND,
            json!({"ratio": "1:1", "mode": "pad", "pad_color": "#0000ff80"}),
        );
        let snapshot = run(&mut opened, &mut f, &input, 0.0);
        assert_eq!((snapshot.width(), snapshot.height()), (12, 12));
        near(&snapshot, 6, 0, [0, 0, 255, 128], 1);
        near(&snapshot, 6, 1, R, 1);
        near(&snapshot, 6, 9, R, 1);
        near(&snapshot, 6, 10, [0, 0, 255, 128], 1);
        near(&snapshot, 6, 11, [0, 0, 255, 128], 1);
    }

    #[test]
    fn stretch_resamples_with_each_filter() {
        let Some(mut opened) = open() else {
            return;
        };
        // 8x4 (R | G halves) stretched to 1:2 keeps the 8: 4x8, halved across.
        let input = bands(&opened.compositor, 4, 4, &[R, G]);
        for resampling in ["bilinear", "bicubic", "lanczos"] {
            let mut f = filter(
                KIND,
                json!({"ratio": "custom", "custom_width": 1, "custom_height": 2,
                       "mode": "stretch", "resampling": resampling}),
            );
            let snapshot = run(&mut opened, &mut f, &input, 0.0);
            assert_eq!(
                (snapshot.width(), snapshot.height()),
                (4, 8),
                "{resampling}"
            );
            for y in [0, 3, 7] {
                assert_eq!(row(&snapshot, y), "RRGG", "{resampling} row {y}");
            }
        }
    }

    /// Each kernel gives its own value between a black and a white row: a 2x upscale
    /// samples output row 1 a quarter of the way from black to white.
    #[test]
    fn the_kernels_differ_on_an_upscale() {
        let Some(mut opened) = open() else {
            return;
        };
        const K: [u8; 4] = [0, 0, 0, 255];
        let pixels: Vec<[u8; 4]> = [K, W].iter().flat_map(|c| [*c; 8]).collect();
        let input = image(&opened, 8, &pixels);
        let mut value = |resampling: &str| {
            let mut f = filter(
                KIND,
                json!({"ratio": "custom", "custom_width": 2, "custom_height": 1,
                       "mode": "stretch", "resampling": resampling}),
            );
            let snapshot = run(&mut opened, &mut f, &input, 0.0);
            assert_eq!((snapshot.width(), snapshot.height()), (8, 4));
            snapshot.pixel(4, 1).expect("inside")[0]
        };
        // Linear white weights: bilinear 0.25, Catmull-Rom 0.203, Lanczos-3 0.211.
        let (bilinear, bicubic, lanczos) = (value("bilinear"), value("bicubic"), value("lanczos"));
        assert!(bilinear.abs_diff(137) <= 2, "bilinear {bilinear}");
        assert!(bicubic.abs_diff(124) <= 2, "bicubic {bicubic}");
        assert!(lanczos.abs_diff(127) <= 2, "lanczos {lanczos}");
    }

    #[test]
    fn a_one_to_one_stretch_is_the_identity() {
        let Some(mut opened) = open() else {
            return;
        };
        let pixels: Vec<[u8; 4]> = (0u8..16 * 9)
            .map(|i| {
                let (x, y) = (i % 16, i / 16);
                let a = if (x + y) % 5 == 0 { 128 } else { 255 };
                [x * 16, y * 28, x.wrapping_mul(y).wrapping_mul(7), a]
            })
            .collect();
        let input = image(&opened, 16, &pixels);
        for resampling in ["bilinear", "bicubic", "lanczos"] {
            let mut f = filter(KIND, json!({"mode": "stretch", "resampling": resampling}));
            let snapshot = run(&mut opened, &mut f, &input, 0.0);
            assert_eq!((snapshot.width(), snapshot.height()), (16, 9));
            for (i, expected) in pixels.iter().enumerate() {
                let (x, y) = (i as u32 % 16, i as u32 / 16);
                near(&snapshot, x, y, *expected, 2);
            }
        }
    }

    #[test]
    fn a_size_past_the_adapter_limit_errors() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = crate::tests::solid(&opened.compositor, 8, 8, R);
        let mut f = filter(
            KIND,
            json!({"ratio": "custom", "custom_width": 10_000, "custom_height": 1, "mode": "pad"}),
        );
        let result = opened
            .compositor
            .apply_filter(&mut f, &input, FilterClock::default());
        assert!(
            matches!(
                result,
                Err(RenderError::TooLarge {
                    width: 80_000,
                    height: 8,
                    ..
                })
            ),
            "{result:?}"
        );
    }
}
