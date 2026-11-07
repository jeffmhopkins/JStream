//! Help › Check for updates, and the Settings checkbox that runs it at launch
//! (docs/UI-FEATURES.md#header), on the Settings dialog's General page (`settings.rs`).
//!
//! The desktop app serves GitHub's public releases list at [`CHECK_PATH`] (its
//! `updates.rs`). This page reads that list and compares it with the version it was
//! built as. It never chooses the address: a browser that isn't the desktop app has
//! nothing to ask, and says so. Nothing is downloaded.
//!
//! "Check for updates at launch" is kept in the browser's storage and is off until the
//! user turns it on. When it is on, the desktop app's main window checks once each time
//! it opens and shows a notice only when a newer release exists. A failure stays quiet
//! there; the menu item is what reports it.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::about::{REPOSITORY, VERSION};
use crate::dialog::Modal;
use crate::host;
use crate::ui_settings::{self, GLOBAL_HOTKEYS_KEY, Kept, global_hotkeys_on};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::cmp::Ordering;
use std::fmt;

/// The path the desktop app serves the releases list on. The same path is named in
/// `apps/jstream-desktop`'s `updates.rs`.
pub const CHECK_PATH: &str = "/jstream-update-check";

/// Where the browser keeps "Check for updates at launch". Absent means off.
const LAUNCH_KEY: &str = "jstream.check-updates-at-launch";

/// Set once a launch check has started, so opening the workspace again in the same
/// window doesn't ask a second time. A new launch of the app has a new session.
const LAUNCH_ONCE: &str = "jstream.update-launch-checked";

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// Nothing newer than the running version is published.
    Current,
    /// `version` is newer. `url` is its release page, a page of [`REPOSITORY`].
    Available {
        /// The newer version, without a leading `v`.
        version: String,
        /// Its release page.
        url: String,
    },
    /// The check couldn't be made, in words the menu shows.
    Failed(String),
}

/// A newer release the launch notice can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The newer version, without a leading `v`.
    pub version: String,
    /// Its release page.
    pub url: String,
}

/// Whether a launch check should run: the desktop app, the setting on, and not already
/// started in this window.
pub fn should_launch(in_desktop: bool, setting_on: bool, already: bool) -> bool {
    in_desktop && setting_on && !already
}

/// The release a launch check shows, when it shows one: only a newer version, and only
/// from a check that was allowed to run. A failure and an up-to-date result show nothing.
pub fn launch_notice(allowed: bool, report: &Report) -> Option<Release> {
    if !allowed {
        return None;
    }
    match report {
        Report::Available { version, url } => Some(Release {
            version: version.clone(),
            url: url.clone(),
        }),
        Report::Current | Report::Failed(_) => None,
    }
}

/// Reads `body`, GitHub's releases list or `{"error": "…"}` from the desktop app, against
/// `running` (the version this UI was built as).
pub fn report(running: &str, body: &str) -> Report {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Report::Failed("GitHub's reply couldn't be read.".into());
    };
    if let Some(message) = value.get("error").and_then(Value::as_str) {
        let message = message.trim();
        if message.is_empty() {
            return Report::Failed("GitHub's reply couldn't be read.".into());
        }
        return Report::Failed(message.to_owned());
    }
    let Some(list) = value.as_array() else {
        return Report::Failed("GitHub's reply couldn't be read.".into());
    };
    let Some(running) = Semver::parse(running) else {
        return Report::Failed("This build's version couldn't be compared.".into());
    };
    let mut newest: Option<Semver> = None;
    for item in list {
        if item.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(tag) = item.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(version) = Semver::parse(tag.strip_prefix('v').unwrap_or(tag)) else {
            continue;
        };
        newest = Some(match newest {
            Some(best) if best >= version => best,
            _ => version,
        });
    }
    match newest {
        Some(latest) if latest > running => {
            let version = latest.to_string();
            let url = format!("{REPOSITORY}/releases/tag/v{version}");
            Report::Available { version, url }
        }
        _ => Report::Current,
    }
}

/// A semantic version, pre-release included and build metadata ignored
/// ([SemVer](https://semver.org/spec/v2.0.0.html)).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Semver {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Ident>,
}

/// One pre-release identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ident {
    /// Digits, compared as a number. A number sorts before a word.
    Num(u64),
    /// Letters, digits and hyphens, compared by ASCII order.
    Text(String),
}

impl Semver {
    /// `text` as a version, or `None` when it isn't one. A leading `v` is not accepted
    /// here; the caller strips the tag's.
    fn parse(text: &str) -> Option<Self> {
        let (core, rest) = text.split_once('+').unwrap_or((text, ""));
        let _ = rest;
        let (numbers, pre) = match core.split_once('-') {
            Some((numbers, pre)) => (numbers, Some(pre)),
            None => (core, None),
        };
        let mut parts = numbers.split('.');
        let major = numeric(parts.next()?)?;
        let minor = numeric(parts.next()?)?;
        let patch = numeric(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some("") => return None,
            Some(pre) => pre.split('.').map(Ident::parse).collect::<Option<_>>()?,
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl PartialOrd for Semver {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Semver {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| cmp_pre(&self.pre, &other.pre))
    }
}

impl fmt::Display for Semver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-")?;
            for (i, ident) in self.pre.iter().enumerate() {
                if i > 0 {
                    write!(f, ".")?;
                }
                write!(f, "{ident}")?;
            }
        }
        Ok(())
    }
}

impl Ident {
    fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        if text.bytes().all(|b| b.is_ascii_digit()) {
            // A number with a leading zero is not a numeric identifier.
            if text.len() > 1 && text.starts_with('0') {
                return None;
            }
            return Some(Ident::Num(text.parse().ok()?));
        }
        Some(Ident::Text(text.to_owned()))
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
            (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
        }
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ident::Num(n) => write!(f, "{n}"),
            Ident::Text(text) => f.write_str(text),
        }
    }
}

/// Pre-release precedence: a release sorts after its pre-releases, identifiers compare
/// in order, and a longer list sorts after a shorter one that matches up to there.
fn cmp_pre(a: &[Ident], b: &[Ident]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (left, right) in a.iter().zip(b.iter()) {
                let order = left.cmp(right);
                if order != Ordering::Equal {
                    return order;
                }
            }
            a.len().cmp(&b.len())
        }
    }
}

/// A dotted number with no leading zero, except `0` itself.
fn numeric(text: &str) -> Option<u64> {
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Whether the browser kept the launch check on. Storage can be off; it is then off.
pub(crate) fn kept_launch_check() -> bool {
    storage_item(LAUNCH_KEY).is_some_and(|value| value == "on")
}

/// Keeps the launch check in the browser's storage, when it has storage.
fn keep_launch_check(on: bool) {
    set_storage(LAUNCH_KEY, if on { "on" } else { "off" });
}

/// Whether this window already started its launch check.
fn launch_started() -> bool {
    session_item(LAUNCH_ONCE).is_some()
}

/// Records that this window has started its launch check.
fn mark_launch_started() {
    set_session(LAUNCH_ONCE, "1");
}

fn storage_item(key: &str) -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(key).ok().flatten())
}

fn set_storage(key: &str, value: &str) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(key, value);
    }
}

fn session_item(key: &str) -> Option<String> {
    window()
        .session_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(key).ok().flatten())
}

fn set_session(key: &str, value: &str) {
    if let Ok(Some(storage)) = window().session_storage() {
        let _ = storage.set_item(key, value);
    }
}

/// Asks the desktop app, or says the page isn't one. A failure of the request itself
/// is the offline message; the menu shows whatever comes back.
pub async fn check() -> Report {
    if host::desktop().is_none() {
        return Report::Failed("Check for updates runs in the desktop app.".into());
    }
    #[cfg(target_arch = "wasm32")]
    {
        match fetch_text(CHECK_PATH).await {
            Ok(text) => report(VERSION, &text),
            Err(message) => Report::Failed(message),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = CHECK_PATH;
        Report::Failed("Check for updates runs in the desktop app.".into())
    }
}

/// `GET` of `url` on this page's origin, with no cookies and no referrer.
#[cfg(target_arch = "wasm32")]
async fn fetch_text(url: &str) -> Result<String, String> {
    use leptos::wasm_bindgen::JsCast;
    use leptos::wasm_bindgen::JsValue;
    use leptos::web_sys::js_sys::{Function, Object, Promise, Reflect};

    let offline = || "Couldn't reach GitHub. Check the connection and try again.".to_owned();
    let unread = || "GitHub's reply couldn't be read.".to_owned();
    let window = window();
    let win = JsValue::from(window.clone());
    let fetch = Reflect::get(&win, &JsValue::from_str("fetch"))
        .map_err(|_| offline())?
        .dyn_into::<Function>()
        .map_err(|_| offline())?;
    let init = Object::new();
    for (key, value) in [
        ("method", "GET"),
        ("credentials", "omit"),
        ("cache", "no-store"),
        ("referrer", "no-referrer"),
    ] {
        let set = Reflect::set(&init, &JsValue::from_str(key), &JsValue::from_str(value))
            .map_err(|_| offline())?;
        if !set {
            return Err(offline());
        }
    }
    let promise = fetch
        .call2(&win, &JsValue::from_str(url), &init)
        .map_err(|_| offline())?
        .dyn_into::<Promise>()
        .map_err(|_| offline())?;
    let response = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| offline())?;
    let text_fn = Reflect::get(&response, &JsValue::from_str("text"))
        .map_err(|_| unread())?
        .dyn_into::<Function>()
        .map_err(|_| unread())?;
    let text_promise = text_fn
        .call0(&response)
        .map_err(|_| unread())?
        .dyn_into::<Promise>()
        .map_err(|_| unread())?;
    wasm_bindgen_futures::JsFuture::from(text_promise)
        .await
        .map_err(|_| unread())?
        .as_string()
        .ok_or_else(unread)
}

/// The dialog Help › Check for updates opens. It asks as it opens.
#[component]
pub fn UpdateDialog(
    /// Closes the dialog.
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let report = RwSignal::new(None::<Report>);
    Effect::new(move |_| {
        spawn_local(async move {
            report.set(Some(check().await));
        });
    });
    view! {
        <Modal label="Check for updates" class="update-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Check for updates"</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="update-body">
                {move || match report.get() {
                    None => view! { <p>"Checking…"</p> }.into_any(),
                    Some(Report::Current) => view! {
                        <p>"You're up to date."</p>
                        <p class="update-detail">"This is version " <span class="mono">{VERSION}</span> "."</p>
                    }.into_any(),
                    Some(Report::Available { version, url }) => view! {
                        <p>
                            "Version "
                            <a href=url target="_blank" rel="noopener noreferrer">{version}</a>
                            " is available."
                        </p>
                        <p class="update-detail">"This is version " <span class="mono">{VERSION}</span> ". Nothing is downloaded."</p>
                    }.into_any(),
                    Some(Report::Failed(message)) => view! { <p>{message}</p> }.into_any(),
                }}
            </div>
            <div class="modal-footer">
                <button type="button" class="primary" data-primary on:click=move |_| close()>"Close"</button>
            </div>
        </Modal>
    }
}

/// The Settings dialog's General page (`settings.rs`): "Check for updates at launch" and,
/// in the desktop app, whether hotkeys are heard while another application has the focus.
#[component]
pub fn LaunchCheckSetting() -> impl IntoView {
    let on = RwSignal::new(kept_launch_check());
    // Global hotkeys, in the desktop app only: the server keeps the switch, which the
    // desktop app reads (ADR-0112 §6).
    let session = host::desktop().and_then(|_| {
        use_context::<crate::app::SessionContext>().and_then(|context| context.get())
    });
    let global = RwSignal::new(None::<bool>);
    if let Some(session) = &session {
        let control = session.control.clone();
        spawn_local(async move {
            if let Ok(kept) = ui_settings::get(&*control, GLOBAL_HOTKEYS_KEY).await
                && kept != Kept::Unserved
            {
                global.try_set(Some(global_hotkeys_on(&kept)));
            }
        });
    }
    let control = StoredValue::new_local(session.map(|session| session.control));
    let keep_global = move |checked: bool| {
        global.set(Some(checked));
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(error) =
                ui_settings::set(&*control, GLOBAL_HOTKEYS_KEY, Value::Bool(checked)).await
            {
                leptos::logging::warn!("the hotkeys setting could not be kept: {error}");
            }
        });
    };
    view! {
        <label class="settings-check">
            <input
                type="checkbox"
                aria-label="Check for updates at launch"
                prop:checked=move || on.get()
                on:change=move |event| {
                    let checked = event_target_checked(&event);
                    on.set(checked);
                    keep_launch_check(checked);
                }
            />
            <span>
                <span class="settings-check-label">"Check for updates at launch"</span>
                <span class="settings-check-note">
                    "When on, the desktop app looks once at start and shows a notice only when a newer release exists. Nothing is downloaded. Off until you turn it on."
                </span>
            </span>
        </label>
        <Show when=move || global.get().is_some()>
            <label class="settings-check">
                <input
                    type="checkbox"
                    aria-label="Hear hotkeys while another app has the focus"
                    prop:checked=move || global.get().unwrap_or(true)
                    on:change=move |event| keep_global(event_target_checked(&event))
                />
                <span>
                    <span class="settings-check-label">"Hear hotkeys while another app has the focus"</span>
                    <span class="settings-check-note">
                        "When on, the desktop app listens for the keys you've bound, and only those, while you work in another app. A key another app already holds is heard only while JStream has the focus. On until you turn it off."
                    </span>
                </span>
            </label>
        </Show>
    }
}

/// The notice under the header when a launch check found a newer release.
#[component]
pub fn LaunchNotice() -> impl IntoView {
    let found = RwSignal::new(None::<Release>);
    Effect::new(move |_| {
        let allowed = should_launch(
            host::desktop().is_some(),
            kept_launch_check(),
            launch_started(),
        );
        if !allowed {
            return;
        }
        mark_launch_started();
        spawn_local(async move {
            if let Some(release) = launch_notice(true, &check().await) {
                found.set(Some(release));
            }
        });
    });
    view! {
        {move || found.get().map(|release| view! {
            <div class="update-notice" role="status">
                <span>
                    "Version "
                    <a href=release.url.clone() target="_blank" rel="noopener noreferrer">{release.version}</a>
                    " is available."
                </span>
                <button type="button" on:click=move |_| found.set(None)>"Dismiss"</button>
            </div>
        })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(body: &str) -> (String, String) {
        match report("0.1.0-alpha.4", body) {
            Report::Available { version, url } => (version, url),
            other => panic!("expected a newer release, got {other:?}"),
        }
    }

    fn release(tag: &str, draft: bool) -> String {
        format!(r#"{{"tag_name":"{tag}","draft":{draft}}}"#)
    }

    #[test]
    fn a_newer_prerelease_is_named_and_linked() {
        let body = format!(
            "[{},{}]",
            release("v0.1.0-alpha.4", false),
            release("v0.1.0-alpha.10", false)
        );
        let (version, url) = available(&body);
        assert_eq!(version, "0.1.0-alpha.10");
        assert_eq!(
            url,
            "https://github.com/jeffmhopkins/JStream/releases/tag/v0.1.0-alpha.10"
        );
        assert!(
            url.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/:._-".contains(c)),
            "{url}"
        );
    }

    #[test]
    fn the_same_version_and_an_older_one_are_up_to_date() {
        let current = format!("[{}]", release("v0.1.0-alpha.4.2", false));
        assert_eq!(report("0.1.0-alpha.4.2", &current), Report::Current);
        let older = format!("[{}]", release("v0.1.0-alpha.4", false));
        assert_eq!(report("0.1.0-alpha.4.2", &older), Report::Current);
    }

    #[test]
    fn a_draft_is_skipped_and_a_stable_release_sorts_after_its_prereleases() {
        let body = format!(
            "[{},{},{}]",
            release("v0.1.0-alpha.9", false),
            release("v0.2.0", true),
            release("v0.1.0", false)
        );
        let (version, _) = available(&body);
        assert_eq!(version, "0.1.0", "the draft 0.2.0 is not offered");
    }

    #[test]
    fn versions_sort_by_semver_including_pre_release_identifiers() {
        let order = [
            "0.1.0-alpha",
            "0.1.0-alpha.1",
            "0.1.0-alpha.4",
            "0.1.0-alpha.4.2",
            "0.1.0-alpha.10",
            "0.1.0-beta.1",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1",
            "1.0.0",
        ];
        let parsed: Vec<_> = order.iter().map(|v| Semver::parse(v).unwrap()).collect();
        let mut sorted = parsed.clone();
        sorted.sort();
        assert_eq!(sorted, parsed);
        // A number sorts before a word, and build metadata is ignored.
        assert!(Semver::parse("1.0.0-1").unwrap() < Semver::parse("1.0.0-alpha").unwrap());
        assert_eq!(Semver::parse("1.0.0+build"), Semver::parse("1.0.0"));
        assert_eq!(Semver::parse("01.0.0"), None);
        assert_eq!(Semver::parse("1.0.0-01"), None);
        assert_eq!(Semver::parse("1.0"), None);
    }

    #[test]
    fn a_bad_reply_and_a_bad_build_version_are_errors() {
        assert_eq!(
            report("0.1.0", "not json"),
            Report::Failed("GitHub's reply couldn't be read.".into())
        );
        assert_eq!(
            report(
                "0.1.0",
                r#"{"error":"GitHub asked us to wait before checking again."}"#
            ),
            Report::Failed("GitHub asked us to wait before checking again.".into())
        );
        assert_eq!(
            report("nope", "[]"),
            Report::Failed("This build's version couldn't be compared.".into())
        );
        assert_eq!(report("0.1.0", "[]"), Report::Current);
    }

    #[test]
    fn the_launch_notice_appears_only_for_a_newer_release() {
        let newer = Report::Available {
            version: "0.1.0-alpha.5".into(),
            url: "https://github.com/jeffmhopkins/JStream/releases/tag/v0.1.0-alpha.5".into(),
        };
        assert!(should_launch(true, true, false));
        assert!(
            !should_launch(false, true, false),
            "a browser doesn't check"
        );
        assert!(!should_launch(true, false, false), "the setting starts off");
        assert!(!should_launch(true, true, true), "one check a window");
        assert_eq!(
            launch_notice(true, &newer).map(|r| r.version),
            Some("0.1.0-alpha.5".into())
        );
        assert_eq!(launch_notice(false, &newer), None);
        assert_eq!(launch_notice(true, &Report::Current), None);
        assert_eq!(launch_notice(true, &Report::Failed("offline".into())), None);
    }
}
