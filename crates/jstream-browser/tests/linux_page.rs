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

/// #1069: a page served from this computer loads in a source whose location is local.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_page_served_from_this_computer_loads() {
    let dir = temp_dir("http");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().expect("its address").port();
    // Answers every request with the page, until the test ends.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("the stream"));
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{PAGE}",
                PAGE.len()
            );
        }
    });
    paints_red(
        &dir,
        PageLocation::Web(format!("http://127.0.0.1:{port}/index.html")),
    );
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
