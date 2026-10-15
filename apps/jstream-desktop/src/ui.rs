//! The web UI's files, served to the webview through the app's own URL scheme
//! ([`SCHEME`]), and the page shown instead when the app can't start.
//!
//! The webview loads `jstream-ui`'s bundle (`trunk build`'s `dist`) from a folder at run
//! time, so nothing is embedded in the binary and the build doesn't need the bundle.
//!
//! - **Paths.** The path is percent-decoded and split into segments. A segment that is
//!   empty, `.` or `..`, or holds `\`, `:` or NUL is refused (404), and the file found is
//!   canonicalized and must still be inside the canonical root, so neither `..` nor a
//!   symbolic link reaches outside it. `/` serves `index.html`. A missing path whose
//!   last segment has no `.` serves `index.html` too, for the UI's own routes.
//! - **Headers.** `Content-Type` from the extension, `X-Content-Type-Options: nosniff`,
//!   `Cache-Control: no-cache`, and a `Content-Security-Policy` ([`csp`]) that lets the
//!   page connect only to the in-process engine. In an HTML file, trunk's nonce
//!   placeholder is replaced by a fresh random nonce, which the policy names.
//! - **Methods.** `GET` and `HEAD`; any other is 405.
//!
//! Docs: apps/jstream-desktop/README.md#design

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The URL scheme the app serves the UI on. WebView2 maps it to the origin
/// `http://jstream.localhost` on Windows; other webviews use `jstream://localhost`.
pub const SCHEME: &str = "jstream";

/// The origin pages served on [`SCHEME`] have: what the webview sends as `Origin` on the
/// WebSocket upgrade, so the engine's server must accept it.
pub const ORIGIN: &str = if cfg!(windows) {
    "http://jstream.localhost"
} else {
    "jstream://localhost"
};

/// What trunk writes in `nonce` attributes with `create_nonce = true` (the UI's
/// `Trunk.toml`), replaced with a fresh nonce on every HTML response.
const NONCE_PLACEHOLDER: &str = "{{__TRUNK NONCE__}}";

/// The largest file served, in bytes. A UI bundle is a few MiB.
const MAX_FILE_BYTES: u64 = 64 << 20;

/// A response, independent of the webview library, so it is tested on every platform.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    /// The HTTP status code.
    pub status: u16,
    /// Header names and values.
    pub headers: Vec<(&'static str, String)>,
    /// The body; empty for `HEAD`.
    pub body: Vec<u8>,
}

/// What the scheme serves.
#[derive(Debug)]
pub enum Ui {
    /// The UI bundle in `root` (canonicalized), allowed to connect to `engine`
    /// (`ws://127.0.0.1:<port>`).
    Files {
        /// The bundle's folder.
        root: PathBuf,
        /// The engine's WebSocket origin, for the policy's `connect-src`.
        engine: String,
    },
    /// The app couldn't start: every path serves a page that says why.
    Problem(String),
}

impl Ui {
    /// The bundle in `dir`, connecting to `engine`. A `Problem` that says how to build
    /// it when `dir` is not a folder with an `index.html`.
    pub fn files(dir: &Path, engine: String) -> Ui {
        match dir.canonicalize() {
            Ok(root) if root.join("index.html").is_file() => Ui::Files { root, engine },
            _ => Ui::Problem(format!(
                "The web UI was not found in `{}`. Build it with `trunk build --release` \
                 in `crates/jstream-ui` and put its `dist` folder there as `ui`, or start \
                 the app with `--ui-dir <dist folder>`.",
                dir.display()
            )),
        }
    }

    /// The reply to a `method` request for `path` (the URL's path, still
    /// percent-encoded). `nonce` makes the nonce for an HTML response.
    pub fn respond(&self, method: &str, path: &str, nonce: impl FnOnce() -> String) -> Reply {
        let head = match method {
            "GET" => false,
            "HEAD" => true,
            _ => return plain(405, "method not allowed"),
        };
        let mut reply = match self {
            Ui::Problem(message) => problem_page(message),
            Ui::Files { root, engine } => match resolve(root, path) {
                Some(file) => serve_file(&file, engine, nonce),
                None => plain(404, "not found"),
            },
        };
        if head {
            reply.body.clear();
        }
        reply
    }
}

/// Reads `file` into a reply, with its type, the security headers and, for HTML, a
/// fresh nonce in place of trunk's placeholder.
fn serve_file(file: &Path, engine: &str, nonce: impl FnOnce() -> String) -> Reply {
    let too_big = std::fs::metadata(file).map_or(true, |m| m.len() > MAX_FILE_BYTES);
    if too_big {
        return plain(404, "not found");
    }
    let Ok(mut body) = std::fs::read(file) else {
        return plain(404, "not found");
    };
    let content_type = mime(file);
    let mut headers = vec![("Content-Type", content_type.to_owned())];
    let nonce = if content_type.starts_with("text/html") {
        let nonce = nonce();
        if let Ok(text) = String::from_utf8(body.clone()) {
            body = text.replace(NONCE_PLACEHOLDER, &nonce).into_bytes();
        }
        Some(nonce)
    } else {
        None
    };
    headers.extend(security_headers(&csp(engine, nonce.as_deref())));
    Reply {
        status: 200,
        headers,
        body,
    }
}

/// The page shown when the app can't start, with `message` escaped.
fn problem_page(message: &str) -> Reply {
    let mut html = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>JStream</title></head><body><h1>JStream could not start</h1><p>",
    );
    escape_html(&mut html, message);
    html.push_str("</p><p>The log file in the data folder has the details.</p></body></html>");
    let mut headers = vec![("Content-Type", "text/html; charset=utf-8".to_owned())];
    headers.extend(security_headers("default-src 'none'"));
    Reply {
        status: 200,
        headers,
        body: html.into_bytes(),
    }
}

/// A plain-text reply with `status`.
fn plain(status: u16, text: &str) -> Reply {
    let mut headers = vec![("Content-Type", "text/plain; charset=utf-8".to_owned())];
    headers.extend(security_headers("default-src 'none'"));
    Reply {
        status,
        headers,
        body: format!("{text}\n").into_bytes(),
    }
}

/// The headers every reply carries.
fn security_headers(policy: &str) -> [(&'static str, String); 3] {
    [
        ("Content-Security-Policy", policy.to_owned()),
        ("X-Content-Type-Options", "nosniff".to_owned()),
        ("Cache-Control", "no-cache".to_owned()),
    ]
}

/// The Content-Security-Policy for the UI's files: its own files, WASM compiled from
/// them, inline scripts and styles only with the response's nonce, and connections
/// only to `engine`.
pub fn csp(engine: &str, nonce: Option<&str>) -> String {
    let nonce = nonce.map(|n| format!(" 'nonce-{n}'")).unwrap_or_default();
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{nonce}; \
         style-src 'self'{nonce}; img-src 'self' data: blob:; connect-src 'self' {engine}; \
         object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// The file `path` names inside `root`, or `None` when there is none to serve.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let segments = segments(path)?;
    let mut candidate = root.to_path_buf();
    candidate.extend(&segments);
    let inside = |path: &Path| {
        let found = path.canonicalize().ok()?;
        found.starts_with(root).then_some(found)
    };
    match inside(&candidate) {
        Some(found) if found.is_dir() => inside(&found.join("index.html")),
        Some(found) => Some(found),
        None => {
            let last = segments.last().map(String::as_str).unwrap_or_default();
            if last.contains('.') {
                None
            } else {
                inside(&root.join("index.html"))
            }
        }
    }
}

/// A path's percent-decoded segments, or `None` if one may not name a file below the
/// root: empty, `.` or `..`, holding `\`, `:`, `/` or NUL, or not UTF-8 once decoded.
fn segments(path: &str) -> Option<Vec<String>> {
    let path = path.strip_prefix('/')?;
    if path.is_empty() {
        return Some(Vec::new());
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    path.split('/')
        .map(|raw| {
            let segment = percent_decode(raw)?;
            let refused = segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.contains(['\\', ':', '\0', '/']);
            (!refused).then_some(segment)
        })
        .collect()
}

/// Decodes `%XX` escapes; `None` for a broken escape or a result that isn't UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The `Content-Type` for a file name's extension; `application/octet-stream` for one
/// not listed. `application/wasm` lets the webview compile a module while it streams.
fn mime(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Appends `text` to `out`, escaped for HTML text and attribute values.
fn escape_html(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// A fresh nonce: 16 random bytes from the OS, as hex. Falls back to an empty nonce
/// (which matches no inline script) if the OS has no randomness to give.
pub fn new_nonce() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::{NONCE_PLACEHOLDER, Reply, Ui, csp, new_nonce, segments};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ENGINE: &str = "ws://127.0.0.1:4455";

    /// A fresh folder under the system's temporary folder, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("jstream-desktop-ui-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
            Scratch(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn header<'a>(reply: &'a Reply, name: &str) -> Option<&'a str> {
        reply
            .headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    /// A bundle like trunk's: an index with a nonce placeholder, a script, a module.
    fn bundle(dir: &Path) {
        let index = format!("<script nonce=\"{NONCE_PLACEHOLDER}\">boot()</script>");
        let write = |name: &str, body: &[u8]| {
            fs::write(dir.join(name), body).unwrap_or_else(|e| panic!("{name}: {e}"));
        };
        write("index.html", index.as_bytes());
        write("app-1a2b.js", b"export {}");
        write("app-1a2b_bg.wasm", b"\0asm");
        fs::create_dir(dir.join("assets")).unwrap_or_else(|e| panic!("{e}"));
        write("assets/style.css", b"body{}");
    }

    fn served(dir: &Path) -> Ui {
        let ui = Ui::files(dir, ENGINE.to_owned());
        assert!(matches!(ui, Ui::Files { .. }), "{ui:?}");
        ui
    }

    fn get(ui: &Ui, path: &str) -> Reply {
        ui.respond("GET", path, || "n0nce".to_owned())
    }

    #[test]
    fn serves_the_bundle_with_its_types_and_a_fresh_nonce() {
        let dir = Scratch::new();
        bundle(dir.path());
        let ui = served(dir.path());

        let index = get(&ui, "/");
        assert_eq!(index.status, 200);
        assert_eq!(
            header(&index, "Content-Type"),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(index.body, b"<script nonce=\"n0nce\">boot()</script>");
        assert_eq!(
            header(&index, "Content-Security-Policy"),
            Some(csp(ENGINE, Some("n0nce")).as_str())
        );
        assert_eq!(header(&index, "X-Content-Type-Options"), Some("nosniff"));

        let wasm = get(&ui, "/app-1a2b_bg.wasm");
        assert_eq!(header(&wasm, "Content-Type"), Some("application/wasm"));
        assert_eq!(wasm.body, b"\0asm");
        let css = get(&ui, "/assets/style.css");
        assert_eq!(
            header(&css, "Content-Type"),
            Some("text/css; charset=utf-8")
        );
        let script = get(&ui, "/app%2D1a2b.js");
        assert_eq!(script.body, b"export {}", "percent-decoded");

        // A UI route serves the index; a missing file is 404.
        assert_eq!(get(&ui, "/scenes").status, 200);
        assert_eq!(get(&ui, "/missing.js").status, 404);
        // HEAD has no body, other methods none at all.
        let head = ui.respond("HEAD", "/", String::new);
        assert_eq!((head.status, head.body.len()), (200, 0));
        assert_eq!(ui.respond("POST", "/", String::new).status, 405);
    }

    #[test]
    fn nothing_outside_the_bundle_is_reachable() {
        let outer = Scratch::new();
        fs::write(outer.path().join("secret.txt"), "no").unwrap_or_else(|e| panic!("{e}"));
        let dir = outer.path().join("ui");
        fs::create_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
        bundle(&dir);
        let ui = served(&dir);
        for path in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/assets/..%2f..%2fsecret.txt",
            "/..%5csecret.txt",
            "/C:%5cWindows%5cwin.ini",
            "//etc/passwd",
            "/%zz",
            "relative",
        ] {
            let reply = get(&ui, path);
            assert_ne!(reply.body, b"no", "{path}");
            assert!(
                reply.status == 404 || reply.body.starts_with(b"<script"),
                "{path}"
            );
        }
        assert_eq!(segments("/a/./b"), None);
        assert_eq!(segments("/a//b"), None);
        assert_eq!(segments("/"), Some(Vec::new()));
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_bundle_is_refused() {
        let outer = Scratch::new();
        fs::write(outer.path().join("secret.txt"), "no").unwrap_or_else(|e| panic!("{e}"));
        let dir = outer.path().join("ui");
        fs::create_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
        bundle(&dir);
        std::os::unix::fs::symlink(outer.path().join("secret.txt"), dir.join("link.txt"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(get(&served(&dir), "/link.txt").status, 404);
    }

    #[test]
    fn a_missing_bundle_shows_how_to_build_it_and_escapes_the_message() {
        let dir = Scratch::new();
        let ui = Ui::files(&dir.path().join("none"), ENGINE.to_owned());
        let reply = get(&ui, "/anything");
        let page = String::from_utf8(reply.body).unwrap_or_default();
        assert!(page.contains("trunk build"), "{page}");
        assert_eq!(
            reply
                .headers
                .iter()
                .find(|(n, _)| *n == "Content-Security-Policy")
                .map(|(_, v)| v.as_str()),
            Some("default-src 'none'")
        );

        let ui = Ui::Problem("<script>x</script> & \"q\"".to_owned());
        let page = String::from_utf8(get(&ui, "/").body).unwrap_or_default();
        assert!(page.contains("&lt;script&gt;x&lt;/script&gt; &amp; &quot;q&quot;"));
        assert!(!page.contains("<script>"));
    }

    #[test]
    fn nonces_are_fresh_hex() {
        let (a, b) = (new_nonce(), new_nonce());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
