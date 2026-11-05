//! Host detection: whether the page runs inside a host that hands it the engine, as the
//! desktop app does.
//!
//! The desktop app (`apps/jstream-desktop`) runs a script before the UI's own that
//! defines [`DESKTOP_GLOBAL`] as a frozen `{ url, token, vcam }`: its in-process
//! engine's WebSocket URL and control token, and whether its virtual camera can be
//! used. In a browser the global is absent and the UI shows the connect form.
//!
//! A projector window the app opens for `window.open` (`projector.rs`) may not run that
//! script: on Windows, WebView2 doesn't run it on a window handed to its opener. Such a
//! window reads the global from its opener instead, which the browser allows only when
//! the opener is a page of the same origin, the app's own.
//!
//! Docs: crates/jstream-ui/README.md#design

use leptos::prelude::window;
use leptos::wasm_bindgen::JsValue;
use leptos::web_sys::js_sys::Reflect;

/// The global the desktop app defines. Its bootstrap script names it too.
pub const DESKTOP_GLOBAL: &str = "__JSTREAM_DESKTOP__";

/// The engine a host hands the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEngine {
    /// The engine's WebSocket URL.
    pub url: String,
    /// The control token. Kept in memory only, like the connect form's.
    pub token: String,
}

/// The desktop app's global: this page's own, else its opener's when the opener is a
/// page of the same origin (reading a property of a cross-origin opener throws).
fn global() -> Option<JsValue> {
    let own = |target: &JsValue| {
        Reflect::get(target, &JsValue::from_str(DESKTOP_GLOBAL))
            .ok()
            .filter(JsValue::is_object)
    };
    let window = JsValue::from(window());
    own(&window).or_else(|| {
        let opener = Reflect::get(&window, &JsValue::from_str("opener"))
            .ok()
            .filter(JsValue::is_object)?;
        own(&opener)
    })
}

/// The engine the desktop app hands the page, if the page runs in it.
pub fn desktop() -> Option<HostEngine> {
    let global = global()?;
    let field = |name: &str| {
        Reflect::get(&global, &JsValue::from_str(name))
            .ok()?
            .as_string()
    };
    engine(field("url"), field("token"))
}

/// What the desktop app says about its virtual camera (the global's `vcam`), if the
/// page runs in it: `registered`, `not_registered`, `outdated` or `off`.
pub fn desktop_vcam() -> Option<String> {
    let global = global()?;
    Reflect::get(&global, &JsValue::from_str("vcam"))
        .ok()?
        .as_string()
}

/// The engine from the global's fields: both must be non-empty strings.
fn engine(url: Option<String>, token: Option<String>) -> Option<HostEngine> {
    let url = url.filter(|url| !url.is_empty())?;
    let token = token.filter(|token| !token.is_empty())?;
    Some(HostEngine { url, token })
}

#[cfg(test)]
mod tests {
    use super::{HostEngine, engine};

    #[test]
    fn both_fields_are_needed() {
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(
            engine(some("ws://127.0.0.1:1/api/v1/ws"), some("0a")),
            Some(HostEngine {
                url: "ws://127.0.0.1:1/api/v1/ws".into(),
                token: "0a".into()
            })
        );
        assert_eq!(engine(some("ws://x"), None), None);
        assert_eq!(engine(None, some("0a")), None);
        assert_eq!(engine(some(""), some("0a")), None);
        assert_eq!(engine(some("ws://x"), some("")), None);
    }
}
