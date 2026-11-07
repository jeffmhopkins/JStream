//! The application Settings dialog (docs/UI-FEATURES.md#header): one dialog for the
//! settings that belong to the application rather than to a scene or a selected item,
//! with its pages listed on the left, as OBS's Settings has them.
//!
//! - **General** holds "Check for updates at launch" (`updates.rs`).
//! - **Plugins** is the WASM automation plugins' list, install and permissions
//!   (`plugins.rs`).
//! - **Audio plugins** is where the engine looks for CLAP audio effects, what it found
//!   and Rescan (`clap_plugins.rs`).
//! - **Devices** pairs remote browsers and lists the paired devices (`pairing.rs`),
//!   when a server in front of the engine pairs them.
//!
//! The shell provides [`SettingsDialog`] as a context, so a menu item or a notice can
//! open the dialog on the page it is about. The dialog shows one page at a time; a page
//! reads what it shows each time it is opened.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::clap_plugins::ClapPlugins;
use crate::dialog::Modal;
use crate::pairing::Pairing;
use crate::plugins::Plugins;
use crate::updates::LaunchCheckSetting;
use leptos::prelude::*;

/// A page of the Settings dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    /// Settings that fit no other page: the launch update check.
    General,
    /// The WASM automation plugins.
    Plugins,
    /// The CLAP audio plugins the engine scans for.
    AudioPlugins,
    /// Pairing remote browsers, and the paired devices.
    Devices,
}

impl SettingsPage {
    /// Every page, in the order the dialog lists them.
    pub const ALL: [SettingsPage; 4] = [
        SettingsPage::General,
        SettingsPage::Plugins,
        SettingsPage::AudioPlugins,
        SettingsPage::Devices,
    ];

    /// The page's name in the list and over the page.
    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Plugins => "Plugins",
            SettingsPage::AudioPlugins => "Audio plugins",
            SettingsPage::Devices => "Devices",
        }
    }

    /// A stable key for the page, for tests and styles.
    pub fn key(self) -> &'static str {
        match self {
            SettingsPage::General => "general",
            SettingsPage::Plugins => "plugins",
            SettingsPage::AudioPlugins => "audio-plugins",
            SettingsPage::Devices => "devices",
        }
    }

    /// The page before or after this one in the list, wrapping at either end.
    pub fn step(self, forward: bool) -> SettingsPage {
        let n = Self::ALL.len();
        let at = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        Self::ALL[next]
    }
}

/// Whether the Settings dialog is open, and on which page. The shell provides it.
#[derive(Clone, Copy)]
pub struct SettingsDialog(pub RwSignal<Option<SettingsPage>>);

impl SettingsDialog {
    /// A closed dialog.
    pub fn new() -> Self {
        Self(RwSignal::new(None))
    }

    /// Opens the dialog on `page`, or moves an open one there.
    pub fn open(self, page: SettingsPage) {
        self.0.set(Some(page));
    }

    /// Closes the dialog.
    pub fn close(self) {
        self.0.set(None);
    }
}

impl Default for SettingsDialog {
    fn default() -> Self {
        Self::new()
    }
}

/// The Settings dialog, drawn while the shell's [`SettingsDialog`] is open.
#[component]
pub fn Settings() -> impl IntoView {
    let Some(dialog) = use_context::<SettingsDialog>() else {
        return ().into_any();
    };
    // Open or closed, not which page: a page change keeps the window, and its place.
    let open = Memo::new(move |_| dialog.0.with(Option::is_some));
    view! {
        <Show when=move || open.get()>
            <SettingsWindow dialog=dialog />
        </Show>
    }
    .into_any()
}

/// The open dialog: the page list and the page shown.
#[component]
fn SettingsWindow(dialog: SettingsDialog) -> impl IntoView {
    let close = move || dialog.close();
    let page = move || dialog.0.get().unwrap_or(SettingsPage::General);
    let tab_keys = move |event: leptos::ev::KeyboardEvent| {
        let forward = match event.key().as_str() {
            "ArrowDown" => true,
            "ArrowUp" => false,
            _ => return,
        };
        event.prevent_default();
        let next = page().step(forward);
        dialog.open(next);
        focus_tab(next);
    };
    view! {
        <Modal label="Settings" class="settings-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Settings"</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="settings-main">
                <nav class="settings-pages" role="tablist" aria-orientation="vertical" aria-label="Settings pages" on:keydown=tab_keys>
                    {SettingsPage::ALL.into_iter().map(|p| {
                        let current = move || page() == p;
                        view! {
                            <button
                                type="button"
                                role="tab"
                                class="settings-page-tab"
                                class:on=current
                                id=format!("settings-tab-{}", p.key())
                                data-page=p.key()
                                aria-selected=move || current().to_string()
                                aria-controls="settings-page"
                                tabindex=move || if current() { "0" } else { "-1" }
                                on:click=move |_| dialog.open(p)
                            >
                                {p.title()}
                            </button>
                        }
                    }).collect_view()}
                </nav>
                <div
                    class="settings-body"
                    id="settings-page"
                    role="tabpanel"
                    aria-labelledby=move || format!("settings-tab-{}", page().key())
                    data-page=move || page().key()
                >
                    <h3 class="settings-page-title">{move || page().title()}</h3>
                    {move || match page() {
                        SettingsPage::General => view! { <LaunchCheckSetting /> }.into_any(),
                        SettingsPage::Plugins => view! { <Plugins /> }.into_any(),
                        SettingsPage::AudioPlugins => view! { <ClapPlugins /> }.into_any(),
                        SettingsPage::Devices => view! { <Pairing /> }.into_any(),
                    }}
                </div>
            </div>
            <div class="modal-footer">
                <button type="button" class="primary" data-primary on:click=move |_| close()>"Close"</button>
            </div>
        </Modal>
    }
}

/// Moves the focus to `page`'s tab, as arrow keys in a tab list do.
fn focus_tab(page: SettingsPage) {
    use leptos::wasm_bindgen::JsCast;
    let tab = document()
        .get_element_by_id(&format!("settings-tab-{}", page.key()))
        .and_then(|e| e.dyn_into::<leptos::web_sys::HtmlElement>().ok());
    if let Some(tab) = tab {
        let _ = tab.focus();
    }
}

#[cfg(test)]
mod tests {
    use super::SettingsPage;

    #[test]
    fn the_pages_step_in_order_and_wrap() {
        assert_eq!(SettingsPage::General.step(true), SettingsPage::Plugins);
        assert_eq!(SettingsPage::Plugins.step(true), SettingsPage::AudioPlugins);
        assert_eq!(SettingsPage::Devices.step(true), SettingsPage::General);
        assert_eq!(SettingsPage::General.step(false), SettingsPage::Devices);
        assert_eq!(SettingsPage::Plugins.step(false), SettingsPage::General);
    }

    #[test]
    fn each_page_has_its_own_key_and_title() {
        let keys: std::collections::BTreeSet<_> =
            SettingsPage::ALL.iter().map(|p| p.key()).collect();
        assert_eq!(keys.len(), SettingsPage::ALL.len());
        assert_eq!(SettingsPage::Plugins.title(), "Plugins");
    }
}
