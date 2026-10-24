//! Pixel checks on whatever adapter [`Compositor::open`] finds. With no adapter the test
//! writes a line and returns; in CI that is a failure, because the workflow installs one.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jstream_media::{ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrameInfo};
use jstream_platform::{FrameSync, GpuFrame, PlatformError};

use crate::{
    Color, Compositor, Crop, Image, Item, LegacyTransform, OpenOptions, Placement, RenderError,
    ResourceCache, Scene, Snapshot,
};

/// A compositor whose device drops before the WARP lock.
///
/// Fields drop in declaration order, so the device is destroyed while the lock is held
/// for as long as this value is kept whole. Splitting the fields apart drops them as
/// separate locals and can release the lock first.
pub(crate) struct Opened {
    pub(crate) compositor: Compositor,
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// WARP (Microsoft Basic Render Driver) access-violates, `0xc0000005`, when several
/// tests create and drop Dx12 devices at once.
pub(crate) fn open() -> Option<Opened> {
    let _lock = gpu_lock();
    match Compositor::open(OpenOptions::hardware()) {
        Ok(compositor) => {
            let _ = writeln!(
                std::io::stderr(),
                "jstream-render test adapter: {}",
                compositor.adapter_description()
            );
            Some(Opened { compositor, _lock })
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

pub(crate) fn solid(
    compositor: &Compositor,
    width: u32,
    height: u32,
    rgba: [u8; 4],
) -> crate::Image {
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

pub(crate) fn near(snapshot: &Snapshot, x: u32, y: u32, expected: [u8; 4], tol: u8) {
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
    let Some(mut opened) = open() else {
        return;
    };
    let snapshot = draw(&mut opened.compositor, 4, 3, Color::rgb(1.0, 0.0, 0.0), &[]);
    assert_eq!(
        (snapshot.width(), snapshot.height(), snapshot.rgba().len()),
        (4, 3, 48)
    );
    near(&snapshot, 0, 0, [255, 0, 0, 255], 0);
    near(&snapshot, 3, 2, [255, 0, 0, 255], 0);
}

#[test]
fn opaque_rect_stacks_over_the_background() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 8, 6, [255, 0, 0, 255]);
    let blue = solid(&opened.compositor, 4, 2, [0, 0, 255, 255]);
    let items = [
        item(&red, Placement::IDENTITY, Crop::NONE, 1.0),
        item(
            &blue,
            Placement::Legacy(LegacyTransform {
                position: [2.0, 2.0],
                ..LegacyTransform::IDENTITY
            }),
            Crop::NONE,
            1.0,
        ),
    ];
    let snapshot = draw(&mut opened.compositor, 8, 6, Color::BLACK, &items);
    near(&snapshot, 0, 0, [255, 0, 0, 255], 0);
    near(&snapshot, 3, 3, [0, 0, 255, 255], 0);
    near(&snapshot, 7, 5, [255, 0, 0, 255], 0);
}

#[test]
fn crop_keeps_the_uncut_texels() {
    let Some(mut opened) = open() else {
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
    let image = opened.compositor.upload_rgba8(4, 2, &pixels).unwrap();
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
    let snapshot = draw(&mut opened.compositor, 4, 2, Color::BLACK, &items);
    near(&snapshot, 0, 0, [0, 255, 0, 255], 0);
    near(&snapshot, 1, 1, [0, 255, 0, 255], 0);
    near(&snapshot, 2, 0, [0, 0, 0, 255], 0);
}

#[test]
fn integer_scale_covers_the_destination() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 2, 2, [255, 0, 0, 255]);
    let items = [item(
        &red,
        Placement::Legacy(LegacyTransform {
            scale: [2.0, 2.0],
            ..LegacyTransform::IDENTITY
        }),
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 4, 4, Color::BLACK, &items);
    near(&snapshot, 0, 0, [255, 0, 0, 255], 1);
    near(&snapshot, 3, 3, [255, 0, 0, 255], 1);
}

#[test]
fn rotation_about_the_top_left_is_clockwise() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 20, 10, [255, 0, 0, 255]);
    let items = [item(
        &red,
        Placement::Legacy(LegacyTransform {
            position: [30.0, 30.0],
            rotation_deg: 90.0,
            ..LegacyTransform::IDENTITY
        }),
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 64, 64, Color::BLACK, &items);
    // Interior of the rotated rectangle (x 20..30, y 30..50), and a pixel clearly outside it.
    near(&snapshot, 25, 40, [255, 0, 0, 255], 1);
    near(&snapshot, 40, 40, [0, 0, 0, 255], 0);
}

#[test]
fn half_opacity_blends_in_linear_light() {
    let Some(mut opened) = open() else {
        return;
    };
    let black = solid(&opened.compositor, 2, 2, [0, 0, 0, 255]);
    let items = [item(&black, Placement::IDENTITY, Crop::NONE, 0.5)];
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::WHITE, &items);
    let encoded = (linear_to_srgb(0.5) * 255.0).round() as u8;
    near(&snapshot, 1, 1, [encoded, encoded, encoded, 255], 2);
}

/// The review case: a 2x1 image, opaque red then fully transparent, drawn so a pixel
/// samples between the two texels. Filtering straight alpha would mix in the transparent
/// texel's black and resolve to red 188 at alpha 128 for the half-way sample; filtering
/// premultiplied texels keeps red at 255.
#[test]
fn transparent_edges_filter_without_a_dark_fringe() {
    let Some(mut opened) = open() else {
        return;
    };
    let image = opened
        .compositor
        .upload_rgba8(2, 1, &[255, 0, 0, 255, 0, 0, 0, 0])
        .expect("invariant: the image bytes match");

    // Half a pixel to the right: pixel 1's centre (x 1.5) samples u 0.5, half-way
    // between the texel centres.
    let offset = [item(
        &image,
        Placement::Legacy(LegacyTransform {
            position: [0.5, 0.0],
            ..LegacyTransform::IDENTITY
        }),
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 3, 1, Color::TRANSPARENT, &offset);
    near(&snapshot, 1, 0, [255, 0, 0, 128], 2);

    // Twice as wide: pixel 1 is 3/4 red texel, pixel 2 is 1/4. Straight filtering would
    // give red 225 and 137 there.
    let scaled = [item(
        &image,
        Placement::Legacy(LegacyTransform {
            scale: [2.0, 1.0],
            ..LegacyTransform::IDENTITY
        }),
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 4, 1, Color::TRANSPARENT, &scaled);
    near(&snapshot, 0, 0, [255, 0, 0, 255], 1);
    near(&snapshot, 1, 0, [255, 0, 0, 191], 2);
    near(&snapshot, 2, 0, [255, 0, 0, 64], 2);
    near(&snapshot, 3, 0, [0, 0, 0, 0], 1);
}

fn layer(compositor: &mut Compositor, width: u32, height: u32, items: &[Item<'_>]) -> Image {
    let scene = Scene {
        canvas: FrameSize::new(width, height).expect("invariant: the test canvas is non-zero"),
        background: Color::TRANSPARENT,
        items,
    };
    compositor.render_image(&scene).expect("render_image")
}

/// A nested scene: a transparent 4x4 scene with a red 2x2 square at (1, 1), rendered to
/// an image and placed at (2, 2) in a black 8x8 scene. The square lands at (3, 3) and
/// the inner scene's transparent pixels show the outer background.
#[test]
fn a_rendered_image_places_like_its_scene() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 2, 2, [255, 0, 0, 255]);
    let at_one = Placement::Legacy(LegacyTransform {
        position: [1.0, 1.0],
        ..LegacyTransform::IDENTITY
    });
    let inner = layer(
        &mut opened.compositor,
        4,
        4,
        &[item(&red, at_one, Crop::NONE, 1.0)],
    );
    assert_eq!((inner.width(), inner.height()), (4, 4));
    assert_eq!(inner.texture().format(), wgpu::TextureFormat::Rgba16Float);

    // Another render in between must not touch the image's texels.
    let white = solid(&opened.compositor, 1, 1, [255, 255, 255, 255]);
    let _ = draw(
        &mut opened.compositor,
        8,
        8,
        Color::WHITE,
        &[item(&white, Placement::IDENTITY, Crop::NONE, 1.0)],
    );

    let placed = [item(
        &inner,
        Placement::Legacy(LegacyTransform {
            position: [2.0, 2.0],
            ..LegacyTransform::IDENTITY
        }),
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 8, 8, Color::BLACK, &placed);
    for (x, y) in [(3, 3), (4, 3), (3, 4), (4, 4)] {
        near(&snapshot, x, y, [255, 0, 0, 255], 0);
    }
    for (x, y) in [(0, 0), (2, 2), (5, 5), (5, 3), (7, 7)] {
        near(&snapshot, x, y, [0, 0, 0, 255], 0);
    }
}

/// A rendered image is premultiplied linear light: half-opaque black inside the nested
/// scene, placed over white, blends exactly as the same item drawn directly would.
#[test]
fn a_rendered_image_keeps_partial_alpha_in_linear_light() {
    let Some(mut opened) = open() else {
        return;
    };
    let black = solid(&opened.compositor, 2, 2, [0, 0, 0, 255]);
    let inner = layer(
        &mut opened.compositor,
        2,
        2,
        &[item(&black, Placement::IDENTITY, Crop::NONE, 0.5)],
    );
    let placed = [item(&inner, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::WHITE, &placed);
    let encoded = (linear_to_srgb(0.5) * 255.0).round() as u8;
    near(&snapshot, 1, 1, [encoded, encoded, encoded, 255], 2);
}

/// Nesting twice: a rendered image placed in a scene that is itself rendered to an
/// image, scaled by 2 at each level.
#[test]
fn rendered_images_nest() {
    let Some(mut opened) = open() else {
        return;
    };
    let green = solid(&opened.compositor, 1, 1, [0, 255, 0, 255]);
    let double = Placement::Legacy(LegacyTransform {
        scale: [2.0, 2.0],
        ..LegacyTransform::IDENTITY
    });
    let deepest = layer(
        &mut opened.compositor,
        1,
        1,
        &[item(&green, Placement::IDENTITY, Crop::NONE, 1.0)],
    );
    let middle = layer(
        &mut opened.compositor,
        2,
        2,
        &[item(&deepest, double, Crop::NONE, 1.0)],
    );
    let placed = [item(&middle, double, Crop::NONE, 1.0)];
    let snapshot = draw(&mut opened.compositor, 6, 6, Color::BLACK, &placed);
    for (x, y) in [(0, 0), (1, 2), (3, 3)] {
        near(&snapshot, x, y, [0, 255, 0, 255], 0);
    }
    for (x, y) in [(4, 4), (5, 0), (0, 5)] {
        near(&snapshot, x, y, [0, 0, 0, 255], 0);
    }
}

/// A rendered image belongs to its compositor like an upload does.
#[test]
fn a_rendered_image_is_refused_by_another_compositor() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 1, 1, [255, 0, 0, 255]);
    let inner = layer(
        &mut opened.compositor,
        1,
        1,
        &[item(&red, Placement::IDENTITY, Crop::NONE, 1.0)],
    );
    let mut other = opened.compositor.sibling();
    let scene = Scene {
        canvas: FrameSize::new(1, 1).expect("invariant: non-zero"),
        background: Color::BLACK,
        items: &[item(&inner, Placement::IDENTITY, Crop::NONE, 1.0)],
    };
    assert!(matches!(
        other.render_image(&scene),
        Err(RenderError::Invalid { what: "image", .. })
    ));
}

/// An opaque import reads every texel's alpha as 1, so half-transparent red over black
/// is full red, as a capture should draw (#451), still faded by the item's opacity. A
/// filter over it sees an opaque input too.
#[test]
fn an_opaque_import_ignores_the_textures_alpha() {
    let Some(mut opened) = open() else {
        return;
    };
    let srgb = srgb_texture(&opened.compositor, 2, 2, [255, 0, 0, 128]);
    let image = opened.compositor.import_opaque(srgb, None).unwrap();
    let items = [item(&image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::BLACK, &items);
    near(&snapshot, 1, 1, [255, 0, 0, 255], 2);
    let items = [item(&image, Placement::IDENTITY, Crop::NONE, 0.5)];
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::BLACK, &items);
    let red = (linear_to_srgb(0.5) * 255.0).round() as u8;
    near(&snapshot, 1, 1, [red, 0, 0, 255], 2);

    let mut identity = crate::filter::testing::filter("color_correction", serde_json::json!({}));
    let out = crate::filter::testing::run(&mut opened, &mut identity, &image, 0.0);
    near(&out, 1, 1, [255, 0, 0, 255], 2);
}

/// An imported sRGB texture is straight alpha, so at a texel centre half-transparent red
/// over black is linear red 0.5, not the full red a premultiplied read would give.
#[test]
fn an_imported_texture_is_read_as_straight_alpha() {
    let Some(mut opened) = open() else {
        return;
    };
    let srgb = srgb_texture(&opened.compositor, 2, 2, [255, 0, 0, 128]);
    let image = opened.compositor.import(srgb, None).unwrap();
    let items = [item(&image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::BLACK, &items);
    let alpha = 128.0 / 255.0;
    let red = (linear_to_srgb(alpha) * 255.0).round() as u8;
    near(&snapshot, 1, 1, [red, 0, 0, 255], 2);
}

/// A browser page's texture holds sRGB code values already multiplied by alpha. Read
/// that way, half-transparent red over white keeps full red and lets half the white
/// through in linear light; reading it as straight alpha would darken the red twice.
/// The same texture is refused as a straight import, and an sRGB one as premultiplied.
#[test]
fn a_premultiplied_import_is_unpremultiplied_before_decoding() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    // Straight sRGB red at alpha 128, premultiplied in code values: 255 * 128 / 255.
    let texture = unorm_texture(compositor, 2, 2, [128, 0, 0, 128]);
    let image = compositor
        .import_premultiplied(texture.clone(), None)
        .expect("a Unorm texture imports premultiplied");
    let items = [item(&image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(compositor, 2, 2, Color::WHITE, &items);
    let alpha = 128.0 / 255.0;
    let white = (linear_to_srgb(1.0 - alpha) * 255.0).round() as u8;
    near(&snapshot, 1, 1, [255, white, white, 255], 2);

    assert!(matches!(
        compositor.import(texture, None),
        Err(RenderError::UnsupportedFormat { .. })
    ));
    let srgb = srgb_texture(compositor, 2, 2, [255, 0, 0, 128]);
    assert!(matches!(
        compositor.import_premultiplied(srgb, None),
        Err(RenderError::UnsupportedFormat { .. })
    ));
}

/// A frame the cache imported one way is replaced, not reused, when the same key is
/// imported the other way.
#[test]
fn the_cache_keeps_premultiplied_and_straight_imports_apart() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mut cache: ResourceCache<&'static str> = ResourceCache::new();
    let info = VideoFrameInfo {
        size: FrameSize::new(2, 2).unwrap(),
        format: PixelFormat::Bgra8,
        color: ColorInfo::default(),
        pts: MediaTime::zero(Timebase::NANOS),
    };
    let page = GpuFrame::new(
        unorm_texture(compositor, 2, 2, [0, 0, 255, 255]),
        info,
        Arc::new(CountSync(Arc::new(AtomicUsize::new(0)))),
    );
    cache.begin_frame();
    let image = cache
        .import_premultiplied(compositor, &"page", &page)
        .expect("import");
    let items = [item(image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(compositor, 2, 2, Color::BLACK, &items);
    near(&snapshot, 0, 0, [0, 0, 255, 255], 1);
    drop(cache.end_frame(compositor));

    // The same key, now a straight import: a new image, and the first one retires.
    let (straight, _) = counted_frame(compositor, 2, 2, [0, 255, 0, 255]);
    let (pixel, early) = show_as(compositor, &mut cache, "page", &straight);
    assert_eq!(pixel, [0, 255, 0, 255]);
    assert_eq!((cache.live(), cache.kept_imports(&"page")), (1, 0));
    compositor
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let late = cache.reclaim().map_or(0, |freed| freed.len());
    assert_eq!(early + late, 1, "the premultiplied import retired");
}

#[test]
fn an_image_from_another_compositor_is_refused() {
    let Some(opened) = open() else {
        return;
    };
    // Same device, as two `on_context` calls would make, so no second device is created
    // under the WARP lock. Declared after `opened`, so it drops first.
    let mut other = opened.compositor.sibling();
    let image = solid(&opened.compositor, 1, 1, [255, 0, 0, 255]);
    for visible in [true, false] {
        let items = [Item {
            visible,
            ..item(&image, Placement::IDENTITY, Crop::NONE, 1.0)
        }];
        let scene = Scene {
            canvas: FrameSize::new(1, 1).unwrap(),
            background: Color::BLACK,
            items: &items,
        };
        assert!(
            matches!(
                other.render(&scene, false),
                Err(RenderError::Invalid { what: "image", .. })
            ),
            "visible = {visible}"
        );
    }
}

#[test]
fn hidden_and_fully_cropped_items_draw_nothing() {
    let Some(mut opened) = open() else {
        return;
    };
    let green = solid(&opened.compositor, 4, 4, [0, 255, 0, 255]);
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
        &mut opened.compositor,
        4,
        4,
        Color::rgb(0.0, 0.0, 1.0),
        &[hidden, cropped],
    );
    near(&snapshot, 1, 1, [0, 0, 255, 255], 0);
}

#[test]
fn bad_opacity_and_short_images_are_refused() {
    let Some(mut opened) = open() else {
        return;
    };
    let image = solid(&opened.compositor, 1, 1, [255, 0, 0, 255]);
    let scene = Scene {
        canvas: FrameSize::new(1, 1).unwrap(),
        background: Color::BLACK,
        items: &[item(&image, Placement::IDENTITY, Crop::NONE, 2.0)],
    };
    assert!(matches!(
        opened.compositor.render(&scene, false),
        Err(RenderError::Invalid {
            what: "opacity",
            ..
        })
    ));
    assert!(matches!(
        opened.compositor.upload_rgba8(2, 2, &[0, 0, 0, 255]),
        Err(RenderError::ImageBytes { .. })
    ));
    assert!(matches!(
        opened.compositor.upload_rgba8(0, 1, &[]),
        Err(RenderError::Invalid { what: "image", .. })
    ));
}

#[test]
fn a_gpu_frame_is_staged_once_and_a_hidden_one_is_not() {
    let Some(mut opened) = open() else {
        return;
    };
    let srgb = srgb_texture(&opened.compositor, 2, 2, [255, 0, 0, 255]);
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
    let frame = GpuFrame::new(srgb, info, Arc::new(CountSync(Arc::clone(&count))));
    let image = opened.compositor.import_frame(&frame).unwrap();
    let items = [
        item(&image, Placement::IDENTITY, Crop::NONE, 1.0),
        item(
            &image,
            Placement::Legacy(LegacyTransform {
                position: [1.0, 0.0],
                ..LegacyTransform::IDENTITY
            }),
            Crop::NONE,
            1.0,
        ),
    ];
    let snapshot = draw(&mut opened.compositor, 4, 2, Color::BLACK, &items);
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
    let _ = draw(&mut opened.compositor, 4, 2, Color::BLACK, &[hidden]);
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "a hidden item is not staged"
    );
}

/// A readback too large for a buffer fails before any hold is staged, so the scene's
/// frame is neither armed on a later submit nor kept by the compositor (#391).
#[test]
fn a_refused_readback_stages_and_keeps_no_hold() {
    let Some(mut opened) = open() else {
        return;
    };
    let srgb = srgb_texture(&opened.compositor, 2, 2, [255, 0, 0, 255]);
    let count = Arc::new(AtomicUsize::new(0));
    let sync: Arc<dyn FrameSync> = Arc::new(CountSync(Arc::clone(&count)));
    let info = VideoFrameInfo {
        size: FrameSize::new(2, 2).unwrap(),
        format: PixelFormat::Rgba8,
        color: ColorInfo::default(),
        pts: MediaTime::zero(Timebase::NANOS),
    };
    let frame = GpuFrame::new(srgb, info, Arc::clone(&sync));
    let image = opened.compositor.import_frame(&frame).unwrap();
    let items = [item(&image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let scene = Scene {
        canvas: FrameSize::new(4, 2).unwrap(),
        background: Color::BLACK,
        items: &items,
    };
    let holders = Arc::strong_count(&sync);
    // Two rows padded to 256 bytes need 512; the vertex buffer fits in less.
    opened.compositor.cap_buffer_size(511);
    assert!(matches!(
        opened.compositor.render(&scene, true),
        Err(RenderError::Readback { .. })
    ));
    assert_eq!(count.load(Ordering::SeqCst), 0, "nothing was staged");
    assert_eq!(
        Arc::strong_count(&sync),
        holders,
        "the compositor keeps no hold after the error"
    );

    // Without readback the same scene renders, staging the hold once.
    opened.compositor.render(&scene, false).expect("render");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(Arc::strong_count(&sync), holders);
}

/// A straight-alpha `Rgba8UnormSrgb` texture, the kind `import` takes, filled with one colour.
pub(crate) fn srgb_texture(
    compositor: &Compositor,
    width: u32,
    height: u32,
    rgba: [u8; 4],
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = compositor
        .device()
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-test-srgb"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
    let pixels: Vec<u8> = std::iter::repeat_n(rgba, (width * height) as usize)
        .flatten()
        .collect();
    compositor.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );
    texture
}

/// A `Rgba8Unorm` texture of one colour, `rgba` given as code values.
pub(crate) fn unorm_texture(
    compositor: &Compositor,
    width: u32,
    height: u32,
    rgba: [u8; 4],
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = compositor
        .device()
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-test-unorm"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
    let pixels: Vec<u8> = std::iter::repeat_n(rgba, (width * height) as usize)
        .flatten()
        .collect();
    compositor.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );
    texture
}

/// A [`FrameSync`] that counts how many times it is staged.
pub(crate) struct CountSync(pub(crate) Arc<AtomicUsize>);

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

/// A layer drawn into again keeps its texture and shows only the newest composite.
#[test]
fn a_layer_is_redrawn_in_place() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 2, 2, [255, 0, 0, 255]);
    let blue = solid(&opened.compositor, 1, 1, [0, 0, 255, 255]);
    let target = opened.compositor.layer(2, 2).expect("layer");
    let canvas = FrameSize::new(2, 2).expect("invariant: non-zero");
    let first = [item(&red, Placement::IDENTITY, Crop::NONE, 1.0)];
    opened
        .compositor
        .render_into(
            &Scene {
                canvas,
                background: Color::TRANSPARENT,
                items: &first,
            },
            &target,
        )
        .expect("first composite");
    let placed = [item(&target, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::BLACK, &placed);
    near(&snapshot, 1, 1, [255, 0, 0, 255], 0);

    // The second composite clears the layer: red must not show through.
    let second = [item(&blue, Placement::IDENTITY, Crop::NONE, 1.0)];
    opened
        .compositor
        .render_into(
            &Scene {
                canvas,
                background: Color::TRANSPARENT,
                items: &second,
            },
            &target,
        )
        .expect("second composite");
    let snapshot = draw(&mut opened.compositor, 2, 2, Color::BLACK, &placed);
    near(&snapshot, 0, 0, [0, 0, 255, 255], 0);
    near(&snapshot, 1, 1, [0, 0, 0, 255], 0);
}

/// A layer refuses to be drawn into while the scene places it, at another size, or
/// when it is not a layer at all.
#[test]
fn render_into_refuses_a_bad_target() {
    let Some(mut opened) = open() else {
        return;
    };
    let target = opened.compositor.layer(2, 2).expect("layer");
    let upload = solid(&opened.compositor, 2, 2, [255, 0, 0, 255]);
    let canvas = FrameSize::new(2, 2).expect("invariant: non-zero");
    let itself = [item(&target, Placement::IDENTITY, Crop::NONE, 1.0)];
    let cases: [(&[Item<'_>], FrameSize, &Image); 3] = [
        (&itself, canvas, &target),
        (
            &[],
            FrameSize::new(3, 2).expect("invariant: non-zero"),
            &target,
        ),
        (&[], canvas, &upload),
    ];
    for (items, canvas, target) in cases {
        let scene = Scene {
            canvas,
            background: Color::TRANSPARENT,
            items,
        };
        assert!(matches!(
            opened.compositor.render_into(&scene, target),
            Err(RenderError::Invalid { what: "layer", .. })
        ));
    }
    assert!(matches!(
        opened.compositor.layer(0, 2),
        Err(RenderError::Invalid { what: "layer", .. })
    ));
}

/// Rewriting an upload keeps its texture and changes what it shows; a layer is refused.
#[test]
fn an_upload_is_rewritten_in_place() {
    let Some(mut opened) = open() else {
        return;
    };
    let image = solid(&opened.compositor, 1, 1, [255, 0, 0, 255]);
    let before = image.texture().clone();
    opened
        .compositor
        .rewrite_rgba8(&image, &[0, 255, 0, 255])
        .expect("rewrite");
    assert!(image.texture() == &before);
    let placed = [item(&image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(&mut opened.compositor, 1, 1, Color::BLACK, &placed);
    near(&snapshot, 0, 0, [0, 255, 0, 255], 0);

    assert!(matches!(
        opened.compositor.rewrite_rgba8(&image, &[0, 0, 0]),
        Err(RenderError::ImageBytes { .. })
    ));
    let target = opened.compositor.layer(1, 1).expect("layer");
    assert!(matches!(
        opened.compositor.rewrite_rgba8(&target, &[0, 0, 0, 255]),
        Err(RenderError::Invalid { what: "image", .. })
    ));
}

/// The cache reuses a texture while the key's size holds, rewrites it for a new
/// version, and replaces it for a new size.
#[test]
fn the_cache_reuses_textures_by_key() {
    let Some(opened) = open() else {
        return;
    };
    let compositor = &opened.compositor;
    let mut cache: ResourceCache<&str> = ResourceCache::new();
    cache.begin_frame();
    let first = cache
        .upload(compositor, &"a", 1, 1, 1, &[255, 0, 0, 255])
        .expect("upload")
        .texture()
        .clone();
    let layer = cache
        .layer(compositor, &"layer", 2, 2)
        .expect("layer")
        .texture()
        .clone();
    drop(cache.end_frame(compositor));

    cache.begin_frame();
    let same = cache
        .upload(compositor, &"a", 1, 1, 1, &[0, 0, 0, 0])
        .expect("same version")
        .texture()
        .clone();
    let rewritten = cache
        .upload(compositor, &"a", 2, 1, 1, &[0, 255, 0, 255])
        .expect("new version")
        .texture()
        .clone();
    let same_layer = cache
        .layer(compositor, &"layer", 2, 2)
        .expect("layer")
        .texture()
        .clone();
    assert!(same == first && rewritten == first && same_layer == layer);
    assert_eq!(cache.pending(), 0);

    let resized = cache
        .upload(compositor, &"a", 3, 2, 1, &[0; 8])
        .expect("new size")
        .texture()
        .clone();
    assert!(resized != first);
    assert_eq!((cache.live(), cache.pending()), (2, 1));
}

/// An entry no frame uses is retired, and is handed back only once the GPU has
/// finished the last frame that used it.
#[test]
fn the_cache_frees_after_the_frame_fence() {
    let Some(opened) = open() else {
        return;
    };
    let compositor = &opened.compositor;
    let mut cache: ResourceCache<&str> = ResourceCache::new();
    let frame = cache.begin_frame();
    let _ = cache
        .upload(compositor, &"kept", 1, 1, 1, &[255, 0, 0, 255])
        .expect("upload");
    let _ = cache
        .upload(compositor, &"swapped", 1, 1, 1, &[255, 0, 0, 255])
        .expect("upload");
    // Replaced in the frame that used it: its fence is this frame's, not yet armed.
    let _ = cache
        .upload(compositor, &"swapped", 2, 2, 1, &[0; 8])
        .expect("resize");
    assert_eq!(cache.pending(), 1);
    assert!(cache.reclaim().is_none(), "frame {frame} has not ended");
    // `end_frame` polls without blocking, so a GPU that has already finished this
    // frame's (empty) work hands the replaced texture back here: count it too (#502).
    let first = cache.end_frame(compositor).map_or(0, |freed| freed.len());

    // The next frame uses only "kept": "swapped" retires after this frame's fence.
    cache.begin_frame();
    let _ = cache
        .upload(compositor, &"kept", 1, 1, 1, &[255, 0, 0, 255])
        .expect("reuse");
    let early = cache.end_frame(compositor).map_or(0, |freed| freed.len());
    compositor
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let late = cache.reclaim().map_or(0, |freed| freed.len());
    assert_eq!(first + early + late, 2, "both retired textures come back");
    assert_eq!((cache.live(), cache.pending()), (1, 0));
    assert_eq!(cache.drain().len(), 1);
}

/// A GPU frame in a new `Rgba8UnormSrgb` texture of one colour, with a hold that counts
/// its stages.
fn counted_frame(
    compositor: &Compositor,
    width: u32,
    height: u32,
    rgba: [u8; 4],
) -> (GpuFrame, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let info = VideoFrameInfo {
        size: FrameSize::new(width, height).unwrap(),
        format: PixelFormat::Rgba8,
        color: ColorInfo {
            range: jstream_media::ColorRange::Full,
            ..ColorInfo::default()
        },
        pts: MediaTime::zero(Timebase::NANOS),
    };
    let texture = srgb_texture(compositor, width, height, rgba);
    let frame = GpuFrame::new(texture, info, Arc::new(CountSync(Arc::clone(&count))));
    (frame, count)
}

/// One cached frame of `frame` under `"capture"`, drawn at the canvas origin: imports,
/// renders, ends the frame. Returns the pixel at (0, 0) and how many images the frame
/// handed back.
fn show(
    compositor: &mut Compositor,
    cache: &mut ResourceCache<&'static str>,
    frame: &GpuFrame,
) -> ([u8; 4], usize) {
    show_as(compositor, cache, "capture", frame)
}

/// [`show`] under `key`.
fn show_as(
    compositor: &mut Compositor,
    cache: &mut ResourceCache<&'static str>,
    key: &'static str,
    frame: &GpuFrame,
) -> ([u8; 4], usize) {
    cache.begin_frame();
    let image = cache.import(compositor, &key, frame).expect("import");
    let items = [item(image, Placement::IDENTITY, Crop::NONE, 1.0)];
    let snapshot = draw(compositor, 2, 2, Color::BLACK, &items);
    let freed = cache.end_frame(compositor).map_or(0, |freed| freed.len());
    let pixel = snapshot.pixel(0, 0).expect("inside");
    ([pixel[0], pixel[1], pixel[2], pixel[3]], freed)
}

/// The cache's opaque import reads the frame's alpha as 1 and is reused frame to frame,
/// and an import of the same texture under the same key as straight alpha replaces it.
#[test]
fn the_cache_imports_a_capture_as_opaque() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mut cache: ResourceCache<&'static str> = ResourceCache::new();
    let (frame, _) = counted_frame(compositor, 2, 2, [255, 0, 0, 128]);
    let mut pixel = |cache: &mut ResourceCache<&'static str>, opaque: bool| {
        cache.begin_frame();
        let image = if opaque {
            cache.import_opaque(compositor, &"capture", &frame)
        } else {
            cache.import(compositor, &"capture", &frame)
        }
        .expect("import");
        let items = [item(image, Placement::IDENTITY, Crop::NONE, 1.0)];
        let snapshot = draw(compositor, 2, 2, Color::BLACK, &items);
        drop(cache.end_frame(compositor));
        let pixel = snapshot.pixel(0, 0).expect("inside");
        [pixel[0], pixel[1], pixel[2], pixel[3]]
    };
    assert_eq!(pixel(&mut cache, true), [255, 0, 0, 255]);
    assert_eq!(pixel(&mut cache, true), [255, 0, 0, 255], "reused");
    let half = (linear_to_srgb(128.0 / 255.0) * 255.0).round() as u8;
    let straight = pixel(&mut cache, false);
    assert!(
        straight[0].abs_diff(half) <= 2,
        "a straight import of the same texture: {straight:?}"
    );
}

/// A GPU frame is sampled through the cache with no copy. Each frame stages its own
/// hold, and the cache lets go of it when the frame ends, so the platform can reuse
/// the texture. A ring of textures is imported once per texture; a new size retires
/// the imports of the old one.
#[test]
fn the_cache_imports_gpu_frames_without_keeping_their_holds() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mut cache: ResourceCache<&'static str> = ResourceCache::new();
    let (red, red_stages) = counted_frame(compositor, 2, 2, [255, 0, 0, 255]);
    let (green, green_stages) = counted_frame(compositor, 2, 2, [0, 255, 0, 255]);

    assert_eq!(show(compositor, &mut cache, &red), ([255, 0, 0, 255], 0));
    assert_eq!(red_stages.load(Ordering::SeqCst), 1);
    assert_eq!(
        Arc::strong_count(red.sync()),
        1,
        "nothing but the frame holds it once the frame has ended"
    );

    assert_eq!(show(compositor, &mut cache, &green), ([0, 255, 0, 255], 0));
    assert_eq!(green_stages.load(Ordering::SeqCst), 1);
    assert_eq!(Arc::strong_count(green.sync()), 1);
    assert_eq!((cache.live(), cache.kept_imports(&"capture")), (1, 1));

    // The ring comes round to the first texture again: its import is reused.
    assert_eq!(show(compositor, &mut cache, &red), ([255, 0, 0, 255], 0));
    assert_eq!(
        red_stages.load(Ordering::SeqCst),
        2,
        "each frame stages again"
    );
    assert_eq!(
        cache.get(&"capture").map(|image| image.texture().clone()),
        Some(red.texture().clone())
    );
    assert_eq!(
        (
            cache.live(),
            cache.kept_imports(&"capture"),
            cache.pending()
        ),
        (1, 1, 0)
    );

    // A frame of another size: both 2x2 imports are retired.
    let (blue, _) = counted_frame(compositor, 4, 4, [0, 0, 255, 255]);
    let (pixel, early) = show(compositor, &mut cache, &blue);
    assert_eq!(pixel, [0, 0, 255, 255]);
    assert_eq!(cache.kept_imports(&"capture"), 0);
    compositor
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let late = cache.reclaim().map_or(0, |freed| freed.len());
    assert_eq!(early + late, 2, "both 2x2 imports come back");
    assert_eq!(Arc::strong_count(blue.sync()), 1);

    // An upload under the same key replaces the import, and the import retires.
    cache.begin_frame();
    let _ = cache
        .upload(compositor, &"capture", 1, 1, 1, &[255, 255, 255, 255])
        .expect("upload");
    drop(cache.end_frame(compositor));
    assert_eq!((cache.live(), cache.kept_imports(&"capture")), (1, 0));
    assert!(
        cache
            .get(&"capture")
            .is_some_and(|image| image.texture() != blue.texture()),
        "the upload replaced the import"
    );
}

/// An image of equal-width vertical bands, one colour each, `height` rows tall.
pub(crate) fn bands(
    compositor: &Compositor,
    band_width: u32,
    height: u32,
    colors: &[[u8; 4]],
) -> crate::Image {
    let width = band_width * colors.len() as u32;
    let row: Vec<[u8; 4]> = colors
        .iter()
        .flat_map(|c| std::iter::repeat_n(*c, band_width as usize))
        .collect();
    let pixels: Vec<u8> = std::iter::repeat_n(row, height as usize)
        .flatten()
        .flatten()
        .collect();
    compositor
        .upload_rgba8(width, height, &pixels)
        .expect("invariant: the image bytes match")
}

fn geometry(
    center: [f32; 2],
    size: [f32; 2],
    fit_mode: jstream_protocol::FitMode,
    rotation_deg: f32,
) -> Placement {
    Placement::Geometry(jstream_protocol::ItemGeometry {
        fit_mode,
        rotation_deg: jstream_protocol::Finite32::new(rotation_deg).expect("finite"),
        ..jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(center[0], center[1]).expect("finite"),
            jstream_protocol::Size::try_new(size[0], size[1]).expect("positive"),
        )
        .expect("positive")
    })
}

/// The snapshot sampled every `step` pixels, at the centre of each `step`×`step` cell,
/// one character per sample: `.` black, `R`, `G`, `B`, `W`, or `?` for anything more than
/// a few levels from all of them.
///
/// Every edge in the golden scenes falls on a cell boundary, and every band boundary in
/// their images is more than a texel from a sample, so the grid is the same on any
/// conformant adapter.
fn grid(snapshot: &Snapshot, step: u32) -> String {
    const PALETTE: [(char, [u8; 3]); 5] = [
        ('.', [0, 0, 0]),
        ('R', [255, 0, 0]),
        ('G', [0, 255, 0]),
        ('B', [0, 0, 255]),
        ('W', [255, 255, 255]),
    ];
    let mut out = String::new();
    for y in (step / 2..snapshot.height()).step_by(step as usize) {
        for x in (step / 2..snapshot.width()).step_by(step as usize) {
            let pixel = snapshot
                .pixel(x, y)
                .expect("invariant: inside the snapshot");
            let class = PALETTE
                .iter()
                .find(|(_, rgb)| (0..3).all(|c| pixel[c].abs_diff(rgb[c]) <= 4))
                .map_or('?', |(name, _)| *name);
            out.push(class);
        }
        out.push('\n');
    }
    out
}

/// Golden test of ADR-0022 geometry on a 64×36 canvas, sampled every 4 pixels:
/// - fit: an 8×4 red|green image letterboxed into a 16×16 box centred at (12, 12);
/// - fill: a 32×8 red|green|blue|white image in a 16×16 box centred at (36, 12), clipped
///   to the box, so only the middle green|blue half shows;
/// - stretch, rotated 90° clockwise about the centre (56, 16): the red|green image in a
///   16×8 box, so red is on top;
/// - legacy: an 8×8 blue square with its top-left at (4, 24), beside the boxes.
#[test]
fn golden_geometry_fit_fill_stretch_and_rotation() {
    use jstream_protocol::FitMode;

    let Some(mut opened) = open() else {
        return;
    };
    const R: [u8; 4] = [255, 0, 0, 255];
    const G: [u8; 4] = [0, 255, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const W: [u8; 4] = [255, 255, 255, 255];
    let halves = bands(&opened.compositor, 4, 4, &[R, G]);
    let quarters = bands(&opened.compositor, 8, 8, &[R, G, B, W]);
    let blue = solid(&opened.compositor, 8, 8, B);
    let items = [
        item(
            &halves,
            geometry([12.0, 12.0], [16.0, 16.0], FitMode::Fit, 0.0),
            Crop::NONE,
            1.0,
        ),
        item(
            &quarters,
            geometry([36.0, 12.0], [16.0, 16.0], FitMode::Fill, 0.0),
            Crop::NONE,
            1.0,
        ),
        item(
            &halves,
            geometry([56.0, 16.0], [16.0, 8.0], FitMode::Stretch, 90.0),
            Crop::NONE,
            1.0,
        ),
        item(
            &blue,
            Placement::Legacy(LegacyTransform {
                position: [4.0, 24.0],
                ..LegacyTransform::IDENTITY
            }),
            Crop::NONE,
            1.0,
        ),
    ];
    let snapshot = draw(&mut opened.compositor, 64, 36, Color::BLACK, &items);
    let expected = "\
................
.......GGBB.....
.RRGG..GGBB..RR.
.RRGG..GGBB..RR.
.......GGBB..GG.
.............GG.
.BB.............
.BB.............
................
";
    assert_eq!(grid(&snapshot, 4), expected);
}

/// Golden test of crop with a geometry: cropping the red band off a red|green|blue image
/// leaves green|blue stretched across the whole 32×16 box, whatever the source size.
#[test]
fn golden_geometry_crops_before_fitting() {
    use jstream_protocol::FitMode;

    let Some(mut opened) = open() else {
        return;
    };
    let image = bands(
        &opened.compositor,
        8,
        8,
        &[[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]],
    );
    let crop = Crop {
        left: 8,
        ..Crop::NONE
    };
    let items = [item(
        &image,
        geometry([16.0, 8.0], [32.0, 16.0], FitMode::Stretch, 0.0),
        crop,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 32, 16, Color::BLACK, &items);
    assert_eq!(
        grid(&snapshot, 4),
        "GGGGBBBB\nGGGGBBBB\nGGGGBBBB\nGGGGBBBB\n"
    );
}

/// A geometry with an arbitrary rotation turns about its centre: the centre keeps the
/// image's colour, and a point just outside the unrotated box but inside the rotated
/// one is covered.
#[test]
fn a_rotated_geometry_turns_about_its_centre() {
    use jstream_protocol::FitMode;

    let Some(mut opened) = open() else {
        return;
    };
    let red = solid(&opened.compositor, 4, 4, [255, 0, 0, 255]);
    let items = [item(
        &red,
        geometry([32.0, 32.0], [40.0, 8.0], FitMode::Stretch, 90.0),
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 64, 64, Color::BLACK, &items);
    near(&snapshot, 32, 32, [255, 0, 0, 255], 1);
    // Vertical after the turn: (32, 16) is inside, (16, 32) is not.
    near(&snapshot, 32, 16, [255, 0, 0, 255], 1);
    near(&snapshot, 16, 32, [0, 0, 0, 255], 0);
}

/// A lost device (#349) is reported by `lost`, and drawing on it fails instead of
/// panicking, so the caller can open a new compositor. `destroy` stands in for a driver
/// reset: wgpu reports both through the same device-lost callback.
#[test]
fn a_lost_device_is_reported_and_fails_without_a_panic() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = Color::rgb(1.0, 0.0, 0.0);
    let image = solid(&opened.compositor, 2, 2, [0, 0, 255, 255]);
    let items = [item(&image, Placement::IDENTITY, Crop::NONE, 1.0)];
    draw(&mut opened.compositor, 4, 3, red, &items);
    assert_eq!(opened.compositor.lost(), None);

    opened.compositor.device().destroy();
    assert_eq!(opened.compositor.lost().as_deref(), Some("destroyed"));
    let scene = Scene {
        canvas: FrameSize::new(4, 3).expect("invariant: the test canvas is non-zero"),
        background: red,
        items: &items,
    };
    assert!(
        opened.compositor.render(&scene, true).is_err(),
        "a lost device draws nothing"
    );
    // Uploads and layers raise the invalid-resource errors a loss leaves; none panics.
    let _ = opened.compositor.upload_rgba8(1, 1, &[0, 0, 0, 255]);
    let _ = opened.compositor.render_image(&scene);
}

/// An uncaptured wgpu error fails the next frame instead of panicking, and a frame after
/// it draws again: wgpu's default handler would have panicked on the thread.
#[test]
fn an_uncaptured_error_fails_the_next_frame() {
    let Some(mut opened) = open() else {
        return;
    };
    let device = opened.compositor.device().clone();
    let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("zero-sized"),
            size: wgpu::Extent3d {
                width: 0,
                height: 0,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }));
    assert!(made.is_ok(), "the error is kept, not panicked on");
    let scene = Scene {
        canvas: FrameSize::new(4, 3).expect("invariant: the test canvas is non-zero"),
        background: Color::BLACK,
        items: &[],
    };
    let failed = opened.compositor.render(&scene, true).map(|_| ());
    assert!(
        matches!(failed, Err(RenderError::Uncaptured { .. })),
        "a zero-sized texture is a validation error: {failed:?}"
    );
    assert_eq!(opened.compositor.lost(), None);
    draw(&mut opened.compositor, 4, 3, Color::BLACK, &[]);
}

/// An image of four equal quadrants: `colors` is top-left, top-right, bottom-left,
/// bottom-right.
fn quadrants(compositor: &Compositor, side: u32, colors: [[u8; 4]; 4]) -> crate::Image {
    let half = side / 2;
    let pixels: Vec<u8> = (0..side)
        .flat_map(|y| (0..side).map(move |x| (x, y)))
        .flat_map(|(x, y)| colors[usize::from(y >= half) * 2 + usize::from(x >= half)])
        .collect();
    compositor
        .upload_rgba8(side, side, &pixels)
        .expect("invariant: the image bytes match")
}

/// [`geometry`] with `flip_h` and `flip_v` set.
fn flipped(
    center: [f32; 2],
    size: [f32; 2],
    fit_mode: jstream_protocol::FitMode,
    rotation_deg: f32,
    flip_h: bool,
    flip_v: bool,
) -> Placement {
    let Placement::Geometry(base) = geometry(center, size, fit_mode, rotation_deg) else {
        unreachable!("geometry is a geometry");
    };
    Placement::Geometry(jstream_protocol::ItemGeometry {
        flip_h,
        flip_v,
        ..base
    })
}

/// Golden test of a geometry's flips on a 64×32 canvas, sampled every 4 pixels. An 8×8
/// image with red, green, blue and white quadrants (top-left, top-right, bottom-left,
/// bottom-right) is stretched into 16×16 boxes:
/// - top row, left to right: unflipped, `flip_h`, `flip_v`, both;
/// - bottom left, at (8, 24): `flip_h` and turned 90° clockwise, so the mirror happens
///   in the box's own frame before the turn (green|red over white|blue, turned, is
///   white|green over blue|red);
/// - beside it, at (24, 24): the red|green|blue|white bands filling the box and flipped,
///   so the middle green|blue half shows mirrored as blue|green.
#[test]
fn golden_geometry_flips_mirror_the_content_in_the_box() {
    use jstream_protocol::FitMode;

    let Some(mut opened) = open() else {
        return;
    };
    const R: [u8; 4] = [255, 0, 0, 255];
    const G: [u8; 4] = [0, 255, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const W: [u8; 4] = [255, 255, 255, 255];
    let image = quadrants(&opened.compositor, 8, [R, G, B, W]);
    let quarters = bands(&opened.compositor, 8, 8, &[R, G, B, W]);
    let boxed = |x: f32, flip_h: bool, flip_v: bool| {
        item(
            &image,
            flipped(
                [x, 8.0],
                [16.0, 16.0],
                FitMode::Stretch,
                0.0,
                flip_h,
                flip_v,
            ),
            Crop::NONE,
            1.0,
        )
    };
    let items = [
        boxed(8.0, false, false),
        boxed(24.0, true, false),
        boxed(40.0, false, true),
        boxed(56.0, true, true),
        item(
            &image,
            flipped(
                [8.0, 24.0],
                [16.0, 16.0],
                FitMode::Stretch,
                90.0,
                true,
                false,
            ),
            Crop::NONE,
            1.0,
        ),
        item(
            &quarters,
            flipped([24.0, 24.0], [16.0, 16.0], FitMode::Fill, 0.0, true, false),
            Crop::NONE,
            1.0,
        ),
    ];
    let snapshot = draw(&mut opened.compositor, 64, 32, Color::BLACK, &items);
    let expected = "\
RRGGGGRRBBWWWWBB
RRGGGGRRBBWWWWBB
BBWWWWBBRRGGGGRR
BBWWWWBBRRGGGGRR
WWGGBBGG........
WWGGBBGG........
BBRRBBGG........
BBRRBBGG........
";
    assert_eq!(grid(&snapshot, 4), expected);
}

/// A flipped geometry draws the same pixels as the legacy negative scale it converts
/// from ([`jstream_protocol::ItemGeometry::from_legacy`]).
#[test]
fn a_flipped_geometry_matches_its_legacy_negative_scale() {
    let Some(mut opened) = open() else {
        return;
    };
    let image = quadrants(
        &opened.compositor,
        8,
        [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ],
    );
    // Top-left at (24, 4), reflected left to right: it covers x 8..24 and y 4..28.
    let transform = LegacyTransform {
        position: [24.0, 4.0],
        scale: [-2.0, 3.0],
        rotation_deg: 0.0,
    };
    let legacy = jstream_protocol::ItemGeometry::from_legacy(
        jstream_protocol::ItemTransform {
            position: jstream_protocol::Vec2::new(24.0, 4.0).expect("finite"),
            scale: jstream_protocol::Vec2::new(-2.0, 3.0).expect("finite"),
            rotation_deg: jstream_protocol::Finite32::ZERO,
        },
        jstream_protocol::Crop::NONE,
        8,
        8,
    )
    .expect("finite")
    .expect("drawn");
    assert!(legacy.geometry.flip_h && !legacy.geometry.flip_v);
    let old = draw(
        &mut opened.compositor,
        32,
        32,
        Color::BLACK,
        &[item(&image, Placement::Legacy(transform), Crop::NONE, 1.0)],
    );
    let new = draw(
        &mut opened.compositor,
        32,
        32,
        Color::BLACK,
        &[item(
            &image,
            Placement::Geometry(legacy.geometry),
            Crop::NONE,
            1.0,
        )],
    );
    let expected = "\
........
..GGRR..
..GGRR..
..GGRR..
..WWBB..
..WWBB..
..WWBB..
........
";
    assert_eq!(grid(&old, 4), expected);
    assert_eq!(grid(&new, 4), expected);
}
