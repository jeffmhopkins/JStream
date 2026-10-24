//! Host detection: whether the page runs inside a host that hands it the engine, as the
//! desktop app does.
//!
//! The desktop app (`apps/jstream-desktop`) runs a script before the UI's own that
//! defines [`DESKTOP_GLOBAL`] as a frozen `{ url, token, vcam }`: its in-process
//! engine's WebSocket URL and control token, and whether its virtual camera can be
//! used. In a browser the global is absent and the UI shows the connect form.
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

/// The engine the desktop app hands the page, if the page runs in it.
pub fn desktop() -> Option<HostEngine> {
    let global = Reflect::get(&window(), &JsValue::from_str(DESKTOP_GLOBAL)).ok()?;
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
    let global = Reflect::get(&window(), &JsValue::from_str(DESKTOP_GLOBAL)).ok()?;
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
