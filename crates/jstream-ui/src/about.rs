//! The Help menu's links and the About dialog (docs/UI-FEATURES.md#header): the app's
//! icon, its version and build, the protocol the engine speaks, and links to the
//! project's pages on GitHub.
//!
//! The icon is the desktop app's own, `apps/jstream-desktop/icons/icon.png`, which
//! trunk copies into the bundle as `icon.png` (`index.html`): replacing that file
//! changes the header's mark, the favicon and this dialog at once.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::dialog::Modal;
use leptos::prelude::*;

/// The bundle's copy of the app icon, relative to the page.
pub const ICON: &str = "icon.png";

/// The project's home on GitHub, from the workspace's `repository`.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The release version this UI was built as, from the workspace's `version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit this UI was built from: GitHub Actions sets `GITHUB_SHA` for the CI and
/// release builds. `None` for a local build.
pub const COMMIT: Option<&str> = option_env!("GITHUB_SHA");

/// The licence, as the workspace states it.
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");

/// A link the Help menu and the About dialog offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// The README, which starts the documentation.
    Documentation,
    /// The getting-started guide.
    GettingStarted,
    /// The releases and their notes.
    Releases,
    /// A new issue.
    ReportIssue,
    /// The repository itself.
    Source,
    /// The MIT licence text.
    LicenseMit,
    /// The Apache 2.0 licence text.
    LicenseApache,
}

impl Link {
    /// The Help menu's links, in order.
    pub const MENU: [Link; 4] = [
        Link::Documentation,
        Link::GettingStarted,
        Link::Releases,
        Link::ReportIssue,
    ];

    /// What the link is called.
    pub fn label(self) -> &'static str {
        match self {
            Link::Documentation => "Documentation",
            Link::GettingStarted => "Getting started",
            Link::Releases => "Release notes",
            Link::ReportIssue => "Report an issue",
            Link::Source => "Source code on GitHub",
            Link::LicenseMit => "MIT",
            Link::LicenseApache => "Apache-2.0",
        }
    }

    /// Where it goes: a page of [`REPOSITORY`].
    pub fn url(self) -> String {
        let path = match self {
            Link::Documentation => "#readme",
            Link::GettingStarted => "/blob/main/docs/GETTING-STARTED.md",
            Link::Releases => "/releases",
            Link::ReportIssue => "/issues/new",
            Link::Source => "",
            Link::LicenseMit => "/blob/main/LICENSE-MIT",
            Link::LicenseApache => "/blob/main/LICENSE-APACHE",
        };
        format!("{REPOSITORY}{path}")
    }
}

/// The build line: the commit's first seven characters, or "local build".
pub fn build_label(commit: Option<&str>) -> String {
    match commit.map(str::trim).filter(|c| !c.is_empty()) {
        Some(commit) => commit.chars().take(7).collect(),
        None => "local build".to_owned(),
    }
}

/// An external link: a new tab in a browser, the system browser in the desktop app.
fn external(link: Link, class: &'static str) -> impl IntoView {
    view! {
        <a class=class href=link.url() target="_blank" rel="noopener noreferrer">
            {link.label()}
        </a>
    }
}

/// The About dialog.
#[component]
pub fn About(
    /// The protocol version the engine speaks, from the session.
    protocol: Signal<String>,
    /// Closes the dialog.
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let build = build_label(COMMIT);
    let commit_url = COMMIT.map(|c| format!("{REPOSITORY}/commit/{}", c.trim()));
    view! {
        <Modal label="About JStream" class="about-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"About JStream"</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="about-body">
                <img class="about-icon" src=ICON alt="" width="96" height="96" />
                <p class="about-name">"JStream"</p>
                <p class="about-tagline">"Live streaming and recording studio"</p>
                <dl class="about-facts">
                    <dt>"Version"</dt>
                    <dd class="mono">{VERSION}</dd>
                    <dt>"Build"</dt>
                    <dd class="mono">
                        {match commit_url {
                            Some(url) => view! {
                                <a href=url target="_blank" rel="noopener noreferrer">{build}</a>
                            }.into_any(),
                            None => build.into_any(),
                        }}
                    </dd>
                    <dt>"Protocol"</dt>
                    <dd class="mono">{move || protocol.get()}</dd>
                    <dt>"License"</dt>
                    <dd>
                        {external(Link::LicenseMit, "")}
                        " or "
                        {external(Link::LicenseApache, "")}
                    </dd>
                </dl>
                <div class="about-links">
                    {external(Link::Source, "about-link")}
                    {external(Link::Documentation, "about-link")}
                    {external(Link::ReportIssue, "about-link")}
                </div>
            </div>
            <div class="modal-footer">
                <button type="button" class="primary" data-primary on:click=move |_| close()>
                    "Close"
                </button>
            </div>
        </Modal>
    }
}

/// The Help menu's entries for one link, each a menu item that opens it.
pub fn menu_link(link: Link, done: impl Fn() + Copy + 'static) -> impl IntoView {
    view! {
        <a
            class="menuitem"
            role="menuitem"
            href=link.url()
            target="_blank"
            rel="noopener noreferrer"
            on:click=move |_| done()
        >
            {link.label()}
        </a>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_is_a_page_of_the_repository() {
        assert_eq!(REPOSITORY, "https://github.com/jeffmhopkins/JStream");
        for link in [
            Link::Documentation,
            Link::GettingStarted,
            Link::Releases,
            Link::ReportIssue,
            Link::Source,
            Link::LicenseMit,
            Link::LicenseApache,
        ] {
            assert!(link.url().starts_with(REPOSITORY), "{link:?}");
        }
        assert_eq!(Link::Source.url(), REPOSITORY);
        assert_eq!(
            Link::ReportIssue.url(),
            "https://github.com/jeffmhopkins/JStream/issues/new"
        );
    }

    #[test]
    fn the_build_is_the_short_commit_or_a_local_build() {
        assert_eq!(build_label(Some("0123456789abcdef")), "0123456");
        assert_eq!(build_label(Some("  ")), "local build");
        assert_eq!(build_label(None), "local build");
    }

    #[test]
    fn the_version_is_the_release_version() {
        assert!(VERSION.starts_with("0."), "{VERSION}");
        assert_ne!(VERSION, "0.0.0", "jstream-ui takes the workspace's version");
    }
}
