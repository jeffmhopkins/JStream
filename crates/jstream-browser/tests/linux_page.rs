//! The Linux browser helper end to end (ADR-0095): CEF really starts in the sandboxed
//! helper, paints a page in software, and its frames reach the engine as CPU frames.
//! Ignored by default, as it needs the helper laid out by `cargo xtask browser-linux`,
//! named by `JSTREAM_BROWSER_HELPER`, and a user other than root for Chromium's sandbox:
//!
//! ```text
//! cargo xtask browser-linux --out target/browser-linux
//! JSTREAM_BROWSER_HELPER=target/browser-linux/browser/jstream-browser-helper \
//!   cargo test -p jstream-browser --features cef --test linux_page -- --ignored
//! ```
//!
//! CI runs it on every Linux build of the helper.
//!
//! A second test serves the page over `http` from `127.0.0.1` (#1069): CEF on Linux names
//! `null` as the initiator of a web source's own first load, which the request policy
//! must still count as the source's own.

#![cfg(all(target_os = "linux", feature = "cef"))]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jstream_browser::{BrowserBackend, CefBackend, CefConfig, DEFAULT_CSS, PageLocation, PageSpec};
use jstream_media::{FrameSize, PixelFormat, Timebase};
use jstream_platform::fake::FakeClock;

/// How long the page may take to paint: a first start makes CEF's cache.
const PAINT_TIMEOUT: Duration = Duration::from_secs(60);

/// Left half opaque red, right half the page's own (transparent) background.
const PAGE: &str = r#"<!doctype html>
<html><body>
<div style="position:fixed;left:0;top:0;width:50%;height:100%;background:#ff0000"></div>
</body></html>"#;

#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_page_paints_in_the_sandboxed_helper_and_reaches_the_engine_on_the_cpu() {
    let dir = temp_dir("file");
    let page_file = dir.join("index.html");
    std::fs::write(&page_file, PAGE).expect("the page");
    paints_red(&dir, PageLocation::File(page_file));
    let _ = std::fs::remove_dir_all(&dir);
}

/// #1069: a page served from this computer loads in a source whose location is local,
/// through the redirect its server answers the location with.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_page_served_from_this_computer_loads() {
    let dir = temp_dir("http");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().expect("its address").port();
    // Answers `/` with a redirect to `/index.html`, as dev servers do, and every other
    // request with the page, until the test ends.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("the stream"));
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let root = line.starts_with("GET / ");
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let _ = if root {
                write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: /index.html\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                )
            } else {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                )
            };
        }
    });
    // The source's own first load, then its redirect (#1094's review, B3).
    paints_red(&dir, PageLocation::Web(format!("http://127.0.0.1:{port}/")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh folder for one test's page and CEF's files.
fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("jstream-linux-page-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    dir
}

/// Opens [`PAGE`] from `location` through the helper and checks its paints reach the
/// engine as CPU frames, its left half red and its right half transparent.
fn paints_red(dir: &std::path::Path, location: PageLocation) {
    let helper = PathBuf::from(
        std::env::var_os("JSTREAM_BROWSER_HELPER").expect("JSTREAM_BROWSER_HELPER is set"),
    );
    let backend = CefBackend::new(
        CefConfig {
            helper,
            cache_root: dir.join("cef"),
            log_file: Some(dir.join("cef.log")),
        },
        Arc::new(FakeClock::new(Timebase::MILLIS)),
    );
    backend.use_device(None);
    let (width, height) = (320, 180);
    let page = match backend.open(&PageSpec {
        location,
        size: FrameSize::new(width, height).expect("a size"),
        fps: 30,
        css: DEFAULT_CSS.into(),
        local_network: false,
        page_api: None,
    }) {
        Ok(page) => page,
        Err(error) => {
            let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
            panic!("the page didn't open: {error}\nCEF's log:\n{log}");
        }
    };
    assert!(page.paints_on_cpu());
    assert!(page.frame().is_none(), "no GPU frames on Linux");
    let deadline = Instant::now() + PAINT_TIMEOUT;
    let pixel = |frame: &jstream_media::VideoFrame, x: u32, y: u32| -> [u8; 4] {
        let plane = &frame.planes()[0];
        let at = y as usize * plane.stride() + x as usize * 4;
        let p = &plane.data()[at..at + 4];
        [p[0], p[1], p[2], p[3]]
    };
    let painted = loop {
        if let Some(frame) = page.cpu_frame()
            && pixel(&frame, width / 4, height / 2) == [255, 0, 0, 255]
        {
            break frame;
        }
        if Instant::now() >= deadline {
            let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
            panic!(
                "no red paint within {PAINT_TIMEOUT:?} (last frame: {:?}, {} request(s) \
                 refused)\nCEF's log:\n{log}",
                page.cpu_frame()
                    .map(|frame| pixel(&frame, width / 4, height / 2)),
                page.denied_requests()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let info = painted.info();
    assert_eq!(info.format, PixelFormat::Rgba8);
    assert_eq!((info.size.width(), info.size.height()), (width, height));
    assert_eq!(
        pixel(&painted, width * 3 / 4, height / 2)[3],
        0,
        "the default CSS makes the page's background transparent"
    );
    drop(page);
    // Dropping the backend shuts the helper down and waits for it.
    let stopping = Instant::now();
    drop(backend);
    assert!(stopping.elapsed() < Duration::from_secs(20));
}

/// Changes colour on every animation frame, so it paints at its frame rate.
const ANIMATED: &str = r#"<!doctype html>
<html><body><script>
let n = 0;
function step() {
  n += 1;
  document.body.style.background = n % 2 ? '#ff0000' : '#0000ff';
  requestAnimationFrame(step);
}
requestAnimationFrame(step);
</script></body></html>"#;

/// #1071: a hidden page stops painting, and paints again once shown.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_hidden_page_stops_painting_until_shown() {
    let helper = PathBuf::from(
        std::env::var_os("JSTREAM_BROWSER_HELPER").expect("JSTREAM_BROWSER_HELPER is set"),
    );
    let dir = std::env::temp_dir().join(format!("jstream-linux-hidden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    let page_file = dir.join("index.html");
    std::fs::write(&page_file, ANIMATED).expect("the page");
    let backend = CefBackend::new(
        CefConfig {
            helper,
            cache_root: dir.join("cef"),
            log_file: Some(dir.join("cef.log")),
        },
        Arc::new(FakeClock::new(Timebase::MILLIS)),
    );
    backend.use_device(None);
    let mut page = backend
        .open(&PageSpec {
            location: PageLocation::File(page_file),
            size: FrameSize::new(64, 36).expect("a size"),
            fps: 30,
            css: DEFAULT_CSS.into(),
            local_network: false,
            page_api: None,
        })
        .expect("the page opened");
    // Paints arrive while shown.
    let paints = |page: &dyn jstream_browser::BrowserPage, within: Duration| -> usize {
        let deadline = Instant::now() + within;
        let mut last = page.cpu_frame();
        let mut count = 0;
        while Instant::now() < deadline {
            let now = page.cpu_frame();
            let changed = match (&last, &now) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                (None, Some(_)) => true,
                _ => false,
            };
            if changed {
                count += 1;
                last = now;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        count
    };
    let deadline = Instant::now() + PAINT_TIMEOUT;
    while page.cpu_frame().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        paints(&*page, Duration::from_secs(1)) >= 5,
        "it paints while shown"
    );

    page.set_hidden(true);
    // Let a paint already under way land.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        paints(&*page, Duration::from_secs(1)),
        0,
        "it rests while hidden"
    );

    page.set_hidden(false);
    assert!(
        paints(&*page, Duration::from_secs(2)) >= 5,
        "it paints again once shown"
    );
    drop(page);
    drop(backend);
    let _ = std::fs::remove_dir_all(&dir);
}

/// #1151: a reloaded static page shows again. The helper holds a reloading page's paints
/// until its load ends, and asks CEF for a paint when the hold ends, so a page whose
/// styled paints all came before that still reaches the engine; never as the white
/// Chromium can paint a new document with first.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_reloaded_static_page_paints_again() {
    let helper = PathBuf::from(
        std::env::var_os("JSTREAM_BROWSER_HELPER").expect("JSTREAM_BROWSER_HELPER is set"),
    );
    let dir = std::env::temp_dir().join(format!("jstream-linux-reload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    let page_file = dir.join("index.html");
    std::fs::write(&page_file, PAGE).expect("the page");
    let backend = CefBackend::new(
        CefConfig {
            helper,
            cache_root: dir.join("cef"),
            log_file: Some(dir.join("cef.log")),
        },
        Arc::new(FakeClock::new(Timebase::MILLIS)),
    );
    backend.use_device(None);
    let (width, height) = (64, 36);
    let mut page = backend
        .open(&PageSpec {
            location: PageLocation::File(page_file),
            size: FrameSize::new(width, height).expect("a size"),
            fps: 30,
            css: DEFAULT_CSS.into(),
            local_network: false,
            page_api: None,
        })
        .expect("the page opened");
    let pixel = |frame: &jstream_media::VideoFrame, x: u32, y: u32| -> [u8; 4] {
        let plane = &frame.planes()[0];
        let at = y as usize * plane.stride() + x as usize * 4;
        let p = &plane.data()[at..at + 4];
        [p[0], p[1], p[2], p[3]]
    };
    let red = |frame: &jstream_media::VideoFrame| {
        pixel(frame, width / 4, height / 2) == [255, 0, 0, 255]
            && pixel(frame, width * 3 / 4, height / 2)[3] == 0
    };
    let wait_red = |page: &dyn jstream_browser::BrowserPage,
                    not: Option<&Arc<jstream_media::VideoFrame>>|
     -> Arc<jstream_media::VideoFrame> {
        let deadline = Instant::now() + PAINT_TIMEOUT;
        loop {
            if let Some(frame) = page.cpu_frame() {
                let fresh = not.is_none_or(|old| !Arc::ptr_eq(old, &frame));
                assert!(
                    !fresh || pixel(&frame, width * 3 / 4, height / 2) != [255, 255, 255, 255],
                    "a white frame reached the engine"
                );
                if fresh && red(&frame) {
                    return frame;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no red paint within {PAINT_TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // Past the hold's end and the repaint it may ask for, no white frame follows either.
    let settle = |page: &dyn jstream_browser::BrowserPage| {
        let until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < until {
            if let Some(frame) = page.cpu_frame() {
                assert!(red(&frame), "the page stays as it painted");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let mut last = wait_red(&*page, None);
    for _ in 0..5 {
        page.reload();
        last = wait_red(&*page, Some(&last));
        settle(&*page);
    }
    drop(page);
    drop(backend);
    let _ = std::fs::remove_dir_all(&dir);
}

/// #1155: a new page's first load is held like a reload's, so the white Chromium can
/// paint a new document with first never reaches the engine. Opens the page many times.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_new_page_never_shows_white() {
    let helper = PathBuf::from(
        std::env::var_os("JSTREAM_BROWSER_HELPER").expect("JSTREAM_BROWSER_HELPER is set"),
    );
    let dir = temp_dir("open");
    let page_file = dir.join("index.html");
    std::fs::write(&page_file, PAGE).expect("the page");
    let backend = CefBackend::new(
        CefConfig {
            helper,
            cache_root: dir.join("cef"),
            log_file: Some(dir.join("cef.log")),
        },
        Arc::new(FakeClock::new(Timebase::MILLIS)),
    );
    backend.use_device(None);
    let (width, height) = (64, 36);
    let pixel = |frame: &jstream_media::VideoFrame, x: u32, y: u32| -> [u8; 4] {
        let plane = &frame.planes()[0];
        let at = y as usize * plane.stride() + x as usize * 4;
        let p = &plane.data()[at..at + 4];
        [p[0], p[1], p[2], p[3]]
    };
    for _ in 0..10 {
        let page = backend
            .open(&PageSpec {
                location: PageLocation::File(page_file.clone()),
                size: FrameSize::new(width, height).expect("a size"),
                fps: 30,
                css: DEFAULT_CSS.into(),
                local_network: false,
                page_api: None,
            })
            .expect("the page opened");
        // Every frame up to the first red one, and for a while after it, is never white.
        let deadline = Instant::now() + PAINT_TIMEOUT;
        let mut red_since = None;
        loop {
            if let Some(frame) = page.cpu_frame() {
                assert_ne!(
                    pixel(&frame, width * 3 / 4, height / 2),
                    [255, 255, 255, 255],
                    "a white frame reached the engine"
                );
                if red_since.is_none() && pixel(&frame, width / 4, height / 2) == [255, 0, 0, 255] {
                    red_since = Some(Instant::now());
                }
            }
            if red_since.is_some_and(|at| at.elapsed() > Duration::from_millis(400)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no red paint within {PAINT_TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(page);
    }
    drop(backend);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Paints the page API's state: blue while `window.jstream` is there with no data, green
/// once the update below has arrived, red without the API (and magenta if the API has a
/// member the grant left out).
const API_PAGE: &str = r##"<!doctype html>
<html><body><div id="d" style="position:fixed;inset:0;background:#ff0000"></div>
<script>
function paint() {
  var j = window.jstream, color = "#ff0000";
  if (j && j.version === 1) {
    color = "#0000ff";
    if ("levels" in j) {
      color = "#ff00ff";
    } else if (j.source.shown && j.source.shownAt === 2.5 && j.scenes.program
        && j.scenes.program.name === "Main" && j.values.get("status.ok") === true
        && j.clock.now() > 99) {
      color = "#00ff00";
    }
  }
  document.getElementById("d").style.background = color;
  requestAnimationFrame(paint);
}
paint();
</script></body></html>"##;

#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_granted_page_reads_the_engine_through_window_jstream() {
    use jstream_browser::{PageApi, PageData};

    let helper = PathBuf::from(
        std::env::var_os("JSTREAM_BROWSER_HELPER").expect("JSTREAM_BROWSER_HELPER is set"),
    );
    let dir = std::env::temp_dir().join(format!("jstream-linux-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    let backend = CefBackend::new(
        CefConfig {
            helper,
            cache_root: dir.join("cef"),
            log_file: Some(dir.join("cef.log")),
        },
        Arc::new(FakeClock::new(Timebase::MILLIS)),
    );
    backend.use_device(None);
    let (width, height) = (64, 36);
    let open = |page_api| {
        backend
            .open(&PageSpec {
                location: PageLocation::Html(API_PAGE.into()),
                size: FrameSize::new(width, height).expect("a size"),
                fps: 30,
                css: DEFAULT_CSS.into(),
                local_network: false,
                page_api,
            })
            .unwrap_or_else(|error| {
                let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
                panic!("the page didn't open: {error}\nCEF's log:\n{log}");
            })
    };
    let wait_for = |page: &dyn jstream_browser::BrowserPage, rgb: [u8; 3]| {
        let deadline = Instant::now() + PAINT_TIMEOUT;
        loop {
            let seen = page.cpu_frame().map(|frame| {
                let plane = &frame.planes()[0];
                let at = (height / 2) as usize * plane.stride() + (width / 2) as usize * 4;
                let p = &plane.data()[at..at + 4];
                [p[0], p[1], p[2]]
            });
            if seen == Some(rgb) {
                return;
            }
            if Instant::now() >= deadline {
                let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
                panic!(
                    "no {rgb:?} paint within {PAINT_TIMEOUT:?} (last: {seen:?})\nCEF's log:\n{log}"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // Without a grant the page has no API.
    let plain = open(None);
    wait_for(&*plain, [255, 0, 0]);
    drop(plain);

    let mut page = open(Some(PageApi {
        scenes: true,
        values: true,
        levels: false,
    }));
    wait_for(&*page, [0, 0, 255]);
    page.send_data(PageData {
        clock: Some((100.0, 0.0)),
        source: Some(
            r#"{"shown":true,"previewShown":false,"shownAt":2.5,"previewShownAt":null}"#.into(),
        ),
        scenes: Some(r#"{"program":{"id":"scn_main","name":"Main"},"preview":null}"#.into()),
        levels: None,
        values: vec![("status.ok".into(), Some("true".into()))],
    });
    wait_for(&*page, [0, 255, 0]);
    // A reload starts a new document, which gets the page's state again.
    page.reload();
    wait_for(&*page, [0, 255, 0]);
    // And it keeps getting updates after it: the old document's context, released after
    // the new one's was made, doesn't take the new one's feed with it (#1148).
    let value = |ok: &str| PageData {
        values: vec![("status.ok".into(), Some(ok.into()))],
        ..PageData::default()
    };
    page.send_data(value("false"));
    wait_for(&*page, [0, 0, 255]);
    page.send_data(value("true"));
    wait_for(&*page, [0, 255, 0]);
    drop(page);

    // A page granted a bus reads each reading, and `null` once there is none.
    let mut levels = open_levels(&backend, width, height, &dir);
    wait_for(&*levels, [0, 0, 255]);
    let reading = |text: &str| PageData {
        levels: Some(text.into()),
        ..PageData::default()
    };
    let bands = vec!["0.5"; 64].join(",");
    levels.send_data(reading(&format!(
        r#"{{"peak":[0.5,0.25],"rms":[0.1,0.1],"bands":[{bands}]}}"#
    )));
    wait_for(&*levels, [255, 255, 0]);
    levels.send_data(reading("null"));
    wait_for(&*levels, [0, 255, 255]);
    drop(levels);
    drop(backend);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A page that is blue with `jstream.levels`, yellow once a `levels` event brought the
/// reading `levels()` returns, and cyan once that is `null`.
const LEVELS_PAGE: &str = r##"<!doctype html>
<html><body><div id="d" style="position:fixed;inset:0;background:#ff0000"></div>
<script>
if (window.jstream && typeof window.jstream.levels === "function") {
  document.getElementById("d").style.background = "#0000ff";
  window.jstream.addEventListener("levels", function (reading) {
    var color = "#ff00ff";
    var now = window.jstream.levels();
    if (reading === null && now === null) {
      color = "#00ffff";
    } else if (reading === now && reading.peak[0] === 0.5 && reading.rms[1] === 0.1
        && reading.bands.length === 64 && Object.isFrozen(reading.bands)) {
      color = "#ffff00";
    }
    document.getElementById("d").style.background = color;
  });
}
</script></body></html>"##;

fn open_levels(
    backend: &CefBackend,
    width: u32,
    height: u32,
    dir: &std::path::Path,
) -> Box<dyn jstream_browser::BrowserPage> {
    backend
        .open(&PageSpec {
            location: PageLocation::Html(LEVELS_PAGE.into()),
            size: FrameSize::new(width, height).expect("a size"),
            fps: 30,
            css: DEFAULT_CSS.into(),
            local_network: false,
            page_api: Some(jstream_browser::PageApi {
                levels: true,
                ..jstream_browser::PageApi::default()
            }),
        })
        .unwrap_or_else(|error| {
            let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
            panic!("the levels page didn't open: {error}\nCEF's log:\n{log}");
        })
}
