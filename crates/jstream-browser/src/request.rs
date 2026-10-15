//! Which requests a page may make: ADR-0021 allows a browser source no file access
//! outside its own local file's folder. Every request a page makes (navigations,
//! subresources, fetches) goes through [`RequestPolicy::check`] before it starts; the CEF
//! backend cancels the ones it refuses.
//!
//! - `http`, `https`, `ws` and `wss` are allowed for every page: overlays load fonts,
//!   scripts and data from the web.
//! - `data:` and `blob:` are allowed: they hold only what the page itself made.
//! - `about:blank` and `about:srcdoc` are allowed; other `about:` pages are not.
//! - `file:` is allowed only for a page loaded from a local file, and only for files in
//!   that file's folder or below it. The path is first compared by name, with `..`
//!   resolved, and only a path inside the folder is ever opened: a URL naming another
//!   server, a drive-relative path or a device (a reserved name such as `COM1` included)
//!   is refused without touching it. Then both sides are compared
//!   again after resolving symbolic links, so a link inside the folder can't reach out
//!   of it. A file that doesn't exist is refused.
//! - Every other scheme (Chromium's internal pages, `filesystem:`, extensions) is
//!   refused.
//!
//! The folder is the user's own choice of trusted content, as an asset root is
//! (`jstream-sources`): something that can write inside it can also swap a file for a
//! link between the check and Chromium opening it.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::PageLocation;

/// A page's request rules, and how many requests it refused.
#[derive(Debug)]
pub struct RequestPolicy {
    /// The folder a file page may read below. `None` for a web or blank page, and for a
    /// file page whose folder could not be resolved: then no file is allowed.
    folder: Option<Folder>,
    denied: AtomicU64,
}

/// A file page's folder, both ways the policy compares it.
#[derive(Debug)]
struct Folder {
    /// By name ([`lexical`]): checked before anything is opened.
    named: Vec<String>,
    /// With links resolved: checked after.
    resolved: PathBuf,
}

/// Whether one request may go ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It may.
    Allow,
    /// It is refused, for this reason (fixed text that names no path).
    Deny(&'static str),
}

impl RequestPolicy {
    /// The rules for a page loaded from `location`.
    pub fn for_location(location: &PageLocation) -> Self {
        let folder = match location {
            PageLocation::File(file) => file.parent().and_then(|dir| {
                Some(Folder {
                    named: lexical(dir)?,
                    resolved: dir.canonicalize().ok()?,
                })
            }),
            PageLocation::Blank | PageLocation::Web(_) => None,
        };
        Self {
            folder,
            denied: AtomicU64::new(0),
        }
    }

    /// Whether the page may request `url`. A refusal is counted ([`denied`](Self::denied)).
    pub fn check(&self, url: &str) -> Verdict {
        let verdict = self.verdict(url);
        if verdict != Verdict::Allow {
            self.denied.fetch_add(1, Ordering::Relaxed);
        }
        verdict
    }

    /// How many requests the page has had refused.
    pub fn denied(&self) -> u64 {
        self.denied.load(Ordering::Relaxed)
    }

    fn verdict(&self, url: &str) -> Verdict {
        let Some((scheme, rest)) = url.split_once(':') else {
            return Verdict::Deny("the URL has no scheme");
        };
        match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" | "ws" | "wss" | "data" | "blob" => Verdict::Allow,
            "about" if matches!(before_query(rest), "blank" | "srcdoc") => Verdict::Allow,
            "file" => self.file(rest),
            _ => Verdict::Deny("the page may not load this kind of URL"),
        }
    }

    /// The verdict on a `file:` URL, given what follows `file:`.
    fn file(&self, rest: &str) -> Verdict {
        let Some(folder) = &self.folder else {
            return Verdict::Deny("only a page loaded from a local file may load local files");
        };
        let Some(path) = file_path(rest) else {
            return Verdict::Deny("the file URL is malformed");
        };
        // By name first: opening a path outside the folder is itself the access the
        // policy refuses (on Windows, a server's name makes an SMB connection).
        match lexical(&path) {
            Some(named) if named.starts_with(&folder.named) => {}
            _ => return Verdict::Deny("the file is outside the page's folder"),
        }
        match path.canonicalize() {
            Ok(path) if path.starts_with(&folder.resolved) => Verdict::Allow,
            Ok(_) => Verdict::Deny("the file resolves to outside the page's folder"),
            Err(_) => Verdict::Deny("the file does not exist or can't be read"),
        }
    }
}

/// What precedes a URL's query and fragment.
fn before_query(rest: &str) -> &str {
    rest.split(['?', '#']).next().unwrap_or_default()
}

/// An absolute path as names to compare, without touching the file system: `.` dropped
/// and `..` applied by name. On Windows, a drive or server-and-share comes first, in
/// lower case as is every name (Windows compares names without case), with `\\?\`
/// forms the same as plain ones. `None` for a relative path (a drive-relative `C:x`
/// included), a `..` above the root, a device path (`\\.\`, `\\?\` without a drive or
/// share, or a reserved device name such as `COM1` on Windows) or a name that isn't UTF-8.
fn lexical(path: &Path) -> Option<Vec<String>> {
    if !path.has_root() {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => names.push(prefix_name(prefix.kind())?),
            Component::RootDir if names.is_empty() => names.push("/".to_owned()),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if names.len() < 2 {
                    return None;
                }
                names.pop();
            }
            Component::Normal(_) if names.is_empty() => return None,
            Component::Normal(name) => {
                let name = name.to_str()?;
                if device_name(name) {
                    return None;
                }
                names.push(fold(name));
            }
        }
    }
    (!names.is_empty()).then_some(names)
}

#[cfg(windows)]
fn prefix_name(prefix: std::path::Prefix<'_>) -> Option<String> {
    use std::path::Prefix;
    match prefix {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
            Some(format!("{}:", char::from(drive).to_ascii_lowercase()))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => Some(fold(&format!(
            r"\\{}\{}",
            server.to_str()?,
            share.to_str()?
        ))),
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => None,
    }
}

#[cfg(not(windows))]
fn prefix_name(_: std::path::Prefix<'_>) -> Option<String> {
    None
}

/// Whether Windows may open the name as a device wherever it appears (`CON`, `NUL`,
/// `COM1`, `LPT1.txt`, ...), whatever follows the first dot.
#[cfg(windows)]
fn device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let port = upper
                .strip_prefix("COM")
                .or_else(|| upper.strip_prefix("LPT"));
            port.is_some_and(|n| {
                matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                    || matches!(n, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        }
    }
}

#[cfg(not(windows))]
fn device_name(_: &str) -> bool {
    false
}

#[cfg(windows)]
fn fold(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(not(windows))]
fn fold(name: &str) -> String {
    name.to_owned()
}

/// The host path a `file:` URL names, given what follows `file:`: `//host/path` with an
/// empty host (or `localhost`) for a local path, or a server's name for a UNC path on
/// Windows. The query and fragment are ignored. `None` for anything else, or a path
/// that doesn't decode to UTF-8 without NULs.
fn file_path(rest: &str) -> Option<PathBuf> {
    let rest = before_query(rest).strip_prefix("//")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let path = percent_decode(path)?;
    let local = host.is_empty() || host.eq_ignore_ascii_case("localhost");
    host_path(if local { "" } else { host }, &path)
}

#[cfg(windows)]
fn host_path(host: &str, path: &str) -> Option<PathBuf> {
    let path = path.replace('/', "\\");
    if host.is_empty() {
        // `/C:/dir/x.html`: a drive path.
        let path = path.strip_prefix('\\')?;
        let drive = path.as_bytes();
        (drive.len() >= 2 && drive[0].is_ascii_alphabetic() && drive[1] == b':')
            .then(|| PathBuf::from(path))
    } else {
        Some(PathBuf::from(format!(r"\\{host}{path}")))
    }
}

#[cfg(not(windows))]
fn host_path(host: &str, path: &str) -> Option<PathBuf> {
    host.is_empty().then(|| PathBuf::from(path))
}

/// `%XX` escapes decoded; `None` for a bad escape, a NUL, or bytes that aren't UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
}

/// The folder a policy allows files below, resolved, for tests.
#[cfg(test)]
fn folder(policy: &RequestPolicy) -> Option<&Path> {
    policy
        .folder
        .as_ref()
        .map(|folder| folder.resolved.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::file_url;

    /// A fresh folder under the temp dir, with `page/index.html`, `page/img/a.png`,
    /// `page/a b%.css` and `secret.txt` beside `page`.
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "jstream-browser-request-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("page/img")).expect("folders");
        for file in [
            "page/index.html",
            "page/img/a.png",
            "page/a b%.css",
            "secret.txt",
        ] {
            std::fs::write(root.join(file), b"x").expect("file");
        }
        root
    }

    fn deny(verdict: Verdict) -> bool {
        matches!(verdict, Verdict::Deny(_))
    }

    #[test]
    fn a_file_page_loads_only_from_its_folder() {
        let root = tree("folder");
        let page = root.join("page/index.html");
        let policy = RequestPolicy::for_location(&PageLocation::File(page.clone()));
        assert!(folder(&policy).is_some());

        // The stream of requests a page makes, as CEF hands them over.
        let requests = [
            (file_url(&page), true),
            (file_url(&root.join("page/img/a.png")), true),
            (
                format!("{}?v=2#top", file_url(&root.join("page/a b%.css"))),
                true,
            ),
            (file_url(&root.join("secret.txt")), false),
            (file_url(&root.join("page/../secret.txt")), false),
            (file_url(&root.join("page/missing.png")), false),
            ("https://fonts.example.com/a.woff2".to_owned(), true),
            ("wss://chat.example.com/socket".to_owned(), true),
            ("data:image/png;base64,AAAA".to_owned(), true),
            ("blob:file:///0b7c".to_owned(), true),
            ("about:blank".to_owned(), true),
            ("about:blank#frame".to_owned(), true),
            ("about:srcdoc?x".to_owned(), true),
            ("about:version".to_owned(), false),
            ("chrome://settings".to_owned(), false),
            ("filesystem:file:///temporary/x".to_owned(), false),
            ("file:///%00".to_owned(), false),
            ("file:///%zz".to_owned(), false),
            ("nonsense".to_owned(), false),
        ];
        let mut refused = 0;
        for (url, allowed) in &requests {
            let verdict = policy.check(url);
            assert_eq!(verdict == Verdict::Allow, *allowed, "{url}: {verdict:?}");
            refused += u64::from(!allowed);
        }
        assert_eq!(policy.denied(), refused);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn web_and_blank_pages_load_no_files() {
        let root = tree("web");
        let file = file_url(&root.join("page/index.html"));
        for location in [
            PageLocation::Blank,
            PageLocation::Web("https://example.com/".into()),
        ] {
            let policy = RequestPolicy::for_location(&location);
            assert!(deny(policy.check(&file)));
            assert_eq!(policy.check("https://example.com/x.js"), Verdict::Allow);
            assert_eq!(policy.denied(), 1);
        }
        // A file page whose folder is gone allows no files.
        let gone = RequestPolicy::for_location(&PageLocation::File(root.join("nope/x.html")));
        assert!(folder(&gone).is_none());
        assert!(deny(gone.check(&file)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_folder_is_refused() {
        let root = tree("link");
        std::os::unix::fs::symlink(root.join("secret.txt"), root.join("page/link.txt"))
            .expect("symlink");
        let policy = RequestPolicy::for_location(&PageLocation::File(root.join("page/index.html")));
        assert_eq!(
            policy.check(&file_url(&root.join("page/link.txt"))),
            Verdict::Deny("the file resolves to outside the page's folder")
        );
        // Through a linked folder too.
        std::os::unix::fs::symlink(&root, root.join("page/up")).expect("symlink");
        assert!(deny(
            policy.check(&file_url(&root.join("page/up/secret.txt")))
        ));
        assert_eq!(
            policy.check(&file_url(&root.join("page/up/page/img/a.png"))),
            Verdict::Allow
        );
        assert_eq!(policy.denied(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paths_compare_by_name_before_anything_is_opened() {
        #[cfg(not(windows))]
        {
            let names = |p: &str| lexical(Path::new(p));
            assert_eq!(
                names("/a/./b/../c"),
                Some(vec!["/".into(), "a".into(), "c".into()])
            );
            assert_eq!(names("/a/.."), Some(vec!["/".into()]));
            assert_eq!(names("/.."), None);
            assert_eq!(names("a/b"), None);
        }
        #[cfg(windows)]
        {
            let names = |p: &str| lexical(Path::new(p));
            assert_eq!(names(r"C:\A\b"), names(r"\\?\c:\a\B"));
            assert_eq!(names(r"\\Server\Share\x"), names(r"\\?\UNC\server\share\X"));
            assert_eq!(names(r"\\.\pipe\x"), None);
            assert_eq!(names(r"\\?\GLOBALROOT\x"), None);
            assert_eq!(names(r"C:\.."), None);
            assert_eq!(names(r"C:a\b"), None);
            assert_eq!(names(r"C:\a\COM1"), None);
            assert_eq!(names(r"C:\a\nul.txt"), None);
            assert_eq!(names(r"C:\a\Con .css"), None);
            assert_eq!(
                names(r"C:\a\com10"),
                Some(vec!["c:".into(), "a".into(), "com10".into()])
            );
            assert_eq!(
                names(r"C:\a\console"),
                Some(vec!["c:".into(), "a".into(), "console".into()])
            );
        }
    }

    /// A URL naming another server, a device or a drive-relative path is refused by name:
    /// the policy never opens it, which on Windows would connect to that server or open
    /// that device.
    #[cfg(windows)]
    #[test]
    fn a_server_or_device_url_is_refused_without_opening_it() {
        let root = tree("unc");
        let policy = RequestPolicy::for_location(&PageLocation::File(root.join("page/index.html")));
        let device = file_url(&root.join("page/COM1.css"));
        for url in [
            "file://attacker.example/share/x.png",
            "file://./pipe/x",
            "file://localhost/%5C%5Cattacker.example/share/x.png",
            "file:///C:overlays/page/x.png",
            device.as_str(),
        ] {
            let verdict = policy.check(url);
            assert!(
                matches!(
                    verdict,
                    Verdict::Deny(
                        "the file is outside the page's folder" | "the file URL is malformed"
                    )
                ),
                "{url}: {verdict:?}"
            );
        }
        assert_eq!(policy.denied(), 5);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_urls_decode_to_host_paths() {
        assert_eq!(percent_decode("/a%20b/%C3%A9"), Some("/a b/é".into()));
        assert_eq!(percent_decode("/a%2"), None);
        assert_eq!(percent_decode("/%FF"), None);
        assert_eq!(file_path("/x"), None);
        assert_eq!(file_path("//host"), None);
        #[cfg(not(windows))]
        {
            assert_eq!(file_path("///a/b.html"), Some(PathBuf::from("/a/b.html")));
            assert_eq!(file_path("//localhost/a"), Some(PathBuf::from("/a")));
            assert_eq!(file_path("//server/a"), None);
        }
        #[cfg(windows)]
        {
            assert_eq!(
                file_path("///C:/a/b.html"),
                Some(PathBuf::from(r"C:\a\b.html"))
            );
            assert_eq!(
                file_path("//server/share/a"),
                Some(PathBuf::from(r"\\server\share\a"))
            );
            assert_eq!(file_path("///a/b"), None);
        }
    }
}
