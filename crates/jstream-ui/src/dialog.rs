//! The modal dialog frame: a dialog centred over a dim backdrop. A press on the
//! backdrop or Escape cancels it, and Enter presses its primary button (one marked
//! `data-primary`) unless the key was meant for a text area, a button or a select.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos::web_sys::wasm_bindgen::JsCast;

/// Whether Enter on an element of this tag presses the dialog's primary button. A text
/// area takes Enter as a new line, and a button or a select acts on it themselves.
fn enter_submits(tag: &str) -> bool {
    !matches!(
        tag.to_ascii_lowercase().as_str(),
        "textarea" | "button" | "select"
    )
}

/// A modal dialog.
#[component]
pub fn Modal(
    /// The dialog's accessible name.
    label: &'static str,
    /// Extra classes for the dialog box (its width).
    #[prop(optional)]
    class: &'static str,
    /// Cancels the dialog: the backdrop, Escape and the header's ×.
    cancel: impl Fn() + Copy + 'static,
    children: Children,
) -> impl IntoView {
    let dialog = NodeRef::<html::Div>::new();
    // Focus the dialog's first field, so Enter and Escape reach it at once.
    request_animation_frame(move || {
        if let Some(dialog) = dialog.get_untracked() {
            let first = dialog
                .query_selector("input:not([type=radio]), select, textarea")
                .ok()
                .flatten()
                .and_then(|e| e.dyn_into::<leptos::web_sys::HtmlElement>().ok());
            let _ = match first {
                Some(field) => field.focus(),
                None => dialog.focus(),
            };
        }
    });
    let on_key = move |event: ev::KeyboardEvent| match event.key().as_str() {
        "Escape" => {
            event.prevent_default();
            event.stop_propagation();
            cancel();
        }
        "Enter" => {
            let tag = event
                .target()
                .and_then(|t| t.dyn_into::<leptos::web_sys::Element>().ok())
                .map(|e| e.tag_name())
                .unwrap_or_default();
            if !enter_submits(&tag) {
                return;
            }
            let primary = dialog
                .get_untracked()
                .and_then(|d| d.query_selector("[data-primary]").ok().flatten())
                .and_then(|e| e.dyn_into::<leptos::web_sys::HtmlElement>().ok());
            if let Some(primary) = primary {
                event.prevent_default();
                // A field commits on `change`, which the prevented Enter no longer
                // fires: blurring it fires it now, so the primary button reads the
                // typed value.
                if let Some(field) = event
                    .target()
                    .and_then(|t| t.dyn_into::<leptos::web_sys::HtmlElement>().ok())
                {
                    let _ = field.blur();
                    primary.click();
                    // Back to the field, should the dialog stay open (a refused OK).
                    let _ = field.focus();
                } else {
                    primary.click();
                }
            }
        }
        _ => {}
    };
    view! {
        <div class="modal-backdrop" on:pointerdown=move |_| cancel()>
            <div
                class=format!("modal {class}")
                role="dialog"
                aria-modal="true"
                aria-label=label
                tabindex="-1"
                node_ref=dialog
                on:keydown=on_key
                on:pointerdown=|event: ev::PointerEvent| event.stop_propagation()
                on:contextmenu=|event: ev::MouseEvent| event.stop_propagation()
            >
                {children()}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::enter_submits;

    #[test]
    fn enter_presses_the_primary_button_from_fields_only() {
        assert!(enter_submits("INPUT"));
        assert!(enter_submits("div"));
        assert!(!enter_submits("TEXTAREA"));
        assert!(!enter_submits("BUTTON"));
        assert!(!enter_submits("select"));
    }
}
