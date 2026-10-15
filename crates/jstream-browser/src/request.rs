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
//!   that file's folder or below it. Both sides are compared after resolving symbolic
//!   links and `..`, so a link inside the folder can't reach out of it. A file that
//!   doesn't exist is refused.
//! - Every other scheme (Chromium's internal pages, `filesystem:`, extensions) is
//!   refused.
//!
//! The folder is the user's own choice of trusted content, as an asset root is
//! (`jstream-sources`): something that can write inside it can also swap a file for a
//! link between the check and Chromium opening it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::PageLocation;

/// A page's request rules, and how many requests it refused.
#[derive(Debug)]
pub struct RequestPolicy {
    /// The folder a file page may read below, resolved. `None` for a web or blank page,
    /// and for a file page whose folder could not be resolved: then no file is allowed.
    folder: Option<PathBuf>,
    denied: AtomicU64,
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
            PageLocation::File(file) => file.parent().and_then(|dir| dir.canonicalize().ok()),
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
            "about" if matches!(rest, "blank" | "srcdoc") => Verdict::Allow,
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
        match path.canonicalize() {
            Ok(path) if path.starts_with(folder) => Verdict::Allow,
            Ok(_) => Verdict::Deny("the file is outside the page's folder"),
            Err(_) => Verdict::Deny("the file does not exist or can't be read"),
        }
    }
}

/// The host path a `file:` URL names, given what follows `file:`: `//host/path` with an
/// empty host (or `localhost`) for a local path, or a server's name for a UNC path on
/// Windows. The query and fragment are ignored. `None` for anything else, or a path
/// that doesn't decode to UTF-8 without NULs.
fn file_path(rest: &str) -> Option<PathBuf> {
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let rest = rest.strip_prefix("//")?;
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

/// The folder a policy allows files below, for tests.
#[cfg(test)]
fn folder(policy: &RequestPolicy) -> Option<&std::path::Path> {
    policy.folder.as_deref()
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
        assert!(deny(policy.check(&file_url(&root.join("page/link.txt")))));
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
