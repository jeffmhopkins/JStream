//! Local network access from pages, through Chromium's own checks (#347, #1035).
//!
//! The request policy reads a request's URL, so it can't see a public name that resolves
//! to this computer. Chromium's Local Network Access checks can, at the network layer,
//! and in the pinned CEF they refuse a public origin's local request without asking the
//! permission handler. These tests show both halves on the real helper: what an opt-in
//! now reaches, and what nothing reaches.
//!
//! Every server here listens on `127.0.0.1`. The helper is started through a wrapper
//! script that adds two of Chromium's test switches: `--host-resolver-rules` maps the
//! `.test` names to `127.0.0.1`, and `--ip-address-space-overrides` makes the page's own
//! server count as a public address, as a page from the internet would be. The
//! `https` page's certificate is made here, and the wrapper passes
//! `--ignore-certificate-errors` for it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use jstream_browser::{
    BrowserBackend, CefBackend, CefConfig, DEFAULT_CSS, HTML_PAGE_URL, PageLocation, PageSpec,
};
use jstream_media::{FrameSize, Timebase};
use jstream_platform::fake::FakeClock;

/// How long a page has to paint its verdict.
const VERDICT_TIMEOUT: Duration = Duration::from_secs(60);

/// What a test page paints once its request succeeded.
const REACHED: [u8; 4] = [255, 0, 0, 255];
/// What it paints once its request failed.
const REFUSED: [u8; 4] = [0, 0, 255, 255];

/// A page that fetches `target` and paints [`REACHED`] or [`REFUSED`], with `extra`
/// HTML after the script.
fn fetching_page(target: &str, extra: &str) -> String {
    format!(
        r#"<!doctype html><html><body style="background:#00ff00"><script>
fetch("{target}").then(r => r.text()).then(
  () => {{ document.body.style.background = '#ff0000'; }},
  () => {{ document.body.style.background = '#0000ff'; }});
</script>{extra}</body></html>"#
    )
}

/// Paths a server was asked for, in order.
type Asked = Arc<Mutex<Vec<String>>>;

/// Serves every request on `listener` with `body` (as HTML, readable from any origin),
/// over TLS when `tls` is given, until the test ends. Returns the paths it is asked for.
fn serve(listener: TcpListener, body: String, tls: Option<Arc<rustls::ServerConfig>>) -> Asked {
    let asked = Asked::default();
    let log = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (body, log, tls) = (body.clone(), Arc::clone(&log), tls.clone());
            std::thread::spawn(move || {
                let _ = match tls {
                    Some(config) => match rustls::ServerConnection::new(config) {
                        Ok(conn) => answer(rustls::StreamOwned::new(conn, stream), &body, &log),
                        Err(_) => Ok(()),
                    },
                    None => answer(stream, &body, &log),
                };
            });
        }
    });
    asked
}

/// Answers one HTTP/1.1 request on `stream` with `body`, logging its path.
fn answer(
    mut stream: impl Read + Write,
    body: &str,
    log: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    let mut head = Vec::new();
    {
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                break;
            }
            head.push(line);
        }
    }
    let path = head
        .first()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_owned();
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(path);
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nAccess-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// A TLS server config for `name`, with a certificate made now.
fn tls_for(name: &str) -> Arc<rustls::ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec![name.to_owned()]).expect("a certificate");
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_no_client_auth()
    .with_single_cert(vec![cert.cert.der().clone()], key)
    .expect("the certificate");
    Arc::new(config)
}

/// A port on `127.0.0.1`, and its listener.
fn listen() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().expect("its address").port();
    (listener, port)
}

/// A fresh folder for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jstream-lna-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    dir
}

/// A script in `dir` that starts the real helper with the `.test` names mapped to
/// `127.0.0.1` and `public` ports counted as public addresses.
fn wrapper(dir: &Path, public: &[u16]) -> PathBuf {
    let helper = std::env::var("JSTREAM_BROWSER_HELPER").expect("JSTREAM_BROWSER_HELPER is set");
    let overrides: Vec<String> = public
        .iter()
        .map(|port| format!("127.0.0.1:{port}=public"))
        .collect();
    let script = dir.join("helper.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec '{helper}' \"$@\" '--host-resolver-rules=MAP *.test 127.0.0.1' \
             '--ip-address-space-overrides={}' --ignore-certificate-errors --no-proxy-server\n",
            overrides.join(",")
        ),
    )
    .expect("the wrapper");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("an executable wrapper");
    script
}

/// Opens `location` through the wrapped helper, and returns the colour it settles on:
/// [`REACHED`] or [`REFUSED`], and how many requests the request policy refused. The
/// page stays open until `settled` holds too.
fn verdict(
    dir: &Path,
    public: &[u16],
    location: PageLocation,
    local_network: bool,
    settled: &dyn Fn() -> bool,
) -> ([u8; 4], u64) {
    let backend = CefBackend::new(
        CefConfig {
            helper: wrapper(dir, public),
            cache_root: dir.join("cef"),
            log_file: Some(dir.join("cef.log")),
        },
        Arc::new(FakeClock::new(Timebase::MILLIS)),
    );
    backend.use_device(None);
    let page = match backend.open(&PageSpec {
        location,
        size: FrameSize::new(32, 32).expect("a size"),
        fps: 30,
        css: DEFAULT_CSS.into(),
        local_network,
        page_api: None,
    }) {
        Ok(page) => page,
        Err(error) => {
            let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
            panic!("the page didn't open: {error}\nCEF's log:\n{log}");
        }
    };
    let deadline = Instant::now() + VERDICT_TIMEOUT;
    let colour = loop {
        let pixel = page.cpu_frame().map(|frame| {
            let p = &frame.planes()[0].data()[..4];
            [p[0], p[1], p[2], p[3]]
        });
        if let Some(pixel) = pixel.filter(|p| *p == REACHED || *p == REFUSED) {
            break pixel;
        }
        if Instant::now() >= deadline {
            let log = std::fs::read_to_string(dir.join("cef.log")).unwrap_or_default();
            panic!("no verdict within {VERDICT_TIMEOUT:?} (last: {pixel:?})\nCEF's log:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    while !settled() {
        assert!(
            Instant::now() < deadline,
            "the page didn't settle within {VERDICT_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let denied = page.denied_requests();
    drop(page);
    drop(backend);
    (colour, denied)
}

/// Whether `asked` holds `path`.
fn was_asked(asked: &Asked, path: &str) -> bool {
    asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|p| p == path)
}

/// #347: a page from the internet can't reach this computer through a public name that
/// resolves to it (as `*.nip.io` does, or DNS rebinding). The request policy lets the
/// name through, since it reads only the URL; Chromium refuses it.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn a_public_page_can_t_reach_this_computer_through_a_name_that_resolves_to_it() {
    let dir = temp_dir("rebind");
    let (target, target_port) = listen();
    let target = serve(target, "pong".into(), None);
    let (page, page_port) = listen();
    serve(
        page,
        fetching_page(&format!("http://rebind.test:{target_port}/ping"), ""),
        None,
    );
    let (colour, denied) = verdict(
        &dir,
        &[page_port],
        PageLocation::Web(format!("http://public.test:{page_port}/")),
        false,
        &|| true,
    );
    assert_eq!(colour, REFUSED);
    assert_eq!(denied, 0, "the policy let it through: Chromium refused it");
    assert!(!was_asked(&target, "/ping"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// #347: an `https` page from the internet whose source opted in reaches this computer;
/// without the opt-in it doesn't, and a third-party frame inside the opted-in page
/// doesn't either. Chromium refuses a public origin's local request without asking the
/// permission handler, so the helper grants the opt-in to Chromium for the source's own
/// origin.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn an_opted_in_https_page_reaches_this_computer_and_its_third_party_frames_don_t() {
    let (target, target_port) = listen();
    let target = serve(target, "pong".into(), None);
    // The frame tells its own server how its request went.
    let (frame, frame_port) = listen();
    let frame = serve(
        frame,
        format!(
            r#"<!doctype html><script>
fetch("http://127.0.0.1:{target_port}/from-frame").then(
  () => fetch("/frame-reached"), () => fetch("/frame-refused"));
</script>"#
        ),
        Some(tls_for("third.test")),
    );
    let (page, page_port) = listen();
    serve(
        page,
        fetching_page(
            &format!("http://127.0.0.1:{target_port}/ping"),
            &format!(
                r#"<iframe style="position:fixed;left:24px;top:24px;width:4px;height:4px;border:0" src="https://third.test:{frame_port}/"></iframe>"#
            ),
        ),
        Some(tls_for("public.test")),
    );
    let location = PageLocation::Web(format!("https://public.test:{page_port}/"));
    for (opted, expected) in [(false, REFUSED), (true, REACHED)] {
        let dir = temp_dir(&format!("opt-in-{opted}"));
        let settled = || was_asked(&frame, "/frame-reached") || was_asked(&frame, "/frame-refused");
        let (colour, _) = verdict(
            &dir,
            &[page_port, frame_port],
            location.clone(),
            opted,
            &settled,
        );
        assert_eq!(colour, expected, "opted in: {opted}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(was_asked(&frame, "/frame-refused"));
    assert!(!was_asked(&frame, "/frame-reached"));
    assert!(!was_asked(&target, "/from-frame"));
}

/// #1035: a page given as HTML loads from its own origin, the reserved one the helper
/// answers itself, and reaches this computer only when its source opts in.
#[test]
#[ignore = "needs the helper (JSTREAM_BROWSER_HELPER) and CEF's runtime beside it"]
fn an_html_page_has_its_own_origin_and_reaches_this_computer_only_when_opted_in() {
    let (target, target_port) = listen();
    let target = serve(target, "pong".into(), None);
    let origin = HTML_PAGE_URL.trim_end_matches('/');
    // Paints the verdict only on its own origin; anywhere else it stays green.
    let html = fetching_page(&format!("http://127.0.0.1:{target_port}/ping"), "").replace(
        "<script>",
        &format!("<script>if (location.origin === '{origin}')"),
    );
    for (opted, expected) in [(false, REFUSED), (true, REACHED)] {
        let dir = temp_dir(&format!("html-{opted}"));
        let (colour, _) = verdict(&dir, &[], PageLocation::Html(html.clone()), opted, &|| true);
        assert_eq!(colour, expected, "opted in: {opted}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(was_asked(&target, "/ping"));
}
