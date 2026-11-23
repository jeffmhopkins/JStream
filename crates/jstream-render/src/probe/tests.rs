//! Probe resampling goldens (ADR-0170 decision 3): six faces resampled into a panorama,
//! placed in a scene and read back.

use jstream_media::FrameSize;

use super::ProbeFace;
use crate::tests::{item, near, open, solid};
use crate::{Color, Compositor, Crop, Image, LegacyTransform, Placement, Scene, Snapshot};

const SIDE: u32 = 16;

/// A face: a square layer of one colour, or of two halves, left and right.
fn face(compositor: &mut Compositor, left: [u8; 4], right: [u8; 4]) -> Image {
    let half = SIDE / 2;
    let a = solid(compositor, half, SIDE, left);
    let b = solid(compositor, half, SIDE, right);
    let items = [
        item(
            &a,
            Placement::Legacy(LegacyTransform::IDENTITY),
            Crop::NONE,
            1.0,
        ),
        item(
            &b,
            Placement::Legacy(LegacyTransform {
                position: [half as f32, 0.0],
                ..LegacyTransform::IDENTITY
            }),
            Crop::NONE,
            1.0,
        ),
    ];
    let scene = Scene {
        canvas: FrameSize::new(SIDE, SIDE).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &items,
    };
    compositor.render_image(&scene).expect("the face renders")
}

fn read(compositor: &mut Compositor, panorama: &Image) -> Snapshot {
    let items = [item(
        panorama,
        Placement::Legacy(LegacyTransform::IDENTITY),
        Crop::NONE,
        1.0,
    )];
    let scene = Scene {
        canvas: FrameSize::new(SIDE * 4, SIDE * 2).expect("non-zero"),
        background: Color::rgb(0.0, 0.0, 0.0),
        items: &items,
    };
    compositor
        .render(&scene, true)
        .expect("the scene renders")
        .pixels()
        .expect("readback was requested")
        .clone()
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];
const CYAN: [u8; 4] = [0, 255, 255, 255];
const MAGENTA: [u8; 4] = [255, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Each direction of the panorama reads the face looking that way: its centre is the
/// face along -z, a quarter right is +x, a quarter left -x, the back is +z, the top -y
/// (up) and the bottom +y; and the face along -z keeps its left on the left.
#[test]
fn each_direction_reads_the_face_looking_that_way() {
    let Some(opened) = open() else { return };
    let mut compositor = opened.compositor;
    let front = face(&mut compositor, RED, WHITE);
    let right = face(&mut compositor, GREEN, GREEN);
    let back = face(&mut compositor, BLUE, BLUE);
    let left = face(&mut compositor, YELLOW, YELLOW);
    let up = face(&mut compositor, CYAN, CYAN);
    let down = face(&mut compositor, MAGENTA, MAGENTA);
    let faces = [
        ProbeFace {
            image: &right,
            axes: [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        },
        ProbeFace {
            image: &left,
            axes: [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
        },
        ProbeFace {
            image: &down,
            axes: [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
        },
        ProbeFace {
            image: &up,
            axes: [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]],
        },
        ProbeFace {
            image: &back,
            axes: [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        },
        ProbeFace {
            image: &front,
            axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
        },
    ];
    let panorama = compositor
        .layer(SIDE * 4, SIDE * 2)
        .expect("a panorama layer");
    compositor
        .resample_probe(&faces, &panorama)
        .expect("the probe resamples");
    let shot = read(&mut compositor, &panorama);
    let mid = SIDE;
    // Just left and right of the centre: the front face's two halves.
    near(&shot, SIDE * 2 - 3, mid, RED, 4);
    near(&shot, SIDE * 2 + 2, mid, WHITE, 4);
    near(&shot, SIDE * 3, mid, GREEN, 4);
    near(&shot, SIDE, mid, YELLOW, 4);
    near(&shot, 1, mid, BLUE, 4);
    near(&shot, SIDE * 4 - 2, mid, BLUE, 4);
    near(&shot, SIDE * 2, 0, CYAN, 4);
    near(&shot, SIDE * 2, SIDE * 2 - 1, MAGENTA, 4);
}

/// A panorama that isn't four faces by two, or faces of different sizes, are refused.
#[test]
fn mismatched_sizes_are_refused() {
    let Some(opened) = open() else { return };
    let mut compositor = opened.compositor;
    let a = face(&mut compositor, RED, RED);
    let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]];
    let faces = [ProbeFace { image: &a, axes }; 6];
    let wrong = compositor.layer(SIDE * 2, SIDE * 2).expect("a layer");
    assert!(compositor.resample_probe(&faces, &wrong).is_err());
    let small = compositor.layer(SIDE / 2, SIDE / 2).expect("a layer");
    let mut mixed = faces;
    mixed[3] = ProbeFace {
        image: &small,
        axes,
    };
    let right = compositor.layer(SIDE * 4, SIDE * 2).expect("a layer");
    assert!(compositor.resample_probe(&mixed, &right).is_err());
    assert!(compositor.resample_probe(&faces, &right).is_ok());
}
