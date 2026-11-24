//! The browser's own right-click menu and text selection, kept off the UI's chrome.
//!
//! A right-click that no element answers with its own menu ([`crate::menu::Menus`]) is
//! swallowed, so the browser's generic menu (Back, Reload, Inspect) never opens over the
//! app. A text field, and readable text marked [`TEXT`], keeps the browser's menu: there
//! its Cut, Copy and Paste are the menu that fits. [`Menus::open`](crate::menu::Menus::open)
//! leaves those to the browser too, so a panel's menu never opens over its search field.
//!
//! Text selection follows the same line. The style sheet turns `user-select` off on the
//! page and back on for [`TEXT`], and [`install`] stops a selection that starts anywhere
//! else, so a click, a double-click or a drag across panels selects nothing. A press on
//! the chrome also clears a selection left from before, which a drag that prevents its
//! default would otherwise keep highlighted.
//!
//! Docs: docs/UI-FEATURES.md#menus

use leptos::ev;
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;
use leptos::web_sys::{Element, EventTarget, Node};

/// What keeps the browser's own right-click menu and text selection: text fields (not
/// checkboxes, sliders or buttons), editable text, code, and readable text worth copying:
/// an error (`.problem`, the problem bar's message), the connection details, the agent's
/// activity log, and anything the markup marks `.selectable`. The style sheet's
/// `user-select: text` rule lists the same.
pub const TEXT: &str = "input:not([type=checkbox]):not([type=radio]):not([type=range])\
    :not([type=button]):not([type=submit]):not([type=reset]):not([type=color])\
    :not([type=file]):not([type=image]), textarea, [contenteditable]:not([contenteditable=false]), \
    pre, code, .problem, .problem-bar > span, .connection dd, .activity-log, .selectable";

/// Whether an event on `target` is on text that keeps the browser's own menu and
/// selection ([`TEXT`]). A text node counts as its parent element.
pub fn in_text(target: Option<EventTarget>) -> bool {
    let Some(target) = target else {
        return false;
    };
    let element = match target.dyn_into::<Element>() {
        Ok(element) => Some(element),
        Err(target) => target
            .dyn_into::<Node>()
            .ok()
            .and_then(|node| node.parent_element()),
    };
    element.is_some_and(|element| element.closest(TEXT).ok().flatten().is_some())
}

/// Swallows a right-click unless it is on text ([`in_text`]): for an element that keeps
/// a right-click from reaching a menu behind it, so it doesn't reach the browser either.
pub fn swallow(event: &ev::MouseEvent) {
    if !in_text(event.target()) {
        event.prevent_default();
    }
}

/// Installs the window's listeners for the life of the app.
pub fn install() {
    // Bubbling: a menu the target or a panel opened has prevented the default by now.
    let menus = window_event_listener(ev::contextmenu, |event| {
        if !event.default_prevented() {
            swallow(&event);
        }
    });
    let selects = window_event_listener(ev::selectstart, |event| {
        if !in_text(event.target()) {
            event.prevent_default();
        }
    });
    // Capturing, so a drag that stops its press from propagating still clears it.
    let callback = leptos::wasm_bindgen::closure::Closure::<dyn FnMut(ev::PointerEvent)>::new(
        |event: ev::PointerEvent| {
            if event.button() == 0 && !in_text(event.target()) {
                clear_selection();
            }
        },
    )
    .into_js_value();
    let options = leptos::web_sys::AddEventListenerOptions::new();
    options.set_capture(true);
    let _ = window().add_event_listener_with_callback_and_add_event_listener_options(
        "pointerdown",
        callback.unchecked_ref(),
        &options,
    );
    on_cleanup(move || {
        menus.remove();
        selects.remove();
        let _ = window().remove_event_listener_with_callback_and_bool(
            "pointerdown",
            callback.unchecked_ref(),
            true,
        );
    });
}

/// Clears the page's text selection, unless a text field has the focus (its own
/// selection is the one in use).
fn clear_selection() {
    let focused_text = document()
        .active_element()
        .is_some_and(|element| element.closest(TEXT).ok().flatten().is_some());
    if focused_text {
        return;
    }
    if let Ok(Some(selection)) = window().get_selection()
        && !selection.is_collapsed()
    {
        let _ = selection.remove_all_ranges();
    }
}
