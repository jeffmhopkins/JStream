//! Pixel checks on whatever adapter [`Compositor::open`] finds. With no adapter the test
//! writes a line and returns; in CI that is a failure, because the workflow installs one.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jstream_media::{ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrameInfo};
use jstream_platform::{FrameSync, GpuFrame, PlatformError};

use crate::{Color, Compositor, Crop, Item, OpenOptions, Placement, RenderError, Scene, Snapshot};

/// WARP (Microsoft Basic Render Driver) access-violates, `0xc0000005`, when several
/// tests create and drop Dx12 devices at once. The guard is the first element so the
/// device drops while the lock is still held.
fn open() -> Option<(std::sync::MutexGuard<'static, ()>, Compositor)> {
    let guard = gpu_lock();
    match Compositor::open(OpenOptions::headless()) {
        Ok(compositor) => {
            let _ = writeln!(
                std::io::stderr(),
                "jstream-render test adapter: {}",
                compositor.adapter_description()
            );
            Some((guard, compositor))
        }
        Err(err) if err.is_no_adapter() => {
            if std::env::var_os("CI").is_some() {
                panic!("CI has no wgpu adapter: {err}");
            }
            let _ = writeln!(
                std::io::stderr(),
                "jstream-render: skipping GPU test, no adapter: {err}"
            );
            None
        }
        Err(err) => panic!("opening the compositor failed: {err}"),
    }
}

fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn solid(compositor: &Compositor, width: u32, height: u32, rgba: [u8; 4]) -> crate::Image {
    let pixel: Vec<u8> = std::iter::repeat_n(rgba, (width * height) as usize)
        .flatten()
        .collect();
    compositor
        .upload_rgba8(width, height, &pixel)
        .expect("invariant: the image bytes match")
}

fn draw(
    compositor: &mut Compositor,
    width: u32,
    height: u32,
    background: Color,
    items: &[Item<'_>],
) -> Snapshot {
    let scene = Scene {
        canvas: FrameSize::new(width, height).expect("invariant: the test canvas is non-zero"),
        background,
        items,
    };
    let output = compositor.render(&scene, true).expect("render");
    output
        .pixels()
        .expect("invariant: readback was requested")
        .clone()
}

fn near(snapshot: &Snapshot, x: u32, y: u32, expected: [u8; 4], tol: u8) {
    let pixel = snapshot
        .pixel(x, y)
        .expect("invariant: the sample is inside the canvas");
    for channel in 0..4 {
        let delta = pixel[channel].abs_diff(expected[channel]);
        assert!(
            delta <= tol,
            "pixel ({x}, {y}) is {pixel:?}, expected {expected:?} within {tol}"
        );
    }
}

fn item<'a>(image: &'a crate::Image, placement: Placement, crop: Crop, opacity: f32) -> Item<'a> {
    Item {
        image,
        placement,
        crop,
        opacity,
        visible: true,
    }
}

#[test]
fn background_only_fills_the_canvas() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let snapshot = draw(&mut compositor, 4, 3, Color::rgb(1.0, 0.0, 0.0), &[]);
    assert_eq!(
        (snapshot.width(), snapshot.height(), snapshot.rgba().len()),
        (4, 3, 48)
    );
    near(&snapshot, 0, 0, [255, 0, 0, 255], 0);
    near(&snapshot, 3, 2, [255, 0, 0, 255], 0);
}

#[test]
fn opaque_rect_stacks_over_the_background() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let red = solid(&compositor, 8, 6, [255, 0, 0, 255]);
    let blue = solid(&compositor, 4, 2, [0, 0, 255, 255]);
    let items = [
        item(&red, Placement::IDENTITY, Crop::NONE, 1.0),
        item(
            &blue,
            Placement {
                position: [2.0, 2.0],
                ..Placement::IDENTITY
            },
            Crop::NONE,
            1.0,
        ),
    ];
    let snapshot = draw(&mut compositor, 8, 6, Color::BLACK, &items);
    near(&snapshot, 0, 0, [255, 0, 0, 255], 0);
    near(&snapshot, 3, 3, [0, 0, 255, 255], 0);
    near(&snapshot, 7, 5, [255, 0, 0, 255], 0);
}

#[test]
fn crop_keeps_the_uncut_texels() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let mut pixels = Vec::new();
    for _row in 0..2 {
        for x in 0..4 {
            let rgba = if x < 2 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            };
            pixels.extend_from_slice(&rgba);
        }
    }
    // 4x2, both rows red-red-green-green. Cropping the left half leaves green.
    let image = compositor.upload_rgba8(4, 2, &pixels).unwrap();
    let items = [item(
        &image,
        Placement::IDENTITY,
        Crop {
            left: 2,
            top: 0,
            right: 0,
            bottom: 0,
        },
        1.0,
    )];
    let snapshot = draw(&mut compositor, 4, 2, Color::BLACK, &items);
    near(&snapshot, 0, 0, [0, 255, 0, 255], 0);
    near(&snapshot, 1, 1, [0, 255, 0, 255], 0);
    near(&snapshot, 2, 0, [0, 0, 0, 255], 0);
}

#[test]
fn integer_scale_covers_the_destination() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let red = solid(&compositor, 2, 2, [255, 0, 0, 255]);
    let items = [item(
        &red,
        Placement {
            scale: [2.0, 2.0],
            ..Placement::IDENTITY
        },
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut compositor, 4, 4, Color::BLACK, &items);
    near(&snapshot, 0, 0, [255, 0, 0, 255], 1);
    near(&snapshot, 3, 3, [255, 0, 0, 255], 1);
}

#[test]
fn rotation_about_the_top_left_is_clockwise() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let red = solid(&compositor, 20, 10, [255, 0, 0, 255]);
    let items = [item(
        &red,
        Placement {
            position: [30.0, 30.0],
            rotation_deg: 90.0,
            ..Placement::IDENTITY
        },
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut compositor, 64, 64, Color::BLACK, &items);
    // Interior of the rotated rectangle (x 20..30, y 30..50), and a pixel clearly outside it.
    near(&snapshot, 25, 40, [255, 0, 0, 255], 1);
    near(&snapshot, 40, 40, [0, 0, 0, 255], 0);
}

#[test]
fn half_opacity_blends_in_linear_light() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let black = solid(&compositor, 2, 2, [0, 0, 0, 255]);
    let items = [item(&black, Placement::IDENTITY, Crop::NONE, 0.5)];
    let snapshot = draw(&mut compositor, 2, 2, Color::WHITE, &items);
    let encoded = (linear_to_srgb(0.5) * 255.0).round() as u8;
    near(&snapshot, 1, 1, [encoded, encoded, encoded, 255], 2);
}

#[test]
fn hidden_and_fully_cropped_items_draw_nothing() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let green = solid(&compositor, 4, 4, [0, 255, 0, 255]);
    let hidden = Item {
        visible: false,
        ..item(&green, Placement::IDENTITY, Crop::NONE, 1.0)
    };
    let cropped = item(
        &green,
        Placement::IDENTITY,
        Crop {
            left: 2,
            top: 0,
            right: 2,
            bottom: 0,
        },
        1.0,
    );
    let snapshot = draw(
        &mut compositor,
        4,
        4,
        Color::rgb(0.0, 0.0, 1.0),
        &[hidden, cropped],
    );
    near(&snapshot, 1, 1, [0, 0, 255, 255], 0);
}

#[test]
fn bad_opacity_and_short_images_are_refused() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let image = solid(&compositor, 1, 1, [255, 0, 0, 255]);
    let scene = Scene {
        canvas: FrameSize::new(1, 1).unwrap(),
        background: Color::BLACK,
        items: &[item(&image, Placement::IDENTITY, Crop::NONE, 2.0)],
    };
    assert!(matches!(
        compositor.render(&scene, false),
        Err(RenderError::Invalid {
            what: "opacity",
            ..
        })
    ));
    assert!(matches!(
        compositor.upload_rgba8(2, 2, &[0, 0, 0, 255]),
        Err(RenderError::ImageBytes { .. })
    ));
    assert!(matches!(
        compositor.upload_rgba8(0, 1, &[]),
        Err(RenderError::Invalid { what: "image", .. })
    ));
}

#[test]
fn a_gpu_frame_is_staged_once_and_a_hidden_one_is_not() {
    let Some((_gpu, mut compositor)) = open() else {
        return;
    };
    let uploaded = solid(&compositor, 2, 2, [255, 0, 0, 255]);
    let count = Arc::new(AtomicUsize::new(0));
    let info = VideoFrameInfo {
        size: FrameSize::new(2, 2).unwrap(),
        format: PixelFormat::Rgba8,
        color: ColorInfo {
            range: jstream_media::ColorRange::Full,
            ..ColorInfo::default()
        },
        pts: MediaTime::zero(Timebase::NANOS),
    };
    let frame = GpuFrame::new(
        uploaded.texture().clone(),
        info,
        Arc::new(CountSync(Arc::clone(&count))),
    );
    let image = compositor.import_frame(&frame).unwrap();
    let items = [
        item(&image, Placement::IDENTITY, Crop::NONE, 1.0),
        item(
            &image,
            Placement {
                position: [1.0, 0.0],
                ..Placement::IDENTITY
            },
            Crop::NONE,
            1.0,
        ),
    ];
    let snapshot = draw(&mut compositor, 4, 2, Color::BLACK, &items);
    near(&snapshot, 0, 0, [255, 0, 0, 255], 1);
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "one submit stages the hold once"
    );

    count.store(0, Ordering::SeqCst);
    let hidden = Item {
        visible: false,
        ..item(&image, Placement::IDENTITY, Crop::NONE, 1.0)
    };
    let _ = draw(&mut compositor, 4, 2, Color::BLACK, &[hidden]);
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "a hidden item is not staged"
    );
}

struct CountSync(Arc<AtomicUsize>);

impl std::fmt::Debug for CountSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CountSync")
            .field("count", &self.0.load(Ordering::Relaxed))
            .finish()
    }
}

impl FrameSync for CountSync {
    fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// IEC 61966-2-1, the encode the sRGB target applies. Independent of [`crate::color`].
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
