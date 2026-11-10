//! R-RT-005: what the compositor costs per frame on the adapter it finds. Run on the
//! machine the numbers are for, and state that machine with them:
//!
//! ```text
//! cargo bench -p jstream-render --bench composite
//! ```
//!
//! `OpenOptions::hardware()` takes the GPU when there is one, else a software adapter
//! (lavapipe, WARP), as CI has. On a software adapter the numbers are CPU rasterisation,
//! not GPU cost: they track regressions in what the compositor submits, never a GPU
//! budget. With no adapter at all the benchmarks are
//! skipped with a line on stderr, except under `CI`, where that panics.
//!
//! - `composite_1080p_readback`: a 1920x1080 frame of N 1280x720 items, with the readback
//!   that `video.snapshot` asks for, so the time includes the GPU finishing the frame.
//! - `composite_1080p_submit`: the same frame without readback, then a wait for the queue
//!   to drain: the encoder path.
//! - `composite_1080p_blend`: the submit frame of 4 items, each drawn with one blend
//!   mode: `normal`, `multiply` (two fixed-function draws) and `overlay` (a backdrop
//!   copy and a pass per item, ADR-0082).
//! - `composite_1080p_camera`: the submit frame of N items in a 3D scene (ADR-0094): each
//!   turned and at its own depth, seen through an off-axis camera and sorted far to near,
//!   so every item takes the 3D path (its mip chain, anisotropic sampling, edge coverage).
//! - `model_1080p`: one built-in sphere drawn into a 1080p layer through that camera,
//!   lit by the studio rig, then placed in the scene (ADR-0107), the per-frame cost of a
//!   model; the same with a cube, the cheapest closed shape; and the sphere lit by a
//!   spot light that casts shadows, its 2048² shadow map drawn each frame (ADR-0151).
//! - `depth_fog_1080p_camera`: what a depth-aware scene filter adds to N of those
//!   cards (ADR-0143): the scene's view depth, then `depth_fog` over its composite;
//!   `ambient_occlusion_1080p_camera`, `volumetric_fog_1080p_camera` and
//!   `depth_bloom_1080p_camera` the same with
//!   `ambient_occlusion`, `volumetric_fog` and `depth_bloom` (ADR-0151).
//! - `rewrite_rgba8_1080p`: refilling a 1080p upload in place (the bytes copied into a
//!   buffer, then premultiplied in linear light on the GPU), as an image or media source
//!   does on a new frame: `rewrite` with the GPU's work waited for, `rewrite_call` the
//!   calling thread's part alone, which is what a render tick pays.

use std::hint::black_box;
use std::io::Write;

use criterion::{BenchmarkId, Criterion, criterion_main};
use jstream_media::FrameSize;
use jstream_media::model::Material;
use jstream_media::model::shapes::Shape;
use jstream_protocol::{Finite32, ItemGeometry, SceneCamera, Size, Vec2, Vec3};
use jstream_render::{
    BlendMode, Color, Compositor, Crop, FilterClock, Image, Item, LegacyTransform, Lighting,
    ModelDraw, OpenOptions, Placement, Scene, VideoFilter, ViewDepth,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const ITEM_WIDTH: u32 = 1280;
const ITEM_HEIGHT: u32 = 720;
const ITEM_COUNTS: [usize; 3] = [1, 4, 16];

fn open() -> Option<Compositor> {
    match Compositor::open(OpenOptions::hardware()) {
        Ok(compositor) => {
            let _ = writeln!(
                std::io::stderr(),
                "jstream-render bench adapter: {}",
                compositor.adapter_description()
            );
            Some(compositor)
        }
        Err(err) if err.is_no_adapter() => {
            // As in the crate's tests: a silent skip would leave `bench` green with no
            // render rows.
            if std::env::var_os("CI").is_some() {
                panic!("CI has no wgpu adapter: {err}");
            }
            let _ = writeln!(
                std::io::stderr(),
                "jstream-render: skipping benchmarks, no adapter: {err}"
            );
            None
        }
        Err(err) => panic!("opening the compositor failed: {err}"),
    }
}

/// A straight-alpha gradient, so the premultiply and filtering do real work.
fn pixels(width: u32, height: u32, seed: u32) -> Vec<u8> {
    (0..height)
        .flat_map(|y| {
            (0..width).flat_map(move |x| {
                [
                    ((x + seed) % 256) as u8,
                    ((y + seed) % 256) as u8,
                    ((x + y) % 256) as u8,
                    200,
                ]
            })
        })
        .collect()
}

fn images(compositor: &Compositor, count: usize) -> Vec<Image> {
    (0..count)
        .map(|i| {
            compositor
                .upload_rgba8(
                    ITEM_WIDTH,
                    ITEM_HEIGHT,
                    &pixels(ITEM_WIDTH, ITEM_HEIGHT, i as u32 * 37),
                )
                .expect("invariant: the image bytes match")
        })
        .collect()
}

/// Items spread over the canvas, each a little rotated and scaled so sampling is not
/// one texel per pixel, at 0.8 opacity so every one blends.
fn items(images: &[Image]) -> Vec<Item<'_>> {
    blended(images, BlendMode::Normal)
}

/// [`items`], each drawn with `blend`.
fn blended(images: &[Image], blend: BlendMode) -> Vec<Item<'_>> {
    images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            let step = i as f32;
            Item {
                image,
                placement: Placement::Legacy(LegacyTransform {
                    position: [(step * 41.0) % 640.0, (step * 23.0) % 360.0],
                    scale: [0.9 + 0.01 * step, 0.9 + 0.01 * step],
                    rotation_deg: (step + 1.0) * 1.5,
                }),
                crop: Crop::NONE,
                opacity: 0.8,
                blend,
                visible: true,
                camera: None,
                depth_run: 0,
            }
        })
        .collect()
}

fn scene<'a>(items: &'a [Item<'a>]) -> Scene<'a> {
    Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("invariant: the canvas is non-zero"),
        background: Color::rgb(0.1, 0.1, 0.1),
        items,
    }
}

fn wait(compositor: &Compositor) {
    compositor
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the queue drains");
}

fn composite(c: &mut Criterion) {
    let Some(mut compositor) = open() else {
        return;
    };
    let max = ITEM_COUNTS.iter().copied().max().unwrap_or(1);
    let images = images(&compositor, max);
    wait(&compositor);

    let mut group = c.benchmark_group("composite_1080p_readback");
    group.sample_size(20);
    for count in ITEM_COUNTS {
        let items = items(&images[..count]);
        let scene = scene(&items);
        group.bench_with_input(BenchmarkId::new("items", count), &scene, |b, scene| {
            b.iter(|| {
                let output = compositor.render(scene, true).expect("render");
                black_box(output.pixels().map(|p| p.rgba().len()));
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("composite_1080p_submit");
    group.sample_size(20);
    for count in ITEM_COUNTS {
        let items = items(&images[..count]);
        let scene = scene(&items);
        group.bench_with_input(BenchmarkId::new("items", count), &scene, |b, scene| {
            b.iter(|| {
                black_box(compositor.render(scene, false).expect("render"));
                wait(&compositor);
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("composite_1080p_blend");
    group.sample_size(20);
    for mode in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Overlay] {
        let items = blended(&images[..4], mode);
        let scene = scene(&items);
        group.bench_with_input(
            BenchmarkId::new("mode", mode.as_str()),
            &scene,
            |b, scene| {
                b.iter(|| {
                    black_box(compositor.render(scene, false).expect("render"));
                    wait(&compositor);
                });
            },
        );
    }
    group.finish();
}

/// The camera the 3D benches look through: up and to the left of the framing camera,
/// looking at the canvas centre.
fn camera() -> SceneCamera {
    let framing = SceneCamera::framing(jstream_protocol::Canvas {
        width: WIDTH,
        height: HEIGHT,
    });
    SceneCamera {
        position: Vec3::new(600.0, 200.0, framing.position.z.get() * 0.9)
            .expect("invariant: finite"),
        ..framing
    }
}

fn geometry(x: f32, y: f32, size: f32) -> ItemGeometry {
    let center = Vec2::new(x, y).expect("invariant: finite");
    let size = Size::try_new(size, size * 9.0 / 16.0).expect("invariant: positive");
    ItemGeometry::try_new(center, size).expect("invariant: a box")
}

/// [`items`] as cards in a 3D scene: each turned a little and at its own depth.
fn cards(images: &[Image]) -> Vec<Item<'_>> {
    let camera = camera();
    images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            let step = i as f32;
            let mut placed = geometry(
                400.0 + (step * 83.0) % 1100.0,
                300.0 + (step * 47.0) % 500.0,
                640.0,
            );
            placed.turn_deg = Finite32::new(10.0 + step * 3.0).expect("invariant: finite");
            placed.depth_px = Finite32::new(-(step * 60.0)).expect("invariant: finite");
            Item {
                image,
                placement: Placement::Geometry(placed),
                crop: Crop::NONE,
                opacity: 0.8,
                blend: BlendMode::Normal,
                visible: true,
                camera: Some(camera),
                depth_run: 0,
            }
        })
        .collect()
}

fn three_d(c: &mut Criterion) {
    let Some(mut compositor) = open() else {
        return;
    };
    let max = ITEM_COUNTS.iter().copied().max().unwrap_or(1);
    let images = images(&compositor, max);
    wait(&compositor);
    let mut group = c.benchmark_group("composite_1080p_camera");
    group.sample_size(20);
    for count in ITEM_COUNTS {
        let items = cards(&images[..count]);
        let scene = scene(&items);
        group.bench_with_input(BenchmarkId::new("items", count), &scene, |b, scene| {
            b.iter(|| {
                // A live source's frame changes every frame, so its mips are rebuilt.
                for image in &images[..count] {
                    image.mark_changed();
                }
                black_box(compositor.render(scene, false).expect("render"));
                wait(&compositor);
            });
        });
    }
    group.finish();

    let layer = compositor
        .layer(WIDTH, HEIGHT)
        .expect("invariant: the canvas fits");
    let camera = camera();
    let placed = geometry(WIDTH as f32 * 0.5, HEIGHT as f32 * 0.5, 900.0);
    let mut group = c.benchmark_group("model_1080p");
    group.sample_size(20);
    for shape in [Shape::Sphere, Shape::Cube] {
        let model = shape
            .model(Material::default())
            .expect("invariant: a built-in shape builds");
        let uploaded = compositor
            .upload_model(&model)
            .expect("invariant: a shape uploads");
        let draw = ModelDraw {
            model: &uploaded,
            geometry: placed,
            camera: Some(camera),
            lighting: Lighting::Studio,
            lights: None,
            shadows: &[],
            environment: None,
            floor: None,
        };
        group.bench_function(BenchmarkId::new("shape", shape.name()), |b| {
            b.iter(|| {
                compositor.draw_model(&draw, &layer).expect("draw");
                let items = [Item {
                    image: &layer,
                    placement: Placement::Layer(placed),
                    crop: Crop::NONE,
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    visible: true,
                    camera: Some(camera),
                    depth_run: 0,
                }];
                black_box(compositor.render(&scene(&items), false).expect("render"));
                wait(&compositor);
            });
        });
    }
    // The sphere lit by one spot light that casts shadows (ADR-0151 decision 3): its
    // 2048 x 2048 map drawn from the light each frame, then the lit draw reading it.
    let sphere = compositor
        .upload_model(
            &Shape::Sphere
                .model(Material::default())
                .expect("invariant: a built-in shape builds"),
        )
        .expect("invariant: a shape uploads");
    let side = jstream_render::SHADOW_MAP_PX;
    let map = compositor
        .layer(side, side)
        .expect("invariant: a shadow map fits");
    let from = [
        WIDTH as f32 * 0.5 - 600.0,
        HEIGHT as f32 * 0.5 - 600.0,
        900.0 + 800.0,
    ];
    let light_camera = SceneCamera {
        position: Vec3::new(from[0], from[1], from[2]).expect("invariant: finite"),
        target: Vec3::new(WIDTH as f32 * 0.5, HEIGHT as f32 * 0.5, 900.0)
            .expect("invariant: finite"),
        fov_deg: Finite32::new(50.0).expect("invariant: finite"),
        roll_deg: Finite32::ZERO,
        depth_sort: false,
        focus_px: None,
        blur_px: Finite32::ZERO,
    };
    let aim = [600.0f64, 600.0, -800.0];
    let lights = [jstream_render::SceneLight {
        kind: jstream_render::LightKind::Spot,
        position: from.map(f64::from),
        aim,
        color: Color::rgb(1.0, 1.0, 1.0),
        intensity: 2.0,
        range_px: 0.0,
        cone_deg: 45.0,
        softness: 0.2,
    }];
    let clear = Scene {
        canvas: FrameSize::new(side, side).expect("invariant: non-zero"),
        background: Color::TRANSPARENT,
        items: &[],
    };
    let shadows = [jstream_render::ShadowMap {
        light: 0,
        image: &map,
        camera: light_camera,
        softness: 0.3,
    }];
    group.bench_function(BenchmarkId::new("shape", "sphere_shadowed"), |b| {
        b.iter(|| {
            compositor.render_view_depth(&clear, &map).expect("clear");
            compositor
                .draw_model_depth(
                    &ModelDraw {
                        model: &sphere,
                        geometry: placed,
                        camera: Some(light_camera),
                        lighting: Lighting::Scene,
                        lights: None,
                        shadows: &[],
                        environment: None,
                        floor: None,
                    },
                    &map,
                )
                .expect("cast");
            compositor
                .draw_model(
                    &ModelDraw {
                        model: &sphere,
                        geometry: placed,
                        camera: Some(camera),
                        lighting: Lighting::Scene,
                        lights: Some(&lights),
                        shadows: &shadows,
                        environment: None,
                        floor: None,
                    },
                    &layer,
                )
                .expect("draw");
            wait(&compositor);
        });
    });
    group.finish();

    // A scene's view depth and depth fog over its composite (ADR-0143): the cost a
    // depth-aware scene filter adds on top of drawing the scene, which is drawn once
    // outside the loop.
    for (id, group_name) in [
        ("depth_fog", "depth_fog_1080p_camera"),
        ("ambient_occlusion", "ambient_occlusion_1080p_camera"),
        ("volumetric_fog", "volumetric_fog_1080p_camera"),
        ("depth_bloom", "depth_bloom_1080p_camera"),
    ] {
        depth_filter(&mut compositor, (&images, camera), id, group_name, c);
    }
}

/// The view depth of N cards and the depth-aware kind `id` at its defaults over their
/// composite, as benchmark group `group_name`.
fn depth_filter(
    compositor: &mut Compositor,
    (images, camera): (&[Image], SceneCamera),
    id: &str,
    group_name: &str,
    c: &mut Criterion,
) {
    let kind = jstream_render::depth_filter_descriptors()
        .into_iter()
        .find(|kind| kind.kind_id == id)
        .expect("invariant: a depth-aware kind");
    let defaults = jstream_protocol::Settings(
        kind.params
            .iter()
            .map(|param| (param.id.clone(), param.default.clone()))
            .collect(),
    );
    let mut fog = VideoFilter::new(id, &defaults).expect("invariant: it builds");
    let depth = std::sync::Arc::new(
        compositor
            .layer(WIDTH, HEIGHT)
            .expect("invariant: the canvas fits"),
    );
    let composite = compositor
        .layer(WIDTH, HEIGHT)
        .expect("invariant: the canvas fits");
    let fogged = compositor
        .layer(WIDTH, HEIGHT)
        .expect("invariant: the canvas fits");
    let view = camera
        .view(jstream_protocol::Canvas {
            width: WIDTH,
            height: HEIGHT,
        })
        .expect("invariant: the camera is valid");
    fog.set_depth(Some(ViewDepth {
        view: Some(view),
        ..ViewDepth::new(std::sync::Arc::clone(&depth), view.focal_px())
    }));
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);
    for count in [4, 16] {
        let items = cards(&images[..count]);
        let scene = scene(&items);
        compositor.render_into(&scene, &composite).expect("render");
        group.bench_with_input(BenchmarkId::new("items", count), &scene, |b, scene| {
            b.iter(|| {
                compositor
                    .render_view_depth(scene, &depth)
                    .expect("view depth");
                compositor
                    .apply_filter_into(&mut fog, &composite, &fogged, FilterClock::default())
                    .expect("the filter");
                wait(compositor);
            });
        });
    }
    group.finish();
}

fn rewrite(c: &mut Criterion) {
    let Some(compositor) = open() else {
        return;
    };
    let frames = [pixels(WIDTH, HEIGHT, 0), pixels(WIDTH, HEIGHT, 101)];
    let image = compositor
        .upload_rgba8(WIDTH, HEIGHT, &frames[0])
        .expect("invariant: the image bytes match");
    let mut group = c.benchmark_group("rewrite_rgba8_1080p");
    group.sample_size(20);
    let mut next = 0;
    group.bench_function("rewrite", |b| {
        b.iter(|| {
            next ^= 1;
            compositor
                .rewrite_rgba8(&image, &frames[next])
                .expect("invariant: the image bytes match");
            // The write is staged until a submit; submit it so staging does not pile up.
            compositor.queue().submit([]);
            wait(&compositor);
        });
    });
    // What the calling thread (the render thread) spends, without waiting for the GPU
    // to run the conversion: the cost a tick pays for a new CPU frame (ADR-0117 §2).
    group.bench_function("rewrite_call", |b| {
        b.iter_custom(|iters| {
            let mut spent = std::time::Duration::ZERO;
            for _ in 0..iters {
                next ^= 1;
                let started = std::time::Instant::now();
                compositor
                    .rewrite_rgba8(&image, &frames[next])
                    .expect("invariant: the image bytes match");
                spent += started.elapsed();
                compositor.queue().submit([]);
                wait(&compositor);
            }
            spent
        });
    });
    group.finish();
}

// `criterion_group!` would generate an undocumented `pub fn` (`missing_docs`).
fn benches() {
    let mut c = Criterion::default().configure_from_args();
    composite(&mut c);
    three_d(&mut c);
    rewrite(&mut c);
}

criterion_main!(benches);
