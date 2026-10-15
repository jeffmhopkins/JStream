//! R-RT-005: what the compositor costs per frame on the adapter it finds. Run on the
//! machine the numbers are for, and state that machine with them:
//!
//! ```text
//! cargo bench -p jstream-render --bench composite
//! ```
//!
//! `OpenOptions::headless()` prefers a software adapter (lavapipe, WARP), as CI has. On
//! those the numbers are CPU rasterisation, not GPU cost: they track regressions in what
//! the compositor submits, never a GPU budget. With no adapter at all the benchmarks are
//! skipped with a line on stderr, except under `CI`, where that panics.
//!
//! - `composite_1080p_readback`: a 1920x1080 frame of N 1280x720 items, with the readback
//!   that `video.snapshot` asks for, so the time includes the GPU finishing the frame.
//! - `composite_1080p_submit`: the same frame without readback, then a wait for the queue
//!   to drain: the encoder path.
//! - `rewrite_rgba8_1080p`: refilling a 1080p upload in place (CPU premultiply in linear
//!   light, then the queue write), as an image or browser source does on a new frame.

use std::hint::black_box;
use std::io::Write;

use criterion::{BenchmarkId, Criterion, criterion_main};
use jstream_media::FrameSize;
use jstream_render::{
    Color, Compositor, Crop, Image, Item, LegacyTransform, OpenOptions, Placement, Scene,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const ITEM_WIDTH: u32 = 1280;
const ITEM_HEIGHT: u32 = 720;
const ITEM_COUNTS: [usize; 3] = [1, 4, 16];

fn open() -> Option<Compositor> {
    match Compositor::open(OpenOptions::headless()) {
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
                visible: true,
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
    group.finish();
}

// `criterion_group!` would generate an undocumented `pub fn` (`missing_docs`).
fn benches() {
    let mut c = Criterion::default().configure_from_args();
    composite(&mut c);
    rewrite(&mut c);
}

criterion_main!(benches);
