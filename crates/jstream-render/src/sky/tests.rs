//! Sky backdrop goldens (ADR-0167): the sky drawn into a layer through a camera, placed
//! under a scene and read back.

use jstream_media::FrameSize;
use jstream_protocol::{Canvas, Finite32, SceneCamera, Vec3};

use super::SkyDraw;
use crate::model::Environment;
use crate::tests::open;
use crate::{Color, Compositor, Crop, Image, LegacyTransform, Placement, Scene, Snapshot};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 100;

fn canvas() -> Canvas {
    Canvas {
        width: WIDTH,
        height: HEIGHT,
    }
}

fn vec3(x: f32, y: f32, z: f32) -> Vec3 {
    let f = |v: f32| Finite32::new(v).expect("finite");
    Vec3 {
        x: f(x),
        y: f(y),
        z: f(z),
    }
}

/// A camera at `position` looking along `aim`.
fn looking(position: [f32; 3], aim: [f32; 3]) -> SceneCamera {
    let [x, y, z] = position;
    SceneCamera {
        position: vec3(x, y, z),
        target: vec3(x + aim[0] * 500.0, y + aim[1] * 500.0, z + aim[2] * 500.0),
        ..SceneCamera::framing(canvas())
    }
}

const BLACK: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

fn gradient() -> Environment<'static> {
    Environment {
        sky: Color {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        },
        horizon: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        },
        ground: Color {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            a: 1.0,
        },
        intensity: 1.0,
        turn_deg: 0.0,
        image: None,
    }
}

fn sky(environment: Environment<'_>, camera: SceneCamera) -> SkyDraw<'_> {
    SkyDraw {
        environment,
        camera,
        stars: 0.0,
        star_brightness: 1.0,
        opacity: 1.0,
    }
}

/// Draws `draw` into a layer, places it over a red background and reads the scene back.
fn shot(compositor: &mut Compositor, draw: &SkyDraw<'_>) -> Snapshot {
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor.draw_sky(draw, &layer).expect("the sky draws");
    let items = [crate::tests::item(
        &layer,
        Placement::Legacy(LegacyTransform::IDENTITY),
        Crop::NONE,
        1.0,
    )];
    let scene = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::rgb(1.0, 0.0, 0.0),
        items: &items,
    };
    compositor
        .render(&scene, true)
        .expect("the scene renders")
        .pixels()
        .expect("readback was requested")
        .clone()
}

fn rgb(snapshot: &Snapshot, x: u32, y: u32) -> [u8; 3] {
    let p = snapshot.pixel(x, y).expect("inside the canvas");
    [p[0], p[1], p[2]]
}

/// A 16 by 2 panorama: four colours around the horizon, a quarter turn each, four
/// columns wide, so linear filtering inside a quarter reads its own colour.
fn quarters(compositor: &Compositor) -> Image {
    let colours: [[u8; 4]; 4] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
    ];
    let mut rgba = Vec::new();
    for _ in 0..2 {
        for colour in colours {
            for _ in 0..4 {
                rgba.extend_from_slice(&colour);
            }
        }
    }
    compositor
        .upload_rgba8(16, 2, &rgba)
        .expect("the panorama uploads")
}

/// The framing camera looks level, so the horizon runs across the middle of the
/// picture: the sky colour above, toward blue at the top, and the ground below.
#[test]
fn golden_sky_a_level_camera_sees_the_horizon_across_the_middle() {
    let Some(mut opened) = open() else {
        return;
    };
    let camera = SceneCamera::framing(canvas());
    let got = shot(&mut opened.compositor, &sky(gradient(), camera));
    let middle = rgb(&got, 80, 50);
    assert!(
        middle.iter().all(|&c| c > 200),
        "the horizon is white: {middle:?}"
    );
    let top = rgb(&got, 80, 0);
    assert!(top[2] > top[0] + 20, "bluer above: {top:?}");
    let bottom = rgb(&got, 80, 99);
    assert!(bottom[1] > bottom[0] + 100, "the ground below: {bottom:?}");
    // Nothing of the red background shows through.
    assert!(top[0] < 250 || top[1] > 0, "{top:?}");
}

/// Infinitely far away: moving the camera without turning it leaves every pixel as it
/// was, stars included.
#[test]
fn golden_sky_moving_the_camera_does_not_move_the_sky() {
    let Some(mut opened) = open() else {
        return;
    };
    let picture = quarters(&opened.compositor);
    let environment = Environment {
        image: Some(&picture),
        ..gradient()
    };
    let draw = |camera| SkyDraw {
        stars: 1.0,
        ..sky(environment, camera)
    };
    let here = shot(
        &mut opened.compositor,
        &draw(looking([0.0, 0.0, 0.0], [0.3, -0.2, -1.0])),
    );
    let there = shot(
        &mut opened.compositor,
        &draw(looking([5000.0, -800.0, 12000.0], [0.3, -0.2, -1.0])),
    );
    assert_eq!(here.rgba(), there.rgba());
}

/// Turning the camera turns the sky: a quarter turn right shows the next quarter of the
/// panorama, and the light's turn spins it.
#[test]
fn golden_sky_turning_the_camera_turns_the_sky() {
    let Some(mut opened) = open() else {
        return;
    };
    let picture = quarters(&opened.compositor);
    let environment = Environment {
        image: Some(&picture),
        ..gradient()
    };
    // Ahead (-z) is the panorama's centre, the seam between its second and third
    // quarters; a little right of it is the third, blue.
    let ahead = shot(
        &mut opened.compositor,
        &sky(environment, looking([0.0; 3], [0.2, 0.0, -1.0])),
    );
    assert_eq!(rgb(&ahead, 80, 50), [0, 0, 255]);
    // Looking right (+x) is a quarter of the way on: the fourth quarter, yellow.
    let right = shot(
        &mut opened.compositor,
        &sky(environment, looking([0.0; 3], [1.0, 0.0, 0.2])),
    );
    assert_eq!(rgb(&right, 80, 50), [255, 255, 0]);
    // The light's turn spins the sky about the vertical the other way: turned a
    // quarter, the camera looking right sees the first quarter, red.
    let turned = Environment {
        turn_deg: 90.0,
        ..environment
    };
    let spun = shot(
        &mut opened.compositor,
        &sky(turned, looking([0.0; 3], [1.0, 0.0, 0.2])),
    );
    assert_eq!(rgb(&spun, 80, 50), [255, 0, 0]);
}

/// Stars: none at 0; at 1, scattered points brighter than the black sky they are on,
/// most of the sky still black. A narrow view keeps a star to a few of this small
/// canvas's pixels, as it is at full size.
#[test]
fn golden_sky_stars_scatter_points_over_the_sky() {
    let Some(mut opened) = open() else {
        return;
    };
    let night = Environment {
        sky: BLACK,
        horizon: BLACK,
        ground: BLACK,
        ..gradient()
    };
    let camera = SceneCamera {
        fov_deg: Finite32::new(15.0).expect("finite"),
        ..SceneCamera::framing(canvas())
    };
    let dark = shot(&mut opened.compositor, &sky(night, camera));
    assert!(dark.rgba().chunks(4).all(|p| p[..3] == [0, 0, 0]));
    let starry = shot(
        &mut opened.compositor,
        &SkyDraw {
            stars: 1.0,
            ..sky(night, camera)
        },
    );
    let lit = starry
        .rgba()
        .chunks(4)
        .filter(|p| p[0].max(p[1]).max(p[2]) > 40)
        .count();
    let total = (WIDTH * HEIGHT) as usize;
    assert!(lit >= 5, "some stars show: {lit}");
    assert!(
        lit * 4 < total,
        "most of the sky stays dark: {lit} of {total}"
    );
}

/// The backdrop's opacity lets the scene's background show through it.
#[test]
fn golden_sky_its_opacity_fades_it_over_the_background() {
    let Some(mut opened) = open() else {
        return;
    };
    let night = Environment {
        sky: BLACK,
        horizon: BLACK,
        ground: BLACK,
        ..gradient()
    };
    let camera = SceneCamera::framing(canvas());
    let half = shot(
        &mut opened.compositor,
        &SkyDraw {
            opacity: 0.5,
            ..sky(night, camera)
        },
    );
    let got = rgb(&half, 80, 50);
    assert!(
        (100..=200).contains(&got[0]) && got[1] < 5,
        "half the red background: {got:?}"
    );
}
