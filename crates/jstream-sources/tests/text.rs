//! The `text` kind through the public API.
//!
//! Every test draws with the bundled font, `assets/fonts/Roboto-Regular-subset.ttf`
//! (Apache-2.0; the crate README's "Bundled font" says how it was made). The golden
//! tests compare against PNGs in `tests/data/text`; run them with `JSTREAM_BLESS=1` to
//! rewrite those after a deliberate change, and look at the new images before
//! committing them.

use jstream_media::{MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Limits, Registry, SourceError, needs_restart};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn ms(t: i64) -> MediaTime {
    MediaTime::new(t, Timebase::MILLIS)
}

/// The text kind's defaults, with `patch` over them, as the graph would store them.
fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("text").expect("the text kind");
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

fn draw_with(patch: &[(&str, Value)], ctx: &Context) -> Result<VideoFrame, SourceError> {
    let mut source = Registry::builtin().create("text", &settings(patch), ctx)?;
    let frame = source.frame(ms(0))?;
    Ok((*frame).clone())
}

fn draw(patch: &[(&str, Value)]) -> VideoFrame {
    draw_with(patch, &Context::default()).expect("a text frame")
}

fn size(frame: &VideoFrame) -> (u32, u32) {
    (frame.info().size.width(), frame.info().size.height())
}

/// The frame's pixels as packed RGBA, row by row.
fn pixels(frame: &VideoFrame) -> Vec<[u8; 4]> {
    assert_eq!(frame.info().format, PixelFormat::Rgba8);
    let plane = &frame.planes()[0];
    let (w, h) = size(frame);
    (0..h as usize)
        .flat_map(|y| {
            let row = &plane.data()[y * plane.stride()..][..w as usize * 4];
            row.chunks(4)
                .map(|p| p.try_into().expect("4 bytes"))
                .collect::<Vec<[u8; 4]>>()
        })
        .collect()
}

/// The columns and rows holding any pixel with alpha: (first x, last x, first y, last y).
fn ink(frame: &VideoFrame) -> (u32, u32, u32, u32) {
    let (w, _) = size(frame);
    let mut bounds = (u32::MAX, 0, u32::MAX, 0);
    for (i, p) in pixels(frame).iter().enumerate() {
        if p[3] > 0 {
            let (x, y) = (i as u32 % w, i as u32 / w);
            bounds = (
                bounds.0.min(x),
                bounds.1.max(x),
                bounds.2.min(y),
                bounds.3.max(y),
            );
        }
    }
    bounds
}

fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/text")
        .join(format!("{name}.png"))
}

/// Compares `frame` with the golden PNG `name`: alpha, and colour premultiplied by it,
/// each within 2, so a nearly transparent edge pixel's colour doesn't count. With
/// `JSTREAM_BLESS` set, writes the PNG instead.
fn check_golden(name: &str, frame: &VideoFrame) {
    let (w, h) = size(frame);
    let rgba: Vec<u8> = pixels(frame).concat();
    let path = golden(name);
    if std::env::var_os("JSTREAM_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("the golden folder");
        let file = std::fs::File::create(&path).expect("a golden file");
        let mut encoder = png::Encoder::new(file, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("a PNG header");
        writer.write_image_data(&rgba).expect("PNG data");
        return;
    }
    let file = std::fs::File::open(&path).expect("the golden PNG; run with JSTREAM_BLESS=1");
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .expect("a PNG");
    let mut want = vec![
        0;
        reader
            .output_buffer_size()
            .expect("a PNG that fits in memory")
    ];
    let info = reader.next_frame(&mut want).expect("PNG data");
    assert_eq!((info.width, info.height), (w, h), "{name}: size");
    assert_eq!(info.color_type, png::ColorType::Rgba, "{name}: colour type");
    let premultiplied = |p: &[u8]| {
        let a = u32::from(p[3]);
        let c = |i: usize| u32::from(p[i]) * a / 255;
        [c(0), c(1), c(2), a]
    };
    let worst = rgba
        .chunks(4)
        .zip(want.chunks(4))
        .flat_map(|(a, b)| {
            let (a, b) = (premultiplied(a), premultiplied(b));
            (0..4).map(move |i| a[i].abs_diff(b[i]))
        })
        .max()
        .unwrap_or(0);
    assert!(worst <= 2, "{name}: a channel differs by {worst}");
}

#[test]
fn golden_plain_text() {
    let frame = draw(&[("text", json!("Hello, JStream!")), ("size", json!(32))]);
    check_golden("plain", &frame);
}

#[test]
fn golden_wrapped_centred_outlined_text_on_a_background() {
    let frame = draw(&[
        (
            "text",
            json!("Wrapped and centred text, with an outline.\nÅngström façade"),
        ),
        ("size", json!(24)),
        ("color", json!("#ffcc00ff")),
        ("align", json!("center")),
        ("wrap_width", json!(200)),
        ("outline_width", json!(3)),
        ("outline_color", json!("#202040ff")),
        ("background", json!("#3060a080")),
        ("padding", json!(8)),
    ]);
    check_golden("styled", &frame);
}

#[test]
fn the_descriptor_marks_everything_but_the_font_live() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("text").expect("the text kind");
    let ids: Vec<(&str, bool)> = kind
        .params
        .iter()
        .map(|p| (p.id.as_str(), p.live))
        .collect();
    assert_eq!(
        ids,
        [
            ("text", true),
            ("font", false),
            ("size", true),
            ("color", true),
            ("align", true),
            ("wrap_width", true),
            ("outline_width", true),
            ("outline_color", true),
            ("background", true),
            ("padding", true),
        ]
    );
    assert!(kind.video && !kind.audio);
}

#[test]
fn colour_is_straight_alpha_and_the_frame_fits_the_text() {
    let frame = draw(&[("text", json!("Hi")), ("color", json!("#ff000080"))]);
    let (w, h) = size(&frame);
    // 64 px Roboto: 1.32 em from ascent to descent, and "Hi" is about an em wide.
    assert!((80..=90).contains(&h), "height {h}");
    assert!((50..=80).contains(&w), "width {w}");
    let px = pixels(&frame);
    assert!(px.iter().all(|p| p[3] <= 0x80));
    // Inside the stems, colour is the setting's own, not scaled by alpha.
    let solid: Vec<_> = px.iter().filter(|p| p[3] == 0x80).collect();
    assert!(!solid.is_empty());
    assert!(solid.iter().all(|p| p[..3] == [0xff, 0, 0]));
    // Uncovered pixels are fully transparent.
    assert!(px.contains(&[0, 0, 0, 0]));
}

#[test]
fn lines_break_on_newlines_and_wrap_at_spaces() {
    let one = draw(&[("text", json!("one two"))]);
    let two = draw(&[("text", json!("one\ntwo"))]);
    let (w1, h1) = size(&one);
    let (w2, h2) = size(&two);
    assert!(w2 < w1);
    // Roboto's line gap is 0, so a second line adds one line height.
    assert!(h2.abs_diff(2 * h1) <= 1, "{h1} then {h2}");
    // Wrapping at less than "one two" is wide breaks at the space, the same as "\n".
    let wrapped = draw(&[("text", json!("one two")), ("wrap_width", json!(w1 - 10))]);
    assert_eq!(size(&wrapped), (w1 - 10, h2));
    // A word wider than the wrap breaks inside the word.
    let narrow = draw(&[("text", json!("abcdefgh")), ("wrap_width", json!(60))]);
    assert_eq!(size(&narrow).0, 60);
    assert!(size(&narrow).1 >= 2 * h1 - 1);
}

#[test]
fn alignment_moves_lines_within_the_wrap_width() {
    let at = |align: &str| {
        ink(&draw(&[
            ("text", json!("x")),
            ("wrap_width", json!(400)),
            ("align", json!(align)),
        ]))
    };
    let (left, centre, right) = (at("left"), at("center"), at("right"));
    assert!(left.0 < 10, "{left:?}");
    assert!(right.1 > 390, "{right:?}");
    let middle = (centre.0 + centre.1) / 2;
    assert!(middle.abs_diff(200) <= 2, "{centre:?}");
}

#[test]
fn padding_outline_and_background_grow_and_fill_the_frame() {
    let plain = draw(&[("text", json!("Pad"))]);
    let padded = draw(&[
        ("text", json!("Pad")),
        ("padding", json!(10)),
        ("background", json!("#00ff00ff")),
    ]);
    let (w, h) = size(&plain);
    assert_eq!(size(&padded), (w + 20, h + 20));
    // The background reaches every corner.
    let px = pixels(&padded);
    assert_eq!(px[0], [0, 0xff, 0, 0xff]);
    assert_eq!(*px.last().expect("pixels"), [0, 0xff, 0, 0xff]);

    let outlined = draw(&[
        ("text", json!("Pad")),
        ("outline_width", json!(4)),
        ("outline_color", json!("#0000ffff")),
    ]);
    // The outline stays inside the line box here, so the frame only grows if it
    // reaches past the glyphs' side bearings.
    let (ow, oh) = size(&outlined);
    assert!(ow >= w && oh >= h, "{w}x{h} then {ow}x{oh}");
    let blue = pixels(&outlined)
        .into_iter()
        .filter(|p| *p == [0, 0, 0xff, 0xff])
        .count();
    assert!(blue > 100, "{blue} outline pixels");
}

#[test]
fn empty_text_is_a_transparent_frame() {
    let frame = draw(&[]);
    assert!(pixels(&frame).iter().all(|p| *p == [0, 0, 0, 0]));
    assert!(size(&frame).1 > 0);
}

#[test]
fn style_changes_are_live_and_a_font_change_restarts() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("text").expect("the text kind");
    let old = settings(&[("text", json!("a"))]);
    let mut source = registry
        .create("text", &old, &Context::default())
        .expect("a source");
    let before = source.frame(ms(0)).expect("a frame");
    assert_eq!(source.next_change(ms(0)), None);

    let new = settings(&[("text", json!("abc")), ("size", json!(20))]);
    assert!(!needs_restart(kind, &old, &new));
    source.update(&new).expect("a live update");
    let after = source.frame(ms(5)).expect("a frame");
    assert_ne!(size(&before), size(&after));
    assert_eq!(
        size(&after),
        size(&draw(&[("text", json!("abc")), ("size", json!(20))]))
    );

    let font = Reference::Asset {
        root: AssetRoot::Collection,
        path: "font.ttf".into(),
    };
    let moved = settings(&[("text", json!("abc")), ("font", font.to_value())]);
    assert!(needs_restart(kind, &new, &moved));
    assert!(matches!(
        source.update(&moved),
        Err(SourceError::NotLive { param }) if param == "font"
    ));
    assert!(matches!(
        source.update(&settings(&[("align", json!("justify"))])),
        Err(SourceError::Setting { param, .. }) if param == "align"
    ));
}

/// A fresh, empty folder for one test.
fn folder(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("text-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    dir
}

#[test]
fn a_font_asset_is_read_and_a_bad_one_is_an_error() {
    let dir = folder("fonts");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Roboto-Regular-subset.ttf");
    std::fs::copy(&fixture, dir.join("roboto.ttf")).expect("a font copy");
    std::fs::write(dir.join("broken.ttf"), b"not a font").expect("a file");
    let ctx = Context {
        assets: AssetRoots {
            collection: Some(dir.clone()),
            app: None,
        },
        limits: Limits::default(),
        media: None,
    };
    let font = |path: &str| {
        Reference::Asset {
            root: AssetRoot::Collection,
            path: path.into(),
        }
        .to_value()
    };
    let text = ("text", json!("Same font"));
    let from_file = draw_with(&[text.clone(), ("font", font("roboto.ttf"))], &ctx)
        .expect("text in the asset font");
    assert_eq!(
        pixels(&from_file),
        pixels(&draw(std::slice::from_ref(&text)))
    );

    assert!(matches!(
        draw_with(&[text.clone(), ("font", font("broken.ttf"))], &ctx),
        Err(SourceError::Decode { path, .. }) if path == "broken.ttf"
    ));
    assert!(matches!(
        draw_with(&[text, ("font", font("missing.ttf"))], &ctx),
        Err(SourceError::AssetRead { .. })
    ));
}

#[test]
fn text_too_large_for_the_limits_is_an_error() {
    let small = Context {
        limits: Limits {
            max_side: 100,
            ..Limits::default()
        },
        ..Context::default()
    };
    assert!(matches!(
        draw_with(&[("text", json!("far too wide for 100 px"))], &small),
        Err(SourceError::Setting { param, .. }) if param == "text"
    ));
    let tight = Context {
        limits: Limits {
            max_decoded_bytes: 1000,
            ..Limits::default()
        },
        ..Context::default()
    };
    assert!(matches!(
        draw_with(&[("text", json!("x"))], &tight),
        Err(SourceError::Setting { param, .. }) if param == "text"
    ));
}
