//! Pixel checks on whatever adapter [`Compositor::open`] finds. With no adapter the test
//! writes a line and returns; in CI that is a failure, because the workflow installs one.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jstream_media::{ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrameInfo};
use jstream_platform::{FrameSync, GpuFrame, PlatformError};

use crate::{
    Color, Compositor, Crop, Image, Item, OpenOptions, Placement, RenderError, ResourceCache,
    Scene, Snapshot,
};

/// A compositor whose device drops before the WARP lock.
///
/// Fields drop in declaration order, so the device is destroyed while the lock is held
/// for as long as this value is kept whole. Splitting the fields apart drops them as
/// separate locals and can release the lock first.
struct Opened {
    compositor: Compositor,
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// WARP (Microsoft Basic Render Driver) access-violates, `0xc0000005`, when several
/// tests create and drop Dx12 devices at once.
fn open() -> Option<Opened> {
    let _lock = gpu_lock();
    match Compositor::open(OpenOptions::headless()) {
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
            Placement {
                position: [2.0, 2.0],
                ..Placement::IDENTITY
            },
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
        Placement {
            scale: [2.0, 2.0],
            ..Placement::IDENTITY
        },
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
        Placement {
            position: [30.0, 30.0],
            rotation_deg: 90.0,
            ..Placement::IDENTITY
        },
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
        Placement {
            position: [0.5, 0.0],
            ..Placement::IDENTITY
        },
        Crop::NONE,
        1.0,
    )];
    let snapshot = draw(&mut opened.compositor, 3, 1, Color::TRANSPARENT, &offset);
    near(&snapshot, 1, 0, [255, 0, 0, 128], 2);

    // Twice as wide: pixel 1 is 3/4 red texel, pixel 2 is 1/4. Straight filtering would
    // give red 225 and 137 there.
    let scaled = [item(
        &image,
        Placement {
            scale: [2.0, 1.0],
            ..Placement::IDENTITY
        },
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
    let at_one = Placement {
        position: [1.0, 1.0],
        ..Placement::IDENTITY
    };
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
        Placement {
            position: [2.0, 2.0],
            ..Placement::IDENTITY
        },
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
    let double = Placement {
        scale: [2.0, 2.0],
        ..Placement::IDENTITY
    };
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
            Placement {
                position: [1.0, 0.0],
                ..Placement::IDENTITY
            },
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

/// A straight-alpha `Rgba8UnormSrgb` texture, the kind `import` takes, filled with one colour.
fn srgb_texture(compositor: &Compositor, width: u32, height: u32, rgba: [u8; 4]) -> wgpu::Texture {
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
    drop(cache.end_frame(compositor));

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
    assert_eq!(early + late, 2, "both retired textures come back");
    assert_eq!((cache.live(), cache.pending()), (1, 0));
    assert_eq!(cache.drain().len(), 1);
}
