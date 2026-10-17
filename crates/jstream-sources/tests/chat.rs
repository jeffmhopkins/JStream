//! The `chat_message` kind through the public API, drawn with the bundled font.

use jstream_media::{MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_protocol::Settings;
use jstream_sources::{
    ChatFeature, Context, FeaturedMessage, Registry, Source, SourceError, needs_restart,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn ms(t: i64) -> MediaTime {
    MediaTime::new(t, Timebase::MILLIS)
}

/// The kind's defaults, with `patch` over them, as the graph would store them.
fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("chat_message").expect("the chat kind");
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

/// What a host shows at `at`: it paints first, off any lock, as the engine's sources
/// thread does when `next_change` says the frame is due, then asks for the frame.
fn shown(source: &mut dyn Source, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
    source.painter().expect("a painter").paint(at);
    source.frame(at)
}

fn size(frame: &VideoFrame) -> (u32, u32) {
    (frame.info().size.width(), frame.info().size.height())
}

fn pixel(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
    assert_eq!(frame.info().format, PixelFormat::Rgba8);
    let plane = &frame.planes()[0];
    let at = y as usize * plane.stride() + x as usize * 4;
    plane.data()[at..at + 4].try_into().expect("4 bytes")
}

fn message(text: &str) -> FeaturedMessage {
    FeaturedMessage {
        author: "Viewer".into(),
        badge: Some("Moderator".into()),
        text: text.into(),
        amount: None,
    }
}

#[test]
fn the_kind_is_registered_with_live_styling() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("chat_message").expect("registered");
    assert!(kind.video && !kind.audio);
    let ids: Vec<_> = kind.params.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "font",
            "size",
            "author_color",
            "text_color",
            "background",
            "padding",
            "max_width",
            "outline_width",
            "outline_color"
        ]
    );
    // Only the font restarts the source.
    let old = settings(&[]);
    let restyled = settings(&[("size", json!(48.0)), ("background", json!("#00000000"))]);
    assert!(!needs_restart(kind, &old, &restyled));
}

#[test]
fn nothing_featured_draws_one_transparent_pixel() {
    let ctx = Context::default();
    let mut source = Registry::builtin()
        .create("chat_message", &settings(&[]), &ctx)
        .expect("a source");
    let frame = shown(&mut *source, ms(0)).expect("a frame");
    assert_eq!(size(&frame), (1, 1));
    assert_eq!(pixel(&frame, 0, 0), [0, 0, 0, 0]);
    // The featured message can change at any time, so the source asks to be looked at.
    assert_eq!(source.next_change(ms(1000)), Some(ms(1100)));
}

#[test]
fn a_featured_message_is_drawn_and_redrawn_when_it_changes() {
    let ctx = Context::default();
    let feature: &ChatFeature = &ctx.chat;
    let padding = 10;
    let mut source = Registry::builtin()
        .create(
            "chat_message",
            &settings(&[
                ("background", json!("#102030ff")),
                ("padding", json!(padding)),
            ]),
            &ctx,
        )
        .expect("a source");

    feature.set(Some(message("hello there")));
    let first = shown(&mut *source, ms(0)).expect("a frame");
    let (w, h) = size(&first);
    assert!(w > 50 && h > 40, "{w}x{h}");
    // The padding is the background colour.
    assert_eq!(pixel(&first, 0, 0), [0x10, 0x20, 0x30, 0xff]);
    assert_eq!(pixel(&first, w - 1, h - 1), [0x10, 0x20, 0x30, 0xff]);

    // The same frame comes back until the message changes.
    let again = shown(&mut *source, ms(100)).expect("a frame");
    assert!(Arc::ptr_eq(&first, &again));

    feature.set(Some(message(
        "a much longer message that takes more room across",
    )));
    let longer = shown(&mut *source, ms(200)).expect("a frame");
    assert!(size(&longer).0 > w);

    feature.set(None);
    assert_eq!(
        size(&shown(&mut *source, ms(300)).expect("a frame")),
        (1, 1)
    );
}

#[test]
fn long_messages_wrap_at_the_maximum_width() {
    let ctx = Context::default();
    ctx.chat.set(Some(message(&"word ".repeat(200))));
    let mut source = Registry::builtin()
        .create(
            "chat_message",
            &settings(&[("max_width", json!(300)), ("padding", json!(10))]),
            &ctx,
        )
        .expect("a source");
    let frame = shown(&mut *source, ms(0)).expect("a frame");
    let (w, h) = size(&frame);
    assert!(w <= 300, "{w}");
    assert!(h > 200, "{h}");
}

#[test]
fn markup_and_control_characters_are_plain_text() {
    // Drawn as the characters they are: the frame is as wide as the literal text.
    let ctx = Context::default();
    let mut source = Registry::builtin()
        .create("chat_message", &settings(&[("max_width", json!(0))]), &ctx)
        .expect("a source");
    let short = |text: &str| FeaturedMessage {
        author: "a".into(),
        badge: None,
        text: text.into(),
        amount: None,
    };
    ctx.chat.set(Some(short("<b>x</b>")));
    let tagged = size(&shown(&mut *source, ms(0)).expect("a frame")).0;
    ctx.chat.set(Some(short("x")));
    let bare = size(&shown(&mut *source, ms(1)).expect("a frame")).0;
    assert!(tagged > bare);
    // A line break in a name doesn't add a line.
    ctx.chat.set(Some(FeaturedMessage {
        author: "two\nlines".into(),
        ..message("x")
    }));
    let one_line = size(&shown(&mut *source, ms(2)).expect("a frame")).1;
    ctx.chat.set(Some(FeaturedMessage {
        author: "two lines".into(),
        ..message("x")
    }));
    assert_eq!(
        size(&shown(&mut *source, ms(3)).expect("a frame")).1,
        one_line
    );
}

#[test]
fn a_restyle_redraws_and_the_font_needs_a_restart() {
    let ctx = Context::default();
    ctx.chat.set(Some(message("hi")));
    let mut source = Registry::builtin()
        .create("chat_message", &settings(&[]), &ctx)
        .expect("a source");
    let small = size(&shown(&mut *source, ms(0)).expect("a frame"));
    source
        .update(&settings(&[("size", json!(64.0))]))
        .expect("a live update");
    let large = size(&shown(&mut *source, ms(1)).expect("a frame"));
    assert!(large.0 > small.0 && large.1 > small.1);

    let font = json!({"asset": {"root": "app", "path": "other.ttf"}});
    assert!(matches!(
        source.update(&settings(&[("font", font)])),
        Err(SourceError::NotLive { .. })
    ));
}

/// Painting never fails the source: a featured message the settings can't draw shows
/// nothing, and the next one is drawn (review B2 on #363).
#[test]
fn a_message_too_large_to_draw_shows_nothing_and_never_fails() {
    let ctx = Context::default();
    // A `max_width` of 0 doesn't wrap, so 1000 characters are far over 16384 pixels.
    let mut source = Registry::builtin()
        .create("chat_message", &settings(&[("max_width", json!(0))]), &ctx)
        .expect("a source");
    ctx.chat.set(Some(message(&"m".repeat(1000))));
    assert_eq!(size(&shown(&mut *source, ms(0)).expect("a frame")), (1, 1));
    // Not retried on every frame: the same blank comes back.
    let blank = shown(&mut *source, ms(100)).expect("a frame");
    assert_eq!(size(&blank), (1, 1));

    ctx.chat.set(Some(message("short")));
    assert!(size(&shown(&mut *source, ms(200)).expect("a frame")).0 > 1);
}

#[test]
fn a_message_too_large_to_draw_never_fails_creating_or_updating() {
    let ctx = Context::default();
    ctx.chat.set(Some(message(&"m".repeat(1000))));
    // The engine rebuilds a source with `create` on every settings change.
    let wide = settings(&[("max_width", json!(0))]);
    let mut source = Registry::builtin()
        .create("chat_message", &wide, &ctx)
        .expect("created with an undrawable message featured");
    assert_eq!(size(&shown(&mut *source, ms(0)).expect("a frame")), (1, 1));
    source
        .update(&wide)
        .expect("updated with an undrawable message featured");
    assert_eq!(
        size(&shown(&mut *source, ms(100)).expect("a frame")),
        (1, 1)
    );
    // Bad settings still fail, whatever is featured.
    assert!(source.update(&settings(&[("size", json!(0))])).is_err());

    ctx.chat.set(Some(message("short")));
    assert!(size(&shown(&mut *source, ms(200)).expect("a frame")).0 > 1);
}

/// `frame` never draws: a newly featured message waits for the painter, so a host that
/// asks for frames under a lock never draws under it (review B4 on #363).
#[test]
fn frame_hands_out_the_painted_frame_and_never_draws() {
    let ctx = Context::default();
    let mut source = Registry::builtin()
        .create("chat_message", &settings(&[]), &ctx)
        .expect("a source");
    let blank = source.frame(ms(0)).expect("a frame");
    ctx.chat.set(Some(message("hello")));
    let unpainted = source.frame(ms(100)).expect("a frame");
    assert!(Arc::ptr_eq(&blank, &unpainted));
    let painted = shown(&mut *source, ms(100)).expect("a frame");
    assert!(size(&painted).0 > 1);
}
