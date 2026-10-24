//! The `browser` kind against `jstream-browser`'s in-memory backend: the page each
//! setting opens, the settings it refuses, custom CSS as a live setting, closing, and
//! the page's audio as the source's own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jstream_browser::fake::FakeBrowser;
use jstream_browser::{BrowserBackend, BrowserError, DEFAULT_CSS, PageLocation};
use jstream_media::{MediaTime, Timebase};
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
            ("width", false),
            ("height", false),
            ("fps", false),
            ("css", true),
            ("local_network", false)
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
