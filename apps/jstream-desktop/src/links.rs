//! The links the window hands to the system browser: the UI's Help menu and About
//! dialog link to the project's pages on GitHub, which never load in the window
//! itself (`shell.rs`).
//!
//! Docs: apps/jstream-desktop/README.md#design

/// The project's home on GitHub, from the workspace's `repository`: the UI's links
/// (`jstream-ui`'s `about.rs`) all start with it.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Whether `url` is one of the project's pages, which the window opens in the system
/// browser: [`REPOSITORY`] itself, or a path, query or fragment under it, written only
/// with the characters the UI's links use (ASCII letters and digits, and `/ # ? . _ = & -`).
/// Nothing else leaves the app: not another repository whose name starts the same, not
/// another host, and nothing Explorer's command line could read as more than one URL
/// (no spaces, quotes, commas, backslashes or percent escapes).
pub fn is_project_page(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(REPOSITORY) else {
        return false;
    };
    let under = rest.is_empty() || rest.starts_with(['/', '?', '#']);
    under
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/#?._=&-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_projects_own_pages_open_in_the_browser() {
        assert_eq!(REPOSITORY, "https://github.com/jeffmhopkins/JStream");
        for page in [
            "https://github.com/jeffmhopkins/JStream",
            "https://github.com/jeffmhopkins/JStream/issues/new",
            "https://github.com/jeffmhopkins/JStream/releases",
            "https://github.com/jeffmhopkins/JStream#readme",
            "https://github.com/jeffmhopkins/JStream/blob/main/LICENSE-MIT",
            "https://github.com/jeffmhopkins/JStream/blob/main/docs/GETTING-STARTED.md",
            "https://github.com/jeffmhopkins/JStream/commit/0123456789abcdef0123456789abcdef01234567",
            "https://github.com/jeffmhopkins/JStream/releases/tag/v0.1.0-alpha.4.2",
        ] {
            assert!(is_project_page(page), "{page}");
        }
        for other in [
            "https://github.com/jeffmhopkins/JStreamEvil",
            "https://github.com/jeffmhopkins/JStream.evil.example",
            "https://github.com/jeffmhopkins",
            "http://github.com/jeffmhopkins/JStream",
            "https://example.com/?https://github.com/jeffmhopkins/JStream",
            "https://github.com/jeffmhopkins/JStream/ x",
            "https://github.com/jeffmhopkins/JStream/\"x",
            "https://github.com/jeffmhopkins/JStream?x,\\\\host\\share\\y.exe",
            "https://github.com/jeffmhopkins/JStream/%2e%2e/other",
            "https://github.com/jeffmhopkins/JStream/a,b",
            "file:///C:/Windows",
            "",
        ] {
            assert!(!is_project_page(other), "{other}");
        }
    }
}
