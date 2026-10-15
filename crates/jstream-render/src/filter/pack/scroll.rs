//! `scroll`: moves the picture continuously (tickers, credits). The position is the
//! media-clock time times the speed, so it is the same on every output and a test can
//! drive it with any time. The offset is worked out on the CPU in `f64` and wrapped to
//! the image before it reaches the shader, so it stays exact however long the clock
//! has run (the shader's `frame.time` is an `f32` and would drift by whole pixels after
//! a day). Changing the speed moves the picture to where that speed puts it at the
//! current time.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, boolean, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "scroll";

const SHADER: Shader = Shader {
    label: "filter-scroll",
    source: include_str!("scroll.wgsl"),
    blend: Blend::Replace,
};

/// The fastest scroll, in pixels per second.
const MAX_SPEED: f64 = 2000.0;

pub(super) fn descriptor() -> KindDescriptor {
    let speed = |id: &str, label: &str, text: &str| ParamDescriptor {
        description: Some(text.into()),
        ..float(id, label, -MAX_SPEED, MAX_SPEED, 0.0, "px/s")
    };
    filter(
        KIND,
        "Scroll",
        vec![
            speed(
                "speed_x",
                "Horizontal speed",
                "Positive moves the picture right.",
            ),
            speed(
                "speed_y",
                "Vertical speed",
                "Positive moves the picture down.",
            ),
            ParamDescriptor {
                description: Some(
                    "Wrap the picture around. Off, the area it leaves is transparent.".into(),
                ),
                ..boolean("loop_x", "Loop horizontally", true)
            },
            boolean("loop_y", "Loop vertically", true),
        ],
    )
}

/// One axis's offset in pixels at `seconds`: wrapped into `0..size` when it loops,
/// otherwise held just past the image, where everything reads transparent anyway.
fn offset(seconds: f64, speed: f64, size: u32, looped: bool) -> f32 {
    let size = f64::from(size);
    let moved = seconds * speed;
    if looped {
        moved.rem_euclid(size) as f32
    } else {
        moved.clamp(-size - 1.0, size + 1.0) as f32
    }
}

pub(super) struct Scroll {
    speed: [f64; 2],
    looped: [bool; 2],
}

impl Scroll {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut scroll = Self {
            speed: [0.0; 2],
            looped: [true; 2],
        };
        scroll.update(settings);
        scroll
    }

    fn uniforms(&self, seconds: f64, width: u32, height: u32) -> Vec<u8> {
        let flag = |on: bool| if on { 1.0 } else { 0.0 };
        Uniforms::new()
            .vec2([
                offset(seconds, self.speed[0], width, self.looped[0]),
                offset(seconds, self.speed[1], height, self.looped[1]),
            ])
            .f32(flag(self.looped[0]))
            .f32(flag(self.looped[1]))
            .finish()
    }
}

impl Pass for Scroll {
    fn update(&mut self, settings: &Settings) {
        let read = Read(settings);
        let speed = |id| f64::from(read.f32(id, 0.0)).clamp(-MAX_SPEED, MAX_SPEED);
        self.speed = [speed("speed_x"), speed("speed_y")];
        self.looped = [read.bool("loop_x", true), read.bool("loop_y", true)];
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let params = self.uniforms(ctx.clock().seconds, input.width(), input.height());
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
    use super::{KIND, offset};
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{bands, near, open};
    use serde_json::json;

    #[test]
    fn the_offset_wraps_exactly_however_late() {
        assert_eq!(offset(1.0, 2.0, 8, true), 2.0);
        assert_eq!(offset(1.0, -2.0, 8, true), 6.0);
        assert_eq!(offset(1_000_000.0 + 1.0, 2.0, 8, true), 2.0);
        assert_eq!(offset(0.25, 2.0, 8, true), 0.5);
        assert_eq!(offset(1.0, 2.0, 8, false), 2.0);
        assert_eq!(offset(1e9, 2.0, 8, false), 9.0);
        assert_eq!(offset(1e9, -2.0, 8, false), -9.0);
    }

    #[test]
    fn settings_are_read() {
        let s = super::Scroll::new(&crate::filter::testing::settings(
            KIND,
            json!({"speed_x": 30.0, "loop_y": false}),
        ));
        assert_eq!(s.speed, [30.0, 0.0]);
        assert_eq!(s.looped, [true, false]);
        assert_eq!(s.uniforms(0.5, 100, 10).len(), 16);
    }

    const R: [u8; 4] = [255, 0, 0, 255];
    const G: [u8; 4] = [0, 255, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const W: [u8; 4] = [255, 255, 255, 255];
    const GONE: [u8; 4] = [0, 0, 0, 0];

    /// Each 2-pixel band's left pixel in row 2 of an 8x4 R G B W image after scrolling.
    fn bands_at(overrides: serde_json::Value, seconds: f64) -> Option<Vec<[u8; 4]>> {
        let mut opened = open()?;
        let input = bands(&opened.compositor, 2, 4, &[R, G, B, W]);
        let mut filter = filter(KIND, overrides);
        let s = run(&mut opened, &mut filter, &input, seconds);
        Some((0..4).map(|i| s.pixel(i * 2, 2).expect("inside")).collect())
    }

    fn same(got: &[[u8; 4]], want: &[[u8; 4]]) {
        assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(
                (0..4).all(|c| g[c].abs_diff(w[c]) <= 1),
                "band {i}: {got:?} vs {want:?}"
            );
        }
    }

    #[test]
    fn it_moves_with_the_clock_and_wraps() {
        let Some(still) = bands_at(json!({}), 5.0) else {
            return;
        };
        same(&still, &[R, G, B, W]);
        let right = json!({"speed_x": 2.0});
        same(
            &bands_at(right.clone(), 0.0).unwrap_or_default(),
            &[R, G, B, W],
        );
        same(
            &bands_at(right.clone(), 1.0).unwrap_or_default(),
            &[W, R, G, B],
        );
        same(
            &bands_at(right.clone(), 2.0).unwrap_or_default(),
            &[B, W, R, G],
        );
        // A day and a second later it is exactly where it was after a second.
        same(
            &bands_at(right.clone(), 86_401.0).unwrap_or_default(),
            &[W, R, G, B],
        );
        // Half a pixel: each band's left pixel is half its left neighbour, across the seam too.
        let half = bands_at(right, 0.25).unwrap_or_default();
        same(&half[..1], &[[255, 188, 188, 255]]);
        let left = bands_at(json!({"speed_x": -2.0}), 1.0).unwrap_or_default();
        same(&left, &[G, B, W, R]);
    }

    #[test]
    fn without_loop_the_vacated_area_is_transparent() {
        let Some(got) = bands_at(json!({"speed_x": 2.0, "loop_x": false}), 1.0) else {
            return;
        };
        same(&got, &[GONE, R, G, B]);
        let gone = bands_at(json!({"speed_x": 2.0, "loop_x": false}), 100.0).unwrap_or_default();
        same(&gone, &[GONE; 4]);
    }

    #[test]
    fn it_scrolls_vertically() {
        let Some(mut opened) = open() else { return };
        let input = image(&opened, 2, &[R, R, G, G, B, B, W, W]);
        let mut up = filter(KIND, json!({"speed_y": -1.0}));
        let s = run(&mut opened, &mut up, &input, 1.0);
        for (y, want) in [G, B, W, R].into_iter().enumerate() {
            near(&s, 0, y as u32, want, 1);
        }
        let mut down = filter(KIND, json!({"speed_y": 1.0, "loop_y": false}));
        let s = run(&mut opened, &mut down, &input, 1.0);
        for (y, want) in [GONE, R, G, B].into_iter().enumerate() {
            near(&s, 1, y as u32, want, 1);
        }
    }
}
