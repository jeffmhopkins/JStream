//! The links the window hands to the system browser: the UI's Help menu and About
//! dialog link to the project's pages on GitHub, which never load in the window
//! itself (`shell.rs`).
//!
//! Docs: apps/jstream-desktop/README.md#design

/// The project's home on GitHub, from the workspace's `repository`: the UI's links
/// (`jstream-ui`'s `about.rs`) all start with it.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Whether `url` is one of the project's pages, which the window opens in the system
/// browser: [`REPOSITORY`] itself, or a path, query or fragment under it. Nothing else
/// leaves the app: not another repository whose name starts the same, not another host,
/// and nothing with whitespace or control characters a command line could misread.
pub fn is_project_page(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(REPOSITORY) else {
        return false;
    };
    let under = rest.is_empty() || rest.starts_with(['/', '?', '#']);
    under
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
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
            "file:///C:/Windows",
            "",
        ] {
            assert!(!is_project_page(other), "{other}");
        }
    }
}
