//! What a page is: where it loads from, its size and frame rate, and its custom CSS.

use std::path::{Path, PathBuf};

use jstream_media::FrameSize;

use crate::BrowserError;

/// The highest frame rate a page may paint at: CEF's cap for off-screen rendering.
pub const MAX_FPS: u32 = 60;

/// The longest custom CSS a page takes, in bytes.
pub const MAX_CSS_BYTES: usize = 64 * 1024;

/// The custom CSS a new browser source starts with: a transparent page with no margin
/// and no scroll bars, so an overlay shows only what it draws (OBS's default too).
pub const DEFAULT_CSS: &str =
    "body { background-color: rgba(0, 0, 0, 0); margin: 0px auto; overflow: hidden; }";

/// The longest URL a page takes, in bytes.
const MAX_URL_BYTES: usize = 8 * 1024;

/// One page to open off screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageSpec {
    /// Where the page loads from.
    pub location: PageLocation,
    /// Its size in pixels: the page's viewport, at one CSS pixel per pixel.
    pub size: FrameSize,
    /// The most frames a second it paints, `1..=`[`MAX_FPS`]. A page that changes less
    /// often paints less.
    pub fps: u32,
    /// Custom CSS applied on every load ([`css_script`]).
    pub css: String,
    /// Whether a page from the internet may make requests to this computer and the local
    /// network, for widgets that talk to a local app. Off, they are refused
    /// ([`RequestPolicy`](crate::RequestPolicy), #347).
    pub local_network: bool,
}

/// Where a page loads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageLocation {
    /// An empty page (`about:blank`): a source with neither a URL nor a file.
    Blank,
    /// A web page: an `http` or `https` URL, as [`PageLocation::web`] checked it.
    Web(String),
    /// A local HTML file, by its absolute host path.
    File(PathBuf),
}

impl PageLocation {
    /// A web page at `url`, which must be `http://` or `https://` (scheme in any case).
    /// Surrounding whitespace is ignored; an empty URL is [`PageLocation::Blank`].
    ///
    /// Other schemes are refused: `file:` because local files come through an asset
    /// reference, which is checked against its root, and `javascript:`, `data:` and
    /// Chromium's internal pages because a source's URL should only ever load a page
    /// from the web.
    ///
    /// # Errors
    /// [`BrowserError::Url`] for another scheme, a URL with no host, one containing
    /// whitespace or control characters, or one over 8 KiB.
    pub fn web(url: &str) -> Result<Self, BrowserError> {
        let url = url.trim();
        if url.is_empty() {
            return Ok(Self::Blank);
        }
        let refuse = |reason: &str| {
            Err(BrowserError::Url {
                reason: format!("the URL {reason}"),
            })
        };
        if url.len() > MAX_URL_BYTES {
            return refuse("is longer than 8 KiB");
        }
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return refuse("must not contain spaces or control characters");
        }
        let Some((scheme, rest)) = url.split_once("://") else {
            return refuse("must start with http:// or https://");
        };
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return refuse("must start with http:// or https://");
        }
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        // The authority without any user info and port: what is left must name a host.
        let host = host.rsplit('@').next().unwrap_or_default();
        if host.is_empty() || host.starts_with(':') {
            return refuse("has no host");
        }
        Ok(Self::Web(url.to_owned()))
    }

    /// The URL the browser loads.
    pub fn to_url(&self) -> String {
        match self {
            Self::Blank => "about:blank".to_owned(),
            Self::Web(url) => url.clone(),
            Self::File(path) => file_url(path),
        }
    }
}

/// The `file:` URL of an absolute host path, percent-encoded, as Chromium expects it.
/// Windows paths (`C:\a b\x.html`, `\\server\share\x.html`, and both under the `\\?\`
/// prefix that `canonicalize` adds) become `file:///C:/a%20b/x.html` and
/// `file://server/share/x.html`; Unix paths become `file:///a/x.html`.
pub(crate) fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = text.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        text.into_owned()
    };
    let slashed = text.replace('\\', "/");
    let encoded = percent_encode(&slashed);
    if slashed.starts_with("//") {
        // A UNC path: its server is the URL's host.
        format!("file:{encoded}")
    } else if slashed.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// Percent-encodes every byte of `path` but the unreserved ones (RFC 3986), `/` and `:`.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The script that applies `css` to a loaded page: it puts the CSS in one `<style>`
/// element of its own at the end of `<head>` (or of the document, for a page without
/// one), creating it the first time and replacing its text after, so applying it again
/// with new CSS leaves no trace of the old. The CSS goes in as a JavaScript string
/// literal, so no CSS text can end the script early.
pub fn css_script(css: &str) -> String {
    format!(
        "(function () {{\n\
         var id = 'jstream-custom-css';\n\
         var style = document.getElementById(id);\n\
         if (!style) {{\n\
         style = document.createElement('style');\n\
         style.id = id;\n\
         (document.head || document.documentElement).appendChild(style);\n\
         }}\n\
         style.textContent = {};\n\
         }})();",
        js_string(css)
    )
}

/// `text` as a double-quoted JavaScript string literal. Quotes, backslashes, control
/// characters, the line and paragraph separators and `<` are escaped, so the literal is
/// safe inside any script.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls_are_http_or_https_with_a_host() {
        assert_eq!(PageLocation::web("  "), Ok(PageLocation::Blank));
        assert_eq!(
            PageLocation::web(" https://example.com/alerts?id=1 "),
            Ok(PageLocation::Web("https://example.com/alerts?id=1".into()))
        );
        assert_eq!(
            PageLocation::web("HTTP://localhost:8080"),
            Ok(PageLocation::Web("HTTP://localhost:8080".into()))
        );
        assert!(PageLocation::web("http://user@host/").is_ok());
        for refused in [
            "file:///C:/x.html",
            "javascript:alert(1)",
            "data:text/html,hi",
            "chrome://gpu",
            "example.com",
            "https://",
            "https:///path",
            "https://:80/",
            "https://exa mple.com",
            "https://example.com/\u{7}",
        ] {
            assert!(
                matches!(PageLocation::web(refused), Err(BrowserError::Url { .. })),
                "{refused:?} should be refused"
            );
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert!(PageLocation::web(&long).is_err());
    }

    #[test]
    fn file_urls_are_percent_encoded() {
        let url = |p: &str| PageLocation::File(PathBuf::from(p)).to_url();
        assert_eq!(
            url(r"C:\Overlays\a b#1.html"),
            "file:///C:/Overlays/a%20b%231.html"
        );
        assert_eq!(url(r"\\?\C:\x\é.html"), "file:///C:/x/%C3%A9.html");
        assert_eq!(url(r"\\?\UNC\nas\share\x.html"), "file://nas/share/x.html");
        assert_eq!(url(r"\\nas\share\x.html"), "file://nas/share/x.html");
        assert_eq!(url("/home/me/x%y.html"), "file:///home/me/x%25y.html");
        assert_eq!(PageLocation::Blank.to_url(), "about:blank");
    }

    #[test]
    fn css_goes_in_as_one_escaped_string() {
        let script = css_script("body { color: red; }\n</script><script>alert(\"x\\\")");
        assert!(script.contains(
            r#"style.textContent = "body { color: red; }\n\u003c/script>\u003cscript>alert(\"x\\\")";"#
        ));
        assert!(!script.contains("</script>"));
        assert!(css_script("a\u{2028}b").contains(r"a\u2028b"));
        assert!(css_script("\u{0}").contains(r"\u0000"));
    }
}
