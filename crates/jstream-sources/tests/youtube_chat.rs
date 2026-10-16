//! The `youtube_chat` preset against `jstream-browser`'s in-memory backend: the video
//! setting becomes the popout chat page, and the rest behaves as a `browser` source.

use std::sync::Arc;

use jstream_browser::fake::FakeBrowser;
use jstream_browser::{BrowserBackend, PageLocation};
use jstream_protocol::Settings;
use jstream_sources::{Context, Registry, SourceError, YOUTUBE_CHAT_CSS, needs_restart};
use serde_json::{Value, json};

fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("youtube_chat").expect("the chat kind");
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

fn ctx(browser: &Arc<FakeBrowser>) -> Context {
    Context {
        browser: Some(Arc::clone(browser) as Arc<dyn BrowserBackend>),
        ..Context::default()
    }
}

#[test]
fn the_kind_is_a_browser_preset_with_live_css() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("youtube_chat").expect("registered");
    assert_eq!(kind.name, "YouTube chat");
    let ids: Vec<(&str, bool)> = kind
        .params
        .iter()
        .map(|p| (p.id.as_str(), p.live))
        .collect();
    assert_eq!(
        ids,
        [
            ("video", false),
            ("width", false),
            ("height", false),
            ("fps", false),
            ("css", true)
        ]
    );
    let restyled = settings(&[("css", json!("body { color: red; }"))]);
    assert!(!needs_restart(kind, &settings(&[]), &restyled));
}

#[test]
fn a_video_link_opens_its_popout_chat_page() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser);
    let source = Registry::builtin()
        .create(
            "youtube_chat",
            &settings(&[(
                "video",
                json!("https://www.youtube.com/live/dQw4w9WgXcQ?si=x"),
            )]),
            &ctx,
        )
        .expect("a page");
    let pages = browser.pages();
    let spec = pages[0].spec();
    assert_eq!(
        spec.location,
        PageLocation::Web("https://www.youtube.com/live_chat?is_popout=1&v=dQw4w9WgXcQ".into())
    );
    assert_eq!((spec.size.width(), spec.size.height()), (400, 600));
    assert_eq!(spec.css, YOUTUBE_CHAT_CSS);
    drop(source);
    assert!(pages[0].is_closed());
}

#[test]
fn no_video_is_a_blank_page_and_a_bad_one_opens_nothing() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser);
    let _blank = Registry::builtin()
        .create("youtube_chat", &settings(&[]), &ctx)
        .expect("a blank page");
    assert_eq!(browser.pages()[0].spec().location, PageLocation::Blank);

    for bad in ["https://example.com/watch?v=abc", "id&autoplay=1"] {
        let err = Registry::builtin()
            .create("youtube_chat", &settings(&[("video", json!(bad))]), &ctx)
            .err()
            .expect("refused");
        assert!(
            matches!(&err, SourceError::Setting { param, .. } if param == "video"),
            "{bad}: {err}"
        );
    }
    assert_eq!(browser.pages().len(), 1, "nothing opens for a bad video");
}
