//! Shader-source programs on the test adapter: sizes, passes, feedback, images, slots
//! and the `sys` uniform.

use std::sync::Arc;

use jstream_protocol::Settings;

use super::{ExternalProgram, ProgramAudio, ProgramImage, ProgramInput, ProgramPass};
use crate::filter::testing::{image, read, run};
use crate::filter::{FilterClock, VideoFilter};
use crate::tests::{near, open};

struct Fake {
    passes: Vec<ProgramPass>,
    images: Vec<ProgramImage>,
    size: (u32, u32),
}

impl ExternalProgram for Fake {
    fn label(&self) -> &str {
        "test/program"
    }

    fn passes(&self) -> &[ProgramPass] {
        &self.passes
    }

    fn images(&self) -> &[ProgramImage] {
        &self.images
    }

    fn uniforms(&self, _: &Settings) -> Vec<u8> {
        vec![0; 16]
    }

    fn size(&self, _: &Settings) -> (u32, u32) {
        self.size
    }
}

/// A pass whose WGSL declares one texture per input (`in0`, `in1`, …) before `body`.
fn pass(inputs: Vec<ProgramInput>, scale: f32, body: &str) -> ProgramPass {
    let mut wgsl = String::new();
    for n in 0..inputs.len() {
        wgsl.push_str(&format!(
            "@group(0) @binding({}) var in{n}: texture_2d<f32>;\n",
            super::FIRST_INPUT_BINDING + n as u32
        ));
    }
    wgsl.push_str(body);
    ProgramPass {
        wgsl: wgsl.into(),
        scale,
        inputs,
    }
}

fn program(passes: Vec<ProgramPass>, size: (u32, u32)) -> VideoFilter {
    VideoFilter::program(
        "test/program",
        Arc::new(Fake {
            passes,
            images: Vec::new(),
            size,
        }),
        &Settings::default(),
    )
}

const RED: &str = "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(1.0, 0.0, 0.0, 1.0); }";

/// Reads input 0 at this pixel's UV.
const READ0: &str = "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { \
    return to_premul_linear(textureSampleLevel(in0, lin, in.uv, 0.0), input_alpha(0u)); }";

#[test]
fn a_program_draws_at_its_own_size_whatever_its_input() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 255, 255]]);
    let mut filter = program(vec![pass(vec![], 1.0, RED)], (6, 4));
    assert_eq!(filter.output_size(1, 1), (6, 4));
    let shot = run(&mut opened, &mut filter, &input, 0.0);
    assert_eq!((shot.width(), shot.height()), (6, 4));
    near(&shot, 5, 3, [255, 0, 0, 255], 1);
}

#[test]
fn a_later_pass_reads_an_earlier_one_from_the_same_frame() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    let mut filter = program(
        vec![
            pass(vec![], 0.5, RED),
            pass(vec![ProgramInput::Pass(0)], 1.0, READ0),
        ],
        (8, 8),
    );
    let shot = run(&mut opened, &mut filter, &input, 0.0);
    near(&shot, 4, 4, [255, 0, 0, 255], 1);
}

/// Each frame adds a quarter of opaque white to what the pass drew the frame before.
const ACCUMULATE: &str = "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { \
    let prev = to_premul_linear(textureLoad(in0, vec2<i32>(0), 0), input_alpha(0u)); \
    return min(prev + vec4<f32>(0.25), vec4<f32>(1.0)); }";

#[test]
fn feedback_reads_the_frame_before_and_restart_clears_it() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    let mut filter = program(
        vec![pass(vec![ProgramInput::Pass(0)], 1.0, ACCUMULATE)],
        (2, 2),
    );
    let alpha = |opened: &mut _, filter: &mut VideoFilter| {
        run(opened, filter, &input, 0.0)
            .pixel(0, 0)
            .expect("inside")[3]
    };
    assert!(alpha(&mut opened, &mut filter).abs_diff(64) <= 1);
    assert!(alpha(&mut opened, &mut filter).abs_diff(128) <= 1);
    assert!(alpha(&mut opened, &mut filter).abs_diff(191) <= 1);
    filter.restart();
    assert!(alpha(&mut opened, &mut filter).abs_diff(64) <= 1);
}

#[test]
fn feedback_on_an_earlier_pass_reads_its_frame_before() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    // Pass 0 reads pass 1 (listed later: last frame's), and pass 1 copies pass 0.
    let mut filter = program(
        vec![
            pass(vec![ProgramInput::Pass(1)], 1.0, ACCUMULATE),
            pass(vec![ProgramInput::Pass(0)], 1.0, READ0),
        ],
        (2, 2),
    );
    let mut alphas = Vec::new();
    for _ in 0..3 {
        alphas.push(
            run(&mut opened, &mut filter, &input, 0.0)
                .pixel(1, 1)
                .expect("in")[3],
        );
    }
    for (got, want) in alphas.into_iter().zip([64u8, 128, 191]) {
        assert!(got.abs_diff(want) <= 1, "{got} vs {want}");
    }
}

#[test]
fn a_program_reads_its_images_and_filled_slots() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    let fake = Fake {
        passes: vec![pass(
            vec![ProgramInput::Image(0), ProgramInput::Slot(0)],
            1.0,
            "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { \
             let a = to_premul_linear(textureLoad(in0, vec2<i32>(0), 0), input_alpha(0u)); \
             let b = to_premul_linear(textureLoad(in1, vec2<i32>(0), 0), input_alpha(1u)); \
             return select(a, b, in.uv.x > 0.5); }",
        )],
        images: vec![ProgramImage {
            width: 1,
            height: 1,
            rgba: Arc::from([0u8, 255, 0, 255].as_slice()),
        }],
        size: (4, 1),
    };
    let mut filter = VideoFilter::program("test/program", Arc::new(fake), &Settings::default());
    let shot = run(&mut opened, &mut filter, &input, 0.0);
    near(&shot, 0, 0, [0, 255, 0, 255], 1);
    // An empty slot reads transparent.
    near(&shot, 3, 0, [0, 0, 0, 0], 1);
    let blue = image(&opened, 1, &[[0, 0, 255, 255]]);
    filter.set_input(0, Some(Arc::new(blue)));
    let shot = run(&mut opened, &mut filter, &input, 0.0);
    near(&shot, 3, 0, [0, 0, 255, 255], 1);
}

#[test]
fn sys_holds_time_since_start_the_frame_count_and_audio() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    // Alpha carries one value per column: time / 4, frame / 4, level, the pass size / 8.
    let mut filter = program(
        vec![pass(
            vec![],
            1.0,
            "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { \
             let x = i32(floor(in.pos.x)); \
             var a = sys.time / 4.0; \
             if (x == 1) { a = f32(sys.frame) / 4.0; } \
             if (x == 2) { a = sys.level; } \
             if (x == 3) { a = sys.pass_size.x / 8.0; } \
             return vec4<f32>(a); }",
        )],
        (4, 1),
    );
    let at = |opened: &mut _, filter: &mut VideoFilter, seconds: f64| {
        let shot = run(opened, filter, &input, seconds);
        (0..4)
            .map(|x| shot.pixel(x, 0).expect("in")[3])
            .collect::<Vec<_>>()
    };
    let first = at(&mut opened, &mut filter, 10.0);
    assert!(first[0] <= 1 && first[1] <= 1 && first[2] <= 1, "{first:?}");
    assert!(first[3].abs_diff(128) <= 1, "{first:?}");
    filter.set_audio(ProgramAudio {
        level: 0.5,
        peak: 1.0,
        ..ProgramAudio::default()
    });
    let second = at(&mut opened, &mut filter, 11.0);
    assert!(second[0].abs_diff(64) <= 1, "{second:?}");
    assert!(second[1].abs_diff(64) <= 1, "{second:?}");
    assert!(second[2].abs_diff(128) <= 1, "{second:?}");
}

/// The audio data image holds the bands in row 0 and the waveform in row 1, silent until
/// the first `set_audio`.
#[test]
fn a_program_reads_the_audio_image() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    // Column 0: band 3; column 1: wave sample 5, as stored.
    let mut filter = program(
        vec![pass(
            vec![ProgramInput::Audio],
            1.0,
            "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { \
             let x = i32(floor(in.pos.x)); \
             var a = textureLoad(in0, vec2<i32>(3, 0), 0).r; \
             if (x == 1) { a = textureLoad(in0, vec2<i32>(5, 1), 0).r; } \
             return vec4<f32>(a); }",
        )],
        (2, 1),
    );
    let alphas = |opened: &mut _, filter: &mut VideoFilter| {
        let shot = run(opened, filter, &input, 0.0);
        [0, 1].map(|x| shot.pixel(x, 0).expect("in")[3])
    };
    let silent = alphas(&mut opened, &mut filter);
    assert!(silent[0] <= 1 && silent[1].abs_diff(128) <= 1, "{silent:?}");
    let mut audio = ProgramAudio::default();
    audio.bands[3] = 1.0;
    audio.wave[5] = -1.0;
    filter.set_audio(audio);
    let heard = alphas(&mut opened, &mut filter);
    assert!(heard[0] >= 254 && heard[1] <= 1, "{heard:?}");
}

#[test]
fn a_refused_pass_is_an_error_not_a_panic() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    let mut filter = program(
        vec![pass(
            vec![],
            1.0,
            "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return nope; }",
        )],
        (2, 2),
    );
    let clock = FilterClock::default();
    assert!(
        opened
            .compositor
            .apply_filter(&mut filter, &input, clock)
            .is_err()
    );
    // The refusal is kept: the second draw fails the same way.
    assert!(
        opened
            .compositor
            .apply_filter(&mut filter, &input, clock)
            .is_err()
    );
}

#[test]
fn a_program_draws_into_a_kept_layer_like_any_filter() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    let mut filter = program(
        vec![pass(vec![ProgramInput::Pass(0)], 1.0, ACCUMULATE)],
        (2, 2),
    );
    let output = opened.compositor.layer(2, 2).expect("layer");
    for _ in 0..2 {
        opened
            .compositor
            .apply_filter_into(&mut filter, &input, &output, FilterClock::default())
            .expect("draws");
    }
    let shot = read(&mut opened, &output);
    assert!(shot.pixel(0, 0).expect("in")[3].abs_diff(128) <= 1);
}

#[test]
fn a_program_over_its_memory_cap_draws_nothing() {
    let Some(mut opened) = open() else { return };
    let input = image(&opened, 1, &[[0, 0, 0, 0]]);
    // Eight feedback passes at 4096 by 4096: 2 GiB of layers.
    let passes = (0..8)
        .map(|n| pass(vec![ProgramInput::Pass(n)], 1.0, ACCUMULATE))
        .collect();
    let mut filter = program(passes, (4096, 4096));
    let result = opened
        .compositor
        .apply_filter(&mut filter, &input, FilterClock::default());
    let error = result.expect_err("over the cap").to_string();
    assert!(error.contains("MiB"), "{error}");
}
