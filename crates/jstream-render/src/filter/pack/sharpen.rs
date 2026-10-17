//! `sharpen`: an unsharp mask. Each pixel moves away from the mean of the
//! `(2 radius + 1)²` box around it by `amount`. OBS's Sharpen with sharpness `s` is
//! close to amount `4 s` at radius 1.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::filter::params::{Read, Uniforms, filter, float, int};
use crate::filter::{Blend, Shader};

pub(super) const KIND: &str = "sharpen";

pub(super) const SHADER: Shader = Shader {
    label: "filter-sharpen",
    source: include_str!("sharpen.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Sharpen",
        vec![
            ParamDescriptor {
                description: Some(
                    "How far each pixel is pushed away from its neighbourhood's mean. 0 is \
                     unchanged."
                        .into(),
                ),
                ..float("amount", "Amount", 0.0, 4.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "The neighbourhood's half-width. Larger sharpens coarser detail.".into(),
                ),
                ..int("radius", "Radius", 1, 3, 1, "px")
            },
        ],
    )
}

/// `(amount, radius)` from the settings.
fn read(settings: &Settings) -> (f32, i32) {
    let read = Read(settings);
    let amount = read.f32("amount", 0.5).clamp(0.0, 4.0);
    let radius = read.i64("radius", 1).clamp(1, 3) as i32;
    (amount, radius)
}

pub(super) fn uniforms(settings: &Settings) -> Vec<u8> {
    let (amount, radius) = read(settings);
    Uniforms::new().f32(amount).i32(radius).finish()
}

#[cfg(test)]
mod tests {
    use super::KIND;
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{bands, near, open};
    use serde_json::json;

    #[test]
    fn settings_are_read_and_clamped() {
        assert_eq!(super::read(&settings(KIND, json!({}))), (0.5, 1));
        assert_eq!(
            super::read(&settings(KIND, json!({"amount": 2.0, "radius": 3}))),
            (2.0, 3)
        );
        let mut wild = settings(KIND, json!({}));
        wild.0.insert("radius".into(), json!(9));
        assert_eq!(super::read(&wild).1, 3);
        assert_eq!(super::uniforms(&wild).len(), 16);
    }

    const DARK: [u8; 4] = [64, 64, 64, 255];
    const LIGHT: [u8; 4] = [192, 192, 192, 255];

    /// The row through an 8x4 image, dark on the left half and light on the right.
    fn row(overrides: serde_json::Value) -> Option<Vec<[u8; 4]>> {
        let mut opened = open()?;
        let input = bands(&opened.compositor, 4, 4, &[DARK, LIGHT]);
        let mut filter = filter(KIND, overrides);
        let snapshot = run(&mut opened, &mut filter, &input, 0.0);
        Some(
            (0..8)
                .map(|x| snapshot.pixel(x, 2).expect("inside"))
                .collect(),
        )
    }

    #[test]
    fn an_edge_gets_a_halo_on_both_sides() {
        let Some(row) = row(json!({})) else { return };
        // Mean of the 3x3 left of the edge: (2·64 + 192) / 3; 64 + 0.5 (64 - 106.7) = 43.
        let want = [
            DARK,
            DARK,
            DARK,
            [43, 43, 43, 255],
            [213, 213, 213, 255],
            LIGHT,
            LIGHT,
            LIGHT,
        ];
        for (x, (got, want)) in row.iter().zip(want).enumerate() {
            assert!(
                (0..4).all(|c| got[c].abs_diff(want[c]) <= 1),
                "x {x}: {got:?} vs {want:?}"
            );
        }
    }

    #[test]
    fn radius_widens_and_zero_amount_keeps_the_picture() {
        let Some(wide) = row(json!({"radius": 2})) else {
            return;
        };
        // Mean of the 5x5 at x 3: (3·64 + 2·192) / 5 = 115.2; 64 - 0.5 · 51.2 = 38.4.
        assert!(wide[3][0].abs_diff(38) <= 1, "{wide:?}");
        assert!(wide[2][0].abs_diff(51) <= 1, "{wide:?}");
        let Some(flat) = row(json!({"amount": 0.0})) else {
            return;
        };
        for (x, got) in flat.iter().enumerate() {
            let want = if x < 4 { DARK } else { LIGHT };
            assert!(
                (0..4).all(|c| got[c].abs_diff(want[c]) <= 1),
                "x {x}: {got:?}"
            );
        }
    }

    #[test]
    fn a_transparent_edge_keeps_its_alpha() {
        let Some(mut opened) = open() else { return };
        let input = bands(
            &opened.compositor,
            4,
            4,
            &[[0, 0, 0, 0], [200, 50, 50, 255]],
        );
        let mut f = filter(KIND, json!({"amount": 1.0}));
        let snapshot = run(&mut opened, &mut f, &input, 0.0);
        near(&snapshot, 3, 2, [0, 0, 0, 0], 0);
        let edge = snapshot.pixel(4, 2).expect("inside");
        assert_eq!(edge[3], 255, "{edge:?}");
    }
}
