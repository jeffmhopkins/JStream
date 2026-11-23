//! R-RT-005: what each built-in video filter kind costs per frame at 1080p on the adapter
//! it finds. Run on the machine the numbers are for, and state that machine with them:
//!
//! ```text
//! cargo bench -p jstream-render --bench filters
//! cargo bench -p jstream-render --bench filters -- glow      # the kinds that match
//! ```
//!
//! As in `composite.rs`, a software adapter (lavapipe, WARP) gives CPU rasterisation
//! times: they compare one kind with another and one revision with the next, never a GPU
//! budget. With no adapter the benchmarks are skipped with a line on stderr, except under
//! `CI`, where that panics.
//!
//! Each row is one `apply_filter_into` and a wait for the queue to drain, so it holds
//! every pass of the kind.
//!
//! - `filter_1080p/<kind>/opaque`: the kind at its defaults over a 1920x1080 picture with
//!   no transparent pixel, as a camera or a capture is.
//! - `filter_1080p/<kind>/shape`: the same over a mostly transparent frame holding a few
//!   opaque shapes, as text or a logo is: what a shadow, an outline or a glow is drawn
//!   around.
//! - `filter_1080p/<kind>:<setting>/...`: the kind at a setting that costs more than its
//!   default (the widest blur, the highest quality).
//! - `scene_filter_1080p/<kind>`: a depth-aware kind (ADR-0143, ADR-0151) at its defaults
//!   over a scene of 16 cards seen through a camera, with that scene's view depth already
//!   drawn, so the row is the filter alone; `<kind>:<setting>` as above, and
//!   `volumetric_fog:8_lights` with eight scene lights for the fog to scatter.

use std::io::Write;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_main};
use jstream_media::FrameSize;
use jstream_protocol::{
    Canvas, Finite32, ItemGeometry, KindDescriptor, SceneCamera, Settings, Size, Vec2, Vec3,
};
use jstream_render::{
    BlendMode, Color, Compositor, Crop, FilterClock, Image, Item, LightKind, OpenOptions,
    Placement, Scene, SceneLight, VideoFilter, ViewDepth,
};
use serde_json::{Value, json};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

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

/// A 1080p picture with no transparent pixel: gradients, with a bright band so a bloom
/// has something over its threshold.
fn opaque() -> Vec<u8> {
    (0..HEIGHT)
        .flat_map(|y| {
            (0..WIDTH).flat_map(move |x| {
                let bright = (400..480).contains(&y) || (x / 240) % 4 == 1;
                if bright {
                    [250, 245, 235, 255]
                } else {
                    [(x % 256) as u8, (y % 256) as u8, ((x + y) % 200) as u8, 255]
                }
            })
        })
        .collect()
}

/// A 1080p frame, transparent but for a row of opaque discs and a bar: about a tenth of
/// it covered.
fn shape() -> Vec<u8> {
    (0..HEIGHT)
        .flat_map(|y| {
            (0..WIDTH).flat_map(move |x| {
                let (cx, cy) = ((x % 320) as i32 - 160, y as i32 - 420);
                let disc = cx * cx + cy * cy < 90 * 90;
                let bar = (700..760).contains(&y) && (200..1720).contains(&x);
                if disc || bar {
                    [240, 200, 40, 255]
                } else {
                    [0, 0, 0, 0]
                }
            })
        })
        .collect()
}

fn wait(compositor: &Compositor) {
    compositor
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the queue drains");
}

/// `kind`'s defaults with `over` laid over them.
fn settings(kind: &KindDescriptor, over: &Value) -> Settings {
    let mut map: std::collections::BTreeMap<String, Value> = kind
        .params
        .iter()
        .map(|param| (param.id.clone(), param.default.clone()))
        .collect();
    if let Some(over) = over.as_object() {
        for (key, value) in over {
            map.insert(key.clone(), value.clone());
        }
    }
    Settings(map)
}

/// The settings that cost more than a kind's defaults, by a name for the row.
fn heavier(kind: &str) -> Vec<(&'static str, Value)> {
    match kind {
        "blur" => vec![("radius_100", json!({ "radius": 100.0 }))],
        "bloom" => vec![("radius_100", json!({ "radius": 100.0 }))],
        "drop_shadow" => vec![("blur_64", json!({ "blur": 64.0 }))],
        "glow" => vec![("radius_100", json!({ "radius": 100.0 }))],
        "outline" => vec![
            ("width_12", json!({ "width": 12.0 })),
            ("width_32", json!({ "width": 32.0 })),
        ],
        "sharpen" => vec![("radius_3", json!({ "radius": 3 }))],
        "aspect_ratio" => vec![(
            "stretch_lanczos",
            json!({ "mode": "stretch", "ratio": "4:3", "resampling": "lanczos" }),
        )],
        "ambient_occlusion" => vec![("high", json!({ "quality": "high" }))],
        "volumetric_fog" => vec![
            ("low", json!({ "quality": "low" })),
            (LIT, json!({})),
            ("high", json!({ "quality": "high" })),
        ],
        "depth_bloom" => vec![("radius_100", json!({ "radius_px": 100.0 }))],
        _ => Vec::new(),
    }
}

/// The row that gives a scene filter the scene's lights.
const LIT: &str = "8_lights";

/// Eight scene lights about the cards, as a lit set has: six small point lights with a
/// range, a spot light and a directional one.
fn lights(camera: &jstream_protocol::CameraView) -> Vec<SceneLight> {
    let eye = camera.position();
    let focal = f64::from(camera.focal_px());
    let light = |kind, at: [f64; 3], range_px| SceneLight {
        kind,
        position: [eye[0] + at[0], eye[1] + at[1], eye[2] - focal + at[2]],
        aim: [0.2, 1.0, -0.3],
        color: Color::rgb(1.0, 0.9, 0.8),
        intensity: 3.0,
        range_px,
        cone_deg: 50.0,
        softness: 0.3,
    };
    let mut lights: Vec<SceneLight> = (0..6)
        .map(|i| {
            let step = f64::from(i);
            light(
                LightKind::Point,
                [-700.0 + step * 280.0, 200.0, -100.0 - step * 60.0],
                300.0,
            )
        })
        .collect();
    lights.push(light(LightKind::Spot, [0.0, -600.0, -200.0], 0.0));
    lights.push(light(LightKind::Directional, [0.0; 3], 0.0));
    lights
}

/// Few and short samples: there are many rows, and a software adapter takes up to a
/// second for one frame of the slowest.
fn sized(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_millis(500));
    group.measurement_time(std::time::Duration::from_secs(2));
}

/// One row: `filter` over `input` into a layer of its size.
fn row(
    compositor: &mut Compositor,
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    id: BenchmarkId,
    filter: &mut VideoFilter,
    input: &Image,
) {
    let (width, height) = filter.output_size(input.width(), input.height());
    let output = compositor
        .layer(width, height)
        .expect("invariant: the output fits");
    group.bench_function(id, |b| {
        b.iter(|| {
            compositor
                .apply_filter_into(filter, input, &output, FilterClock::default())
                .expect("the filter");
            wait(compositor);
        });
    });
}

fn source_filters(c: &mut Criterion) {
    let Some(mut compositor) = open() else {
        return;
    };
    let inputs = [("opaque", opaque()), ("shape", shape())].map(|(name, pixels)| {
        let image = compositor
            .upload_rgba8(WIDTH, HEIGHT, &pixels)
            .expect("invariant: the image bytes match");
        (name, image)
    });
    // What the kinds that read a second image read: the shapes, as a mask.
    let reference = Arc::new(inputs[1].1.clone());
    let mut group = c.benchmark_group("filter_1080p");
    sized(&mut group);
    for kind in jstream_render::video_filter_descriptors() {
        let mut rows = vec![(kind.kind_id.clone(), json!({}))];
        for (name, over) in heavier(&kind.kind_id) {
            rows.push((format!("{}:{name}", kind.kind_id), over));
        }
        for (name, over) in rows {
            let Some(mut filter) = VideoFilter::new(&kind.kind_id, &settings(&kind, &over)) else {
                continue;
            };
            filter.set_reference(Some(Arc::clone(&reference)));
            for (input_name, input) in &inputs {
                row(
                    &mut compositor,
                    &mut group,
                    BenchmarkId::new(&name, input_name),
                    &mut filter,
                    input,
                );
            }
        }
    }
    group.finish();
}

/// The camera the scene is seen through: up and to the left of the framing camera.
fn camera() -> SceneCamera {
    let framing = SceneCamera::framing(Canvas {
        width: WIDTH,
        height: HEIGHT,
    });
    SceneCamera {
        position: Vec3::new(600.0, 200.0, framing.position.z.get() * 0.9)
            .expect("invariant: finite"),
        ..framing
    }
}

/// `images` as cards in a 3D scene, each turned a little and at its own depth.
fn cards(images: &[Image]) -> Vec<Item<'_>> {
    let camera = camera();
    images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            let step = i as f32;
            let center = Vec2::new(
                400.0 + (step * 83.0) % 1100.0,
                300.0 + (step * 47.0) % 500.0,
            )
            .expect("invariant: finite");
            let size = Size::try_new(640.0, 360.0).expect("invariant: positive");
            let mut placed = ItemGeometry::try_new(center, size).expect("invariant: a box");
            placed.turn_deg = Finite32::new(10.0 + step * 3.0).expect("invariant: finite");
            placed.depth_px = Finite32::new(-(step * 60.0)).expect("invariant: finite");
            Item {
                image,
                placement: Placement::Geometry(placed),
                crop: Crop::NONE,
                opacity: 1.0,
                blend: BlendMode::Normal,
                visible: true,
                camera: Some(camera),
                depth_run: 0,
                depth: None,
            }
        })
        .collect()
}

fn scene_filters(c: &mut Criterion) {
    let Some(mut compositor) = open() else {
        return;
    };
    let picture = opaque();
    let images: Vec<Image> = (0..16)
        .map(|_| {
            compositor
                .upload_rgba8(WIDTH, HEIGHT, &picture)
                .expect("invariant: the image bytes match")
        })
        .collect();
    let items = cards(&images);
    let scene = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("invariant: the canvas is non-zero"),
        background: Color::rgb(0.1, 0.1, 0.1),
        items: &items,
    };
    let layer = |compositor: &Compositor| {
        compositor
            .layer(WIDTH, HEIGHT)
            .expect("invariant: the canvas fits")
    };
    let composite = layer(&compositor);
    let depth = Arc::new(layer(&compositor));
    compositor.render_into(&scene, &composite).expect("render");
    compositor
        .render_view_depth(&scene, &depth)
        .expect("view depth");
    let view = camera()
        .view(Canvas {
            width: WIDTH,
            height: HEIGHT,
        })
        .expect("invariant: the camera is valid");
    let mut group = c.benchmark_group("scene_filter_1080p");
    sized(&mut group);
    for kind in jstream_render::depth_filter_descriptors() {
        let mut rows = vec![(kind.kind_id.clone(), json!({}))];
        for (name, over) in heavier(&kind.kind_id) {
            rows.push((format!("{}:{name}", kind.kind_id), over));
        }
        for (name, over) in rows {
            let Some(mut filter) = VideoFilter::new(&kind.kind_id, &settings(&kind, &over)) else {
                continue;
            };
            let lights = if name.ends_with(LIT) {
                lights(&view)
            } else {
                Vec::new()
            };
            filter.set_depth(Some(ViewDepth {
                view: Some(view),
                lights: lights.into(),
                ..ViewDepth::new(Arc::clone(&depth), view.focal_px())
            }));
            row(
                &mut compositor,
                &mut group,
                BenchmarkId::from_parameter(&name),
                &mut filter,
                &composite,
            );
        }
    }
    group.finish();
}

// `criterion_group!` would generate an undocumented `pub fn` (`missing_docs`).
fn benches() {
    let mut c = Criterion::default().configure_from_args();
    source_filters(&mut c);
    scene_filters(&mut c);
}

criterion_main!(benches);
