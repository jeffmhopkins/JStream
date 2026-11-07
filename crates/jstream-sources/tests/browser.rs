//! The `browser` kind against `jstream-browser`'s in-memory backend: the page each
//! setting opens, the settings it refuses, custom CSS as a live setting, closing, and
//! the page's audio as the source's own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jstream_browser::fake::FakeBrowser;
use jstream_browser::{BrowserBackend, BrowserError, DEFAULT_CSS, PageLocation};
use jstream_media::{
    ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{AudioInputHandler, AudioPacket, PlatformError};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Registry, Source, SourceError};
use serde_json::{Value, json};

/// The settings the graph would store: the kind's defaults, with `patch` over them.
fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("browser").expect("the browser kind");
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

fn ctx(browser: &Arc<FakeBrowser>, collection: Option<&Path>) -> Context {
    Context {
        assets: AssetRoots {
            collection: collection.map(Path::to_path_buf),
            app: None,
        },
        browser: Some(Arc::clone(browser) as Arc<dyn BrowserBackend>),
        ..Context::default()
    }
}

fn create(ctx: &Context, settings: &Settings) -> Result<Box<dyn Source>, SourceError> {
    Registry::builtin().create("browser", settings, ctx)
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jstream-sources-browser-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    dir
}

#[test]
fn the_kind_has_video_and_audio_and_only_css_is_live() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("browser").expect("the browser kind");
    assert!(kind.video && kind.audio);
    let ids: Vec<(&str, bool)> = kind
        .params
        .iter()
        .map(|p| (p.id.as_str(), p.live))
        .collect();
    assert_eq!(
        ids,
        [
            ("url", false),
            ("file", false),
            ("html", false),
            ("width", false),
            ("height", false),
            ("fps", false),
            ("css", true),
            ("restart_on_show", true),
            ("local_network", false),
            ("page_api", false),
            ("page_api_scenes", false),
            ("page_api_values", false),
            ("page_api_bus", false),
        ]
    );
}

#[test]
fn a_url_opens_a_web_page_of_the_set_size_and_rate() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let mut source = create(
        &ctx,
        &settings(&[
            ("url", json!(" https://example.com/alerts ")),
            ("width", json!(640)),
            ("height", json!(360)),
            ("fps", json!(60)),
        ]),
    )
    .expect("a page");
    let pages = browser.pages();
    assert_eq!(pages.len(), 1);
    let spec = pages[0].spec();
    assert_eq!(
        spec.location,
        PageLocation::Web("https://example.com/alerts".into())
    );
    assert_eq!((spec.size.width(), spec.size.height()), (640, 360));
    assert_eq!(spec.fps, 60);
    assert_eq!(spec.css, DEFAULT_CSS);
    // #347: off by default, so the page from the internet can't reach the local network.
    assert!(!spec.local_network);
    assert!(!pages[0].request("http://127.0.0.1:8080/"));
    assert!(pages[0].request("https://example.com/font.woff2"));
    // The CPU frame is a transparent stand-in of the page's size; the fake never paints.
    let frame = source
        .frame(MediaTime::zero(Timebase::MILLIS))
        .expect("a frame");
    assert_eq!(
        (frame.info().size.width(), frame.info().size.height()),
        (640, 360)
    );
    assert!(frame.planes()[0].data().iter().all(|&b| b == 0));
    assert_eq!(source.next_change(MediaTime::zero(Timebase::MILLIS)), None);
    assert!(source.gpu_frame().is_none());
    assert!(!pages[0].is_closed());
    drop(source);
    assert!(pages[0].is_closed());
}

/// ADR-0095: a page whose backend paints on the CPU (CEF on Linux) shows its latest
/// paint as the source's frame, and is looked at again every frame of its rate.
#[test]
fn a_page_that_paints_on_the_cpu_shows_its_paint_each_frame() {
    let browser = FakeBrowser::new();
    browser.paint_on_cpu(true);
    let ctx = ctx(&browser, None);
    let mut source = create(
        &ctx,
        &settings(&[
            ("width", json!(2)),
            ("height", json!(1)),
            ("fps", json!(30)),
        ]),
    )
    .expect("a page");
    let at = MediaTime::new(1000, Timebase::MILLIS);
    assert_eq!(
        source.next_change(at),
        Some(MediaTime::new(1034, Timebase::MILLIS)),
        "one frame at 30 fps, rounded up"
    );
    let before = source.frame(at).expect("a frame");
    assert!(
        before.planes()[0].data().iter().all(|&b| b == 0),
        "transparent before the first paint"
    );
    let painted = VideoFrame::new(
        VideoFrameInfo {
            size: FrameSize::new(2, 1).expect("a size"),
            format: PixelFormat::Rgba8,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::MILLIS),
        },
        vec![Plane::new(vec![255, 0, 0, 255, 0, 0, 255, 128], 8)],
    )
    .expect("a frame");
    let painter = source.painter().expect("a page on the CPU has a painter");
    browser.pages()[0].paint(Arc::new(painted));
    let after = source.frame(at).expect("a frame");
    assert_eq!(after.planes()[0].data(), &[255, 0, 0, 255, 0, 0, 255, 128]);
    assert!(
        Arc::ptr_eq(&painter.frame(), &after),
        "the painter hands it out too"
    );
    assert!(source.gpu_frame().is_none());
}

/// A page given as HTML opens as that HTML, ahead of the URL, behind a local file, and
/// with no local network unless the source allows it.
#[test]
fn html_opens_a_page_built_from_the_text() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let html = "<div class=\"bar\">BREAKING NEWS</div>";
    let _source = create(
        &ctx,
        &settings(&[
            ("url", json!("https://example.com/ignored")),
            ("html", json!(html)),
        ]),
    )
    .expect("a page");
    let page = &browser.pages()[0];
    assert_eq!(page.spec().location, PageLocation::Html(html.into()));
    assert!(!page.request("http://127.0.0.1:4455/"));
    assert!(page.request("https://fonts.gstatic.com/s/inter.woff2"));

    // Blank HTML falls back to the URL.
    let _source = create(
        &ctx,
        &settings(&[
            ("url", json!("https://example.com/alerts")),
            ("html", json!("  \n")),
        ]),
    )
    .expect("a page");
    assert_eq!(
        browser.pages()[1].spec().location,
        PageLocation::Web("https://example.com/alerts".into())
    );

    // Too long is refused, naming the setting.
    let long = "a".repeat(jstream_browser::MAX_HTML_BYTES + 1);
    match create(&ctx, &settings(&[("html", json!(long))])) {
        Err(SourceError::Setting { param, .. }) => assert_eq!(param, "html"),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("over-long HTML opened a page"),
    }

    // Allowed, it may reach a local app.
    let _source = create(
        &ctx,
        &settings(&[("html", json!(html)), ("local_network", json!(true))]),
    )
    .expect("a page");
    assert!(
        browser
            .pages()
            .last()
            .expect("a page")
            .request("ws://127.0.0.1:8080/")
    );
}

/// With `restart_on_show`, the page loads again each time the program shows it again,
/// so an HTML graphic's entrance plays every time; without it, it keeps running.
#[test]
fn restart_on_show_reloads_the_page_when_shown() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let at = MediaTime::zero(Timebase::MILLIS);
    let mut kept = create(&ctx, &settings(&[("html", json!("<p>a</p>"))])).expect("a page");
    let mut restarted = create(
        &ctx,
        &settings(&[
            ("html", json!("<p>b</p>")),
            ("restart_on_show", json!(true)),
        ]),
    )
    .expect("a page");
    kept.show(at);
    restarted.show(at);
    restarted.show(at);
    let pages = browser.pages();
    assert_eq!(pages[0].reloads(), 0);
    assert_eq!(pages[1].reloads(), 2);
    // Live: turning it off stops the reloads without reopening the page.
    restarted
        .update(&settings(&[("html", json!("<p>b</p>"))]))
        .expect("a live change");
    restarted.show(at);
    assert_eq!(browser.pages().len(), 2);
    assert_eq!(pages[1].reloads(), 2);
}

/// `source.restart` (ADR-0115) loads the page again whatever `restart_on_show`.
#[test]
fn a_restart_reloads_the_page() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let mut source = create(&ctx, &settings(&[("html", json!("<p>a</p>"))])).expect("a page");
    source.restart(MediaTime::zero(Timebase::MILLIS));
    assert_eq!(browser.pages()[0].reloads(), 1);
}

/// #347: the source's opt-in lets a page from the internet reach a local app.
#[test]
fn local_network_opts_a_page_in() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let _source = create(
        &ctx,
        &settings(&[
            ("url", json!("https://widgets.example.com/overlay")),
            ("local_network", json!(true)),
        ]),
    )
    .expect("a page");
    let page = &browser.pages()[0];
    assert!(page.spec().local_network);
    assert!(page.request("ws://127.0.0.1:8080/"));
    assert!(page.request("http://192.168.1.10/"));
}

/// ADR-0102: off by default; on, the page opens with its grant, and the engine's view
/// reaches it filtered to that grant, sending only what changed.
#[test]
fn the_page_api_is_opt_in_and_sends_only_what_the_grant_covers() {
    use jstream_browser::PageApi;
    use jstream_media::{MediaTime, Timebase};
    use jstream_sources::PageView;
    use std::collections::BTreeMap;

    fn view(values: &BTreeMap<String, serde_json::Value>, ms: i64) -> PageView<'_> {
        PageView {
            now: MediaTime::new(ms, Timebase::MILLIS),
            program: true,
            preview: false,
            program_scene: Some(("scn_1", "Main")),
            preview_scene: None,
            values,
            buses: &NO_BUSES,
        }
    }
    static NO_BUSES: BTreeMap<jstream_protocol::BusId, jstream_sources::BusLevels> =
        BTreeMap::new();

    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let mut plain =
        create(&ctx, &settings(&[("url", json!("https://example.com/"))])).expect("a page");
    assert!(!plain.page_api());
    assert_eq!(browser.pages()[0].spec().page_api, None);
    let mut values = BTreeMap::new();
    values.insert("status.text".to_owned(), json!("On air"));
    values.insert("other".to_owned(), json!(1));
    plain.page_view(&view(&values, 0));
    plain.page_clock(MediaTime::new(0, Timebase::MILLIS));
    assert!(browser.pages()[0].take_data().is_empty());

    let mut granted = create(
        &ctx,
        &settings(&[
            ("url", json!("https://example.com/")),
            ("page_api", json!(true)),
            ("page_api_values", json!("status.*")),
        ]),
    )
    .expect("a granted page");
    assert!(granted.page_api());
    let page = &browser.pages()[1];
    assert_eq!(
        page.spec().page_api,
        Some(PageApi {
            scenes: false,
            values: true,
            levels: false,
        })
    );
    granted.page_view(&view(&values, 1000));
    let sent = page.take_data();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].clock.map(|c| c.0), Some(1.0));
    assert!(
        sent[0]
            .source
            .as_deref()
            .unwrap()
            .contains(r#""shown":true"#)
    );
    assert_eq!(sent[0].scenes, None, "scenes weren't granted");
    assert_eq!(
        sent[0].values,
        [("status.text".to_owned(), Some(r#""On air""#.to_owned()))]
    );
    // The same view again sends nothing; a look a quarter second on sends an anchor.
    granted.page_view(&view(&values, 1100));
    assert!(page.take_data().is_empty());
    granted.page_clock(MediaTime::new(1250, Timebase::MILLIS));
    let anchors = page.take_data();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].clock.map(|c| c.0), Some(1.25));

    let bad = create(
        &ctx,
        &settings(&[
            ("url", json!("https://example.com/")),
            ("page_api", json!(true)),
            ("page_api_values", json!("Not a name")),
        ]),
    );
    assert!(matches!(bad, Err(SourceError::Setting { param, .. }) if param == "page_api_values"));
}

#[test]
fn no_url_is_a_blank_page_and_a_file_wins_over_a_url() {
    let browser = FakeBrowser::new();
    let dir = folder("file");
    std::fs::write(dir.join("overlay.html"), "<p>hi</p>").expect("a page");
    let ctx = ctx(&browser, Some(&dir));
    let _blank = create(&ctx, &settings(&[])).expect("a blank page");
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "overlay.html".into(),
    }
    .to_value();
    let _file = create(
        &ctx,
        &settings(&[("url", json!("https://example.com")), ("file", file)]),
    )
    .expect("a file page");
    let pages = browser.pages();
    assert_eq!(pages[0].spec().location, PageLocation::Blank);
    let expected = dir.join("overlay.html").canonicalize().expect("the file");
    assert_eq!(pages[1].spec().location, PageLocation::File(expected));
}

#[test]
fn bad_settings_and_a_missing_backend_are_errors() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let param_of = |result: Result<Box<dyn Source>, SourceError>| match result {
        Err(SourceError::Setting { param, .. }) => param,
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("created"),
    };
    assert_eq!(
        param_of(create(
            &ctx,
            &settings(&[("url", json!("javascript:alert(1)"))])
        )),
        "url"
    );
    assert_eq!(
        param_of(create(
            &ctx,
            &settings(&[("url", json!("file:///etc/passwd"))])
        )),
        "url"
    );
    assert_eq!(
        param_of(create(&ctx, &settings(&[("fps", json!(0))]))),
        "fps"
    );
    assert_eq!(
        param_of(create(&ctx, &settings(&[("fps", json!(61))]))),
        "fps"
    );
    let long = "a".repeat(jstream_browser::MAX_CSS_BYTES + 1);
    assert_eq!(
        param_of(create(&ctx, &settings(&[("css", json!(long))]))),
        "css"
    );
    // A file outside any configured root can't be resolved.
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "overlay.html".into(),
    }
    .to_value();
    assert!(matches!(
        create(&ctx, &settings(&[("file", file)])),
        Err(SourceError::NoAssetRoot { .. })
    ));
    assert!(browser.pages().is_empty(), "nothing opens for bad settings");
    assert!(matches!(
        create(&Context::default(), &settings(&[])),
        Err(SourceError::NoBackend { backend: "browser" })
    ));
    browser.refuse(Some(BrowserError::Start {
        reason: "no runtime".into(),
    }));
    assert!(matches!(
        create(&ctx, &settings(&[])),
        Err(SourceError::Browser(BrowserError::Start { .. }))
    ));
}

#[test]
fn css_is_live_and_other_settings_are_not() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let first = settings(&[]);
    let mut source = create(&ctx, &first).expect("a page");
    let page = &browser.pages()[0];
    // The same CSS again changes nothing.
    source.update(&first).expect("no change");
    assert!(page.css_updates().is_empty());
    let red = settings(&[("css", json!("body { background: red; }"))]);
    source.update(&red).expect("a live change");
    assert_eq!(page.css_updates(), ["body { background: red; }"]);
    assert!(matches!(
        source.update(&settings(&[("css", json!("x")), ("width", json!(100))])),
        Err(SourceError::NotLive { param }) if param == "width"
    ));
    assert_eq!(page.css_updates().len(), 1);
}

/// Records the packets it is handed.
struct Recorder(Arc<Mutex<Vec<Vec<f32>>>>);

impl AudioInputHandler for Recorder {
    fn on_packet(&mut self, packet: &AudioPacket<'_>) {
        self.0.lock().expect("lock").push(packet.samples.to_vec());
    }

    fn on_error(&mut self, _: &PlatformError) {}
}

#[test]
fn the_pages_audio_is_the_sources_own() {
    let browser = FakeBrowser::new();
    let ctx = ctx(&browser, None);
    let source = create(&ctx, &settings(&[])).expect("a page");
    let audio = source.audio().expect("a browser source has audio");
    assert_eq!(audio.format(), jstream_browser::PageAudio::format());
    let heard = Arc::new(Mutex::new(Vec::new()));
    let stream = audio.open(Box::new(Recorder(Arc::clone(&heard))));
    assert_eq!(stream.format(), audio.format());
    let page = &browser.pages()[0];
    page.audio()
        .push_planar(&[&[0.5, 0.25], &[-0.5, -0.25]], 2, None);
    assert_eq!(*heard.lock().expect("lock"), [vec![0.5, -0.5, 0.25, -0.25]]);
    drop(stream);
    page.audio().push_planar(&[&[0.5], &[0.5]], 1, None);
    assert_eq!(heard.lock().expect("lock").len(), 1);
}
