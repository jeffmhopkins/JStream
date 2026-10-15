//! The built-in kinds through the public API: the registry's descriptors, the colour
//! source, and the image source on files in a temporary asset folder.
//!
//! PNG, GIF and lossless WebP files are encoded here. `tests/data` holds what no
//! dependency can encode, made once with Pillow: `solid.jpg` (16x8, rgb(200,30,40),
//! quality 95), `lossy.webp` (8x8, rgb(10,120,250), quality 90) and `anim.webp` (4x4,
//! lossless: red 100 ms, green 200 ms, blue at alpha 128 for 300 ms, loop count 2).

use jstream_media::{MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Limits, Registry, SourceError, needs_restart};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn ms(t: i64) -> MediaTime {
    MediaTime::new(t, Timebase::MILLIS)
}

/// A fresh, empty folder for one test.
fn folder(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sources-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    dir
}

fn ctx(collection: &Path) -> Context {
    Context {
        assets: AssetRoots {
            collection: Some(collection.to_owned()),
            app: None,
        },
        limits: Limits::default(),
    }
}

fn image_settings(path: &str) -> Settings {
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: path.into(),
    };
    [("file", file.to_value())].into_iter().collect()
}

/// Decodes `file` from `dir` with the image kind.
fn open(dir: &Path, file: &str) -> Result<Box<dyn jstream_sources::Source>, SourceError> {
    Registry::builtin().create("image", &image_settings(file), &ctx(dir))
}

fn pixel(frame: &VideoFrame, x: usize, y: usize) -> [u8; 4] {
    assert_eq!(frame.info().format, PixelFormat::Rgba8);
    let plane = &frame.planes()[0];
    let at = y * plane.stride() + x * 4;
    plane.data()[at..at + 4].try_into().expect("4 bytes")
}

fn size(frame: &VideoFrame) -> (u32, u32) {
    (frame.info().size.width(), frame.info().size.height())
}

fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= tolerance)
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

#[test]
fn the_registry_describes_every_builtin_kind_in_order() {
    let registry = Registry::builtin();
    let ids: Vec<String> = registry
        .descriptors()
        .into_iter()
        .map(|k| k.kind_id)
        .collect();
    assert_eq!(
        ids,
        [
            "color", "display", "image", "media", "text", "webcam", "window"
        ]
    );
    for kind in registry.descriptors() {
        for p in &kind.params {
            assert_eq!(p.check(&p.default), Ok(()), "{}.{}", kind.kind_id, p.id);
        }
    }
    let ctx = Context::default();
    assert!(matches!(
        registry.create("webcam", &Settings::new(), &ctx),
        Err(SourceError::NotBuilt { kind_id }) if kind_id == "webcam"
    ));
    assert!(matches!(
        registry.create("teleport", &Settings::new(), &ctx),
        Err(SourceError::UnknownKind { .. })
    ));
}

/// The settings the graph would store for a kind: its defaults, with `patch` over them.
fn complete(kind_id: &str, patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor(kind_id).expect("a kind");
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    for (id, value) in patch {
        settings.insert(*id, value.clone());
    }
    settings
}

#[test]
fn a_colour_source_is_one_solid_frame_and_its_colour_is_live() {
    let registry = Registry::builtin();
    let settings = complete(
        "color",
        &[
            ("color", json!("#ff800040")),
            ("width", json!(3)),
            ("height", json!(2)),
        ],
    );
    let mut source = registry
        .create("color", &settings, &Context::default())
        .expect("a source");
    let frame = source.frame(ms(0)).expect("a frame");
    assert_eq!(size(&frame), (3, 2));
    assert_eq!(frame.info().color.range, jstream_media::ColorRange::Full);
    assert!(
        frame.planes()[0]
            .data()
            .chunks(4)
            .all(|p| p == [0xff, 0x80, 0x00, 0x40])
    );
    assert_eq!(source.next_change(ms(0)), None);
    assert!(Arc::ptr_eq(
        &frame,
        &source.frame(ms(5000)).expect("a frame")
    ));

    let recoloured = complete(
        "color",
        &[
            ("color", json!("#00ff00")),
            ("width", json!(3)),
            ("height", json!(2)),
        ],
    );
    let descriptor = registry.descriptor("color").expect("color");
    assert!(!needs_restart(descriptor, &settings, &recoloured));
    source.update(&recoloured).expect("a live change");
    assert_eq!(
        pixel(&source.frame(ms(0)).expect("a frame"), 2, 1),
        [0, 0xff, 0, 0xff]
    );
    assert_eq!(
        pixel(&frame, 0, 0),
        [0xff, 0x80, 0x00, 0x40],
        "a frame already handed out stays"
    );

    let resized = complete(
        "color",
        &[
            ("color", json!("#00ff00")),
            ("width", json!(4)),
            ("height", json!(2)),
        ],
    );
    assert!(needs_restart(descriptor, &recoloured, &resized));
    assert!(
        matches!(source.update(&resized), Err(SourceError::NotLive { param }) if param == "width")
    );
}

#[test]
fn a_colour_source_rejects_bad_settings() {
    let registry = Registry::builtin();
    let ctx = Context::default();
    for (id, value) in [
        ("color", json!("red")),
        ("width", json!(0)),
        ("height", json!(20000)),
    ] {
        let settings = complete("color", &[(id, value)]);
        assert!(
            matches!(registry.create("color", &settings, &ctx), Err(SourceError::Setting { param, .. }) if param == id),
            "{id}"
        );
    }
}

fn write_png(path: &Path, width: u32, height: u32, color: png::ColorType, pixels: &[u8]) {
    let file = std::fs::File::create(path).expect("a file");
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a header");
    writer.write_image_data(pixels).expect("pixels");
}

#[test]
fn png_files_decode_to_rgba_in_every_colour_type() {
    let dir = folder("png");
    write_png(
        &dir.join("rgba.png"),
        2,
        1,
        png::ColorType::Rgba,
        &[1, 2, 3, 4, 5, 6, 7, 8],
    );
    write_png(
        &dir.join("rgb.png"),
        2,
        1,
        png::ColorType::Rgb,
        &[1, 2, 3, 4, 5, 6],
    );
    write_png(
        &dir.join("grey.png"),
        1,
        2,
        png::ColorType::Grayscale,
        &[9, 200],
    );
    write_png(
        &dir.join("grey-alpha.png"),
        1,
        1,
        png::ColorType::GrayscaleAlpha,
        &[9, 99],
    );

    let frame = |file: &str| open(&dir, file).expect(file).frame(ms(0)).expect("a frame");
    let rgba = frame("rgba.png");
    assert_eq!((size(&rgba), pixel(&rgba, 1, 0)), ((2, 1), [5, 6, 7, 8]));
    assert_eq!(pixel(&frame("rgb.png"), 1, 0), [4, 5, 6, 255]);
    let grey = frame("grey.png");
    assert_eq!(
        (size(&grey), pixel(&grey, 0, 1)),
        ((1, 2), [200, 200, 200, 255])
    );
    assert_eq!(pixel(&frame("grey-alpha.png"), 0, 0), [9, 9, 9, 99]);
    assert_eq!(
        open(&dir, "rgba.png").expect("png").next_change(ms(0)),
        None
    );
}

#[test]
fn jpeg_and_webp_files_decode() {
    let dir = folder("jpeg-webp");
    for name in ["solid.jpg", "lossy.webp", "anim.webp"] {
        std::fs::copy(data(name), dir.join(name)).expect("a fixture");
    }
    let mut lossless = Vec::new();
    image_webp::WebPEncoder::new(&mut lossless)
        .encode(
            &[10, 20, 30, 40, 50, 60, 70, 80],
            2,
            1,
            image_webp::ColorType::Rgba8,
        )
        .expect("a webp");
    std::fs::write(dir.join("still.webp"), lossless).expect("written");

    let jpeg = open(&dir, "solid.jpg")
        .expect("jpeg")
        .frame(ms(0))
        .expect("a frame");
    assert_eq!(size(&jpeg), (16, 8));
    assert!(
        close(pixel(&jpeg, 7, 3), [200, 30, 40, 255], 3),
        "{:?}",
        pixel(&jpeg, 7, 3)
    );

    let lossy = open(&dir, "lossy.webp")
        .expect("webp")
        .frame(ms(0))
        .expect("a frame");
    assert!(
        close(pixel(&lossy, 4, 4), [10, 120, 250, 255], 6),
        "{:?}",
        pixel(&lossy, 4, 4)
    );

    let still = open(&dir, "still.webp")
        .expect("webp")
        .frame(ms(0))
        .expect("a frame");
    assert_eq!(pixel(&still, 1, 0), [50, 60, 70, 80]);

    let mut anim = open(&dir, "anim.webp").expect("animated webp");
    // Pillow's encoder may move a channel by one even when lossless.
    let mut at = |t, expected, why: &str| {
        let got = pixel(&anim.frame(ms(t)).expect("a frame"), 0, 0);
        assert!(close(got, expected, 1), "{t} ms ({why}): {got:?}");
    };
    at(0, [255, 0, 0, 255], "red");
    at(150, [0, 255, 0, 255], "green");
    at(400, [0, 0, 255, 128], "translucent blue");
    at(650, [255, 0, 0, 255], "the second play");
    at(
        60_000,
        [0, 0, 255, 128],
        "two plays, then the last frame stays",
    );
    assert_eq!(anim.next_change(ms(650)), Some(ms(700)));
    assert_eq!(anim.next_change(ms(1150)), None);
}

/// A 2x2 GIF with three frames, each on a 1x1 or 2x1 rectangle, testing transparency
/// and the three disposal methods. Palette: 0 transparent, 1 red, 2 green, 3 blue.
fn write_gif(path: &Path, repeat: gif::Repeat) {
    let palette = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
    let file = std::fs::File::create(path).expect("a file");
    let mut encoder = gif::Encoder::new(file, 2, 2, &palette).expect("an encoder");
    encoder.set_repeat(repeat).expect("a loop count");
    let frame = |left, top, width, pixels: &[u8], delay, dispose| gif::Frame {
        left,
        top,
        width,
        height: 1,
        delay,
        dispose,
        transparent: Some(0),
        buffer: pixels.to_vec().into(),
        ..gif::Frame::default()
    };
    // The whole top row red; kept.
    encoder
        .write_frame(&frame(0, 0, 2, &[1, 1], 10, gif::DisposalMethod::Keep))
        .expect("frame 1");
    // Bottom-left green, top-right transparent over red; then restored to before.
    encoder
        .write_frame(&frame(0, 1, 1, &[2], 20, gif::DisposalMethod::Previous))
        .expect("frame 2");
    // Bottom-right blue, with a delay of 0 that shows for 100 ms; then cleared.
    encoder
        .write_frame(&frame(1, 1, 1, &[3], 0, gif::DisposalMethod::Background))
        .expect("frame 3");
}

#[test]
fn an_animated_gif_composites_its_frames_and_loops() {
    let dir = folder("gif");
    write_gif(&dir.join("anim.gif"), gif::Repeat::Infinite);
    let mut source = open(&dir, "anim.gif").expect("gif");
    let (red, green, blue, clear) = ([255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [0; 4]);
    let all = |source: &mut Box<dyn jstream_sources::Source>, t| {
        let f = source.frame(ms(t)).expect("a frame");
        [
            pixel(&f, 0, 0),
            pixel(&f, 1, 0),
            pixel(&f, 0, 1),
            pixel(&f, 1, 1),
        ]
    };
    assert_eq!(all(&mut source, 0), [red, red, clear, clear]);
    assert_eq!(all(&mut source, 100), [red, red, green, clear]);
    assert_eq!(
        all(&mut source, 300),
        [red, red, clear, blue],
        "frame 2 was restored"
    );
    assert_eq!(source.next_change(ms(300)), Some(ms(400)));
    assert_eq!(
        all(&mut source, 400),
        [red, red, clear, clear],
        "loops forever"
    );
    assert_eq!(all(&mut source, 4000 + 350), [red, red, clear, blue]);

    write_gif(&dir.join("once.gif"), gif::Repeat::Finite(0));
    let mut once = open(&dir, "once.gif").expect("gif");
    assert_eq!(
        all(&mut once, 450),
        [red, red, clear, blue],
        "one play, then the last frame"
    );
    assert_eq!(once.next_change(ms(0)), Some(ms(100)));
    assert_eq!(once.next_change(ms(300)), None);
}

#[test]
fn unreadable_and_oversized_images_are_errors() {
    // `wide.png` takes 27 bytes of scanlines while decoding and 36 bytes as RGBA.
    let dir = folder("errors");
    std::fs::write(dir.join("text.png"), b"not an image").expect("written");
    std::fs::write(dir.join("cut.png"), &b"\x89PNG\r\n\x1a\n\0\0"[..]).expect("written");
    write_png(&dir.join("wide.png"), 9, 1, png::ColorType::Rgb, &[0; 27]);

    assert!(matches!(
        open(&dir, "missing.png"),
        Err(SourceError::AssetRead {
            kind: std::io::ErrorKind::NotFound,
            ..
        })
    ));
    assert!(matches!(
        open(&dir, "text.png"),
        Err(SourceError::UnsupportedFormat { .. })
    ));
    assert!(matches!(
        open(&dir, "cut.png"),
        Err(SourceError::Decode { format: "PNG", .. })
    ));

    let small = |limits: Limits| {
        let ctx = Context {
            limits,
            ..ctx(&dir)
        };
        Registry::builtin().create("image", &image_settings("wide.png"), &ctx)
    };
    let defaults = Limits::default();
    assert!(matches!(
        small(Limits {
            max_side: 8,
            ..defaults
        }),
        Err(SourceError::TooLarge { .. })
    ));
    assert!(matches!(
        small(Limits {
            max_decoded_bytes: 62,
            ..defaults
        }),
        Err(SourceError::TooLarge { .. })
    ));
    assert!(matches!(
        small(Limits {
            max_file_bytes: 10,
            ..defaults
        }),
        Err(SourceError::TooLarge { .. })
    ));
    assert!(
        small(Limits {
            max_decoded_bytes: 63,
            ..defaults
        })
        .is_ok()
    );
}

#[test]
fn an_image_with_no_file_is_one_transparent_pixel() {
    let settings = complete("image", &[]);
    let mut source = Registry::builtin()
        .create("image", &settings, &Context::default())
        .expect("a source");
    let frame = source.frame(ms(0)).expect("a frame");
    assert_eq!((size(&frame), pixel(&frame, 0, 0)), ((1, 1), [0; 4]));
    assert!(matches!(
        source.update(&image_settings("a.png")),
        Err(SourceError::NotLive { param }) if param == "file"
    ));
}

#[test]
fn assets_resolve_inside_their_root_only() {
    let dir = folder("roots");
    let root = dir.join("collection");
    std::fs::create_dir_all(root.join("img")).expect("a folder");
    write_png(
        &root.join("img/a.png"),
        1,
        1,
        png::ColorType::Rgb,
        &[1, 2, 3],
    );
    write_png(
        &dir.join("outside.png"),
        1,
        1,
        png::ColorType::Rgb,
        &[1, 2, 3],
    );
    let roots = AssetRoots {
        collection: Some(root.clone()),
        app: None,
    };
    let found = roots
        .resolve(AssetRoot::Collection, "img/a.png")
        .expect("inside");
    assert!(found.ends_with("img/a.png"));
    let bytes = std::fs::read(root.join("img/a.png")).expect("the file");
    let read = roots
        .read(AssetRoot::Collection, "img/a.png", 1 << 20)
        .expect("read inside");
    assert_eq!(read, bytes);
    assert!(matches!(
        roots.read(AssetRoot::Collection, "img", 1 << 20),
        Err(SourceError::AssetRead { .. })
    ));
    assert!(matches!(
        roots.resolve(AssetRoot::App, "a.png"),
        Err(SourceError::NoAssetRoot {
            root: AssetRoot::App
        })
    ));
    let absolute = dir.join("outside.png");
    let absolute = absolute.to_str().expect("a UTF-8 path");
    assert!(roots.resolve(AssetRoot::Absolute, absolute).is_ok());

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("outside.png"), root.join("link.png"))
            .expect("a symlink");
        assert!(matches!(
            roots.resolve(AssetRoot::Collection, "link.png"),
            Err(SourceError::AssetEscapesRoot {
                root: AssetRoot::Collection,
                ..
            })
        ));
        assert!(matches!(
            roots.read(AssetRoot::Collection, "link.png", 1 << 20),
            Err(SourceError::AssetEscapesRoot { .. })
        ));
        // A symlink that stays inside the root is followed.
        std::os::unix::fs::symlink(root.join("img"), root.join("pics")).expect("a symlink");
        assert_eq!(
            roots
                .read(AssetRoot::Collection, "pics/a.png", 1 << 20)
                .expect("read through a link inside the root"),
            bytes
        );
        let err = open(&root, "link.png").err().expect("an error").to_string();
        assert!(
            !err.contains(absolute),
            "errors never name a host path: {err}"
        );
    }
}

/// The decoded-memory cap counts a GIF's working canvas and its "restore to previous"
/// copy, not only the frames kept: the 2x2 file above keeps 3 frames of 16 bytes, holds
/// a 16-byte canvas throughout, and a 16-byte copy while frame 2 is drawn.
#[test]
fn a_gif_working_canvas_counts_against_the_cap() {
    let dir = folder("gif-cap");
    write_gif(&dir.join("anim.gif"), gif::Repeat::Infinite);
    let open_with = |max_decoded_bytes| {
        let limits = Limits {
            max_decoded_bytes,
            ..Limits::default()
        };
        let ctx = Context {
            limits,
            ..ctx(&dir)
        };
        Registry::builtin().create("image", &image_settings("anim.gif"), &ctx)
    };
    assert!(matches!(open_with(63), Err(SourceError::TooLarge { .. })));
    assert!(open_with(64).is_ok());
}

/// The cap counts what `image-webp` allocates for pixels, which its own limit doesn't:
/// `anim.webp` is a 4x4 canvas (64 bytes) with alpha, so decoding it holds the output
/// buffer (64), the decoder's frame and canvas (128) and three kept frames (192).
#[test]
fn a_webp_animation_counts_the_decoders_buffers() {
    let dir = folder("webp-cap");
    std::fs::copy(data("anim.webp"), dir.join("anim.webp")).expect("a fixture");
    let open_with = |max_decoded_bytes| {
        let limits = Limits {
            max_decoded_bytes,
            ..Limits::default()
        };
        let ctx = Context {
            limits,
            ..ctx(&dir)
        };
        Registry::builtin().create("image", &image_settings("anim.webp"), &ctx)
    };
    assert!(matches!(open_with(383), Err(SourceError::TooLarge { .. })));
    assert!(open_with(384).is_ok());
}
