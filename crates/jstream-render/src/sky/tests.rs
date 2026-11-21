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
        deep: super::DeepSky::NONE,
        warp: 0.0,
        travel: 0.0,
        dropping: false,
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

fn night() -> Environment<'static> {
    Environment {
        sky: BLACK,
        horizon: BLACK,
        ground: BLACK,
        ..gradient()
    }
}

/// Pixels brighter than a dim threshold in `shot`.
fn lit(shot: &Snapshot) -> usize {
    shot.rgba()
        .chunks(4)
        .filter(|p| p[0].max(p[1]).max(p[2]) > 30)
        .count()
}

/// A black sky with `nebula` and a `galaxy` band laid out by `seed`.
fn deep(nebula: f32, galaxy: f32, seed: u32) -> SkyDraw<'static> {
    SkyDraw {
        deep: super::DeepSky {
            nebula,
            nebula_colors: [Color::rgb(1.0, 0.2, 0.8), Color::rgb(0.2, 0.5, 1.0)],
            galaxy,
            seed,
        },
        ..sky(night(), SceneCamera::framing(canvas()))
    }
}

/// The sum of every pixel's colour channels.
fn total(shot: &Snapshot) -> u64 {
    shot.rgba()
        .chunks(4)
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum()
}

/// Nebulae and a galaxy band light a black sky where there were no stars; another seed
/// lays them out differently.
#[test]
fn golden_sky_nebulae_and_a_galaxy_band_light_the_sky() {
    let Some(mut opened) = open() else {
        return;
    };
    let clouds = shot(&mut opened.compositor, &deep(1.0, 0.0, 1));
    assert!(
        total(&clouds) > 50_000,
        "the nebulae glow: {}",
        total(&clouds)
    );
    let other = shot(&mut opened.compositor, &deep(1.0, 0.0, 2));
    assert_ne!(clouds.rgba(), other.rgba(), "another seed, another sky");
    // The band crosses the sky somewhere: look along a few directions for it.
    let band: u64 = [
        [0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
    ]
    .into_iter()
    .map(|aim| {
        total(&shot(
            &mut opened.compositor,
            &SkyDraw {
                camera: looking([0.0; 3], aim),
                ..deep(0.0, 1.0, 1)
            },
        ))
    })
    .sum();
    assert!(band > 20_000, "the galaxy band glows: {band}");
    let none = shot(&mut opened.compositor, &deep(0.0, 0.0, 1));
    assert_eq!(total(&none), 0);
}

/// Warp stretches each star into a streak, so more of the sky is lit, looking ahead or
/// to the side; and travel moves the stars.
#[test]
fn golden_sky_warp_streaks_the_stars_at_any_angle() {
    let Some(mut opened) = open() else {
        return;
    };
    let narrow = |aim| SceneCamera {
        fov_deg: Finite32::new(20.0).expect("finite"),
        ..looking([0.0; 3], aim)
    };
    for aim in [[0.3, 0.2, -1.0], [1.0, 0.1, 0.0]] {
        let still = shot(
            &mut opened.compositor,
            &SkyDraw {
                stars: 1.0,
                ..sky(night(), narrow(aim))
            },
        );
        let warp = shot(
            &mut opened.compositor,
            &SkyDraw {
                stars: 1.0,
                warp: 0.5,
                ..sky(night(), narrow(aim))
            },
        );
        assert!(
            lit(&warp) > lit(&still) * 2,
            "looking {aim:?}: {} lit at warp, {} still",
            lit(&warp),
            lit(&still)
        );
        let moved = shot(
            &mut opened.compositor,
            &SkyDraw {
                stars: 1.0,
                travel: 0.05,
                ..sky(night(), narrow(aim))
            },
        );
        assert_ne!(still.rgba(), moved.rgba(), "travel moves the stars");
    }
}

/// At warp the nebulae fade out, the background radiation glows blue ahead, and the
/// stars' light is Doppler shifted: ahead they are bluer and brighter, behind redder and
/// dimmer.
#[test]
fn golden_sky_warp_fades_the_nebulae_and_shifts_the_stars_light() {
    let Some(mut opened) = open() else {
        return;
    };
    // Looking back, where the background radiation doesn't reach.
    let gas = |warp, aim| SkyDraw {
        warp,
        camera: looking([0.0; 3], aim),
        ..deep(1.0, 0.0, 1)
    };
    let back = [0.0, 0.0, 1.0];
    let still = total(&shot(&mut opened.compositor, &gas(0.0, back)));
    let fast = total(&shot(&mut opened.compositor, &gas(0.9, back)));
    assert!(still > 20_000, "the nebulae glow when still: {still}");
    assert!(fast * 20 < still, "they fade at warp: {fast} of {still}");
    // Ahead, the background radiation glows blue.
    let ahead = shot(&mut opened.compositor, &gas(0.9, [0.0, 0.0, -1.0]));
    let [r, _, b] = rgb(&ahead, WIDTH / 2, HEIGHT / 2);
    assert!(b > 40 && b > r * 2, "a blue glow ahead: {:?}", [r, b]);

    let warp = |aim| SkyDraw {
        stars: 1.0,
        warp: 0.8,
        ..sky(night(), looking([0.0; 3], aim))
    };
    let channels = |snapshot: &Snapshot| {
        snapshot.rgba().chunks(4).fold([0u64; 3], |mut sum, p| {
            for (s, v) in sum.iter_mut().zip(p) {
                *s += u64::from(*v);
            }
            sum
        })
    };
    let ahead = channels(&shot(&mut opened.compositor, &warp([0.0, 0.0, -1.0])));
    let behind = channels(&shot(&mut opened.compositor, &warp([0.0, 0.0, 1.0])));
    let sum = |c: [u64; 3]| c.iter().sum::<u64>();
    assert!(
        sum(ahead) > sum(behind) * 3,
        "brighter ahead: {ahead:?} vs {behind:?}"
    );
    assert!(ahead[2] > ahead[0], "bluer ahead: {ahead:?}");
    assert!(behind[0] > behind[2], "redder behind: {behind:?}");
}

/// Coming out of warp the nebulae return early, squeezed toward straight ahead, where
/// going in they are already gone at the same warp.
#[test]
fn golden_sky_dropping_out_of_warp_zooms_the_nebulae_back() {
    let Some(mut opened) = open() else {
        return;
    };
    let at = |dropping| SkyDraw {
        warp: 0.3,
        dropping,
        camera: looking([0.0; 3], [0.0, 0.0, 1.0]),
        ..deep(1.0, 0.0, 1)
    };
    let rising = total(&shot(&mut opened.compositor, &at(false)));
    let dropping = total(&shot(&mut opened.compositor, &at(true)));
    assert!(
        dropping > rising * 5 + 5_000,
        "the nebulae rush back: {dropping} dropping, {rising} rising"
    );
}
