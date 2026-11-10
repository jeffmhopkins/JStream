//! Host detection: whether the page runs inside a host that hands it the engine, as the
//! desktop app does.
//!
//! The desktop app (`apps/jstream-desktop`) runs a script before the UI's own that
//! defines [`DESKTOP_GLOBAL`] as a frozen `{ url, token, vcam, collection, network,
//! networkOpen, browse, allowNetwork, jtunnel, setJtunnel, ptz, setPtz }`: its in-process
//! engine's WebSocket URL and control token,
//! whether its virtual camera can be used, the data folder `collection` asset paths are
//! relative to, whether "Allow devices on my network" was on at start and its listener
//! opened, `browse`, which
//! opens a native file dialog, and `allowNetwork`, which saves that setting; `jtunnel` and
//! `ptz` are the JTunnel and PTZ controller settings as the app read them, and
//! `setJtunnel` and `setPtz` save them. In a browser the global is absent and the UI shows
//! the connect form.
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

/// The desktop app's global, when this page runs in it: the engine, and the data
/// folder `collection` asset paths are relative to.
pub struct DesktopHost {
    /// The in-process engine.
    pub engine: HostEngine,
    /// The collection file's folder, when the app named one.
    pub collection: Option<String>,
}

/// The engine URL the open session connected to, provided by [`crate::App`] so a Browse
/// button can tell the desktop app's own engine from another one.
#[derive(Clone, Copy)]
pub struct ConnectedEngine(pub leptos::prelude::RwSignal<String>);

/// The desktop app's global, if the page runs in it.
pub fn desktop_host() -> Option<DesktopHost> {
    let global = global()?;
    let field = |name: &str| {
        Reflect::get(&global, &JsValue::from_str(name))
            .ok()?
            .as_string()
    };
    let engine = engine(field("url"), field("token"))?;
    let collection = field("collection").filter(|path| !path.is_empty());
    Some(DesktopHost { engine, collection })
}

/// The engine the desktop app hands the page, if the page runs in it.
pub fn desktop() -> Option<HostEngine> {
    desktop_host().map(|host| host.engine)
}

/// Whether the global has a `browse` function, so a Browse button has something to call.
pub fn has_browse() -> bool {
    let Some(global) = global() else {
        return false;
    };
    Reflect::get(&global, &JsValue::from_str("browse"))
        .ok()
        .is_some_and(|value| value.is_function())
}

/// Whether `session` is the desktop app's own engine. A different URL is another
/// machine, or another engine, and a native dialog would name files the engine cannot
/// see.
pub fn same_engine(desktop_url: &str, session_url: &str) -> bool {
    !desktop_url.is_empty() && desktop_url == session_url
}

/// Whether the page runs on macOS, by its user agent: a focus-only wheel chord's note
/// names macOS's Accessibility permission only there.
pub fn on_macos() -> bool {
    Reflect::get(&JsValue::from(window()), &JsValue::from_str("navigator"))
        .ok()
        .and_then(|navigator| Reflect::get(&navigator, &JsValue::from_str("userAgent")).ok())
        .and_then(|agent| agent.as_string())
        .is_some_and(|agent| agent.contains("Macintosh"))
}

/// What the desktop app says about its virtual camera (the global's `vcam`), if the
/// page runs in it: `registered`, `not_registered`, `outdated` or `off`.
pub fn desktop_vcam() -> Option<String> {
    let global = global()?;
    Reflect::get(&global, &JsValue::from_str("vcam"))
        .ok()?
        .as_string()
}

/// The desktop app's "Allow devices on my network" setting as it was when the app
/// started (ADR-0104), if the page runs in an app that has it.
pub fn desktop_network() -> Option<bool> {
    let global = global()?;
    Reflect::get(&global, &JsValue::from_str("allowNetwork"))
        .ok()
        .filter(JsValue::is_function)?;
    Reflect::get(&global, &JsValue::from_str("network"))
        .ok()?
        .as_bool()
}

/// Whether the desktop app said, at its start, that wheel chords are heard only while
/// its window has the focus: macOS without the Accessibility permission, Wayland (whose
/// GlobalShortcuts portal takes keys, not the wheel), or no hotkey listener. `false` in
/// a browser, and from an app that doesn't say.
pub fn desktop_wheel_focus_only() -> bool {
    global()
        .and_then(|global| Reflect::get(&global, &JsValue::from_str("wheelFocusOnly")).ok())
        .and_then(|value| value.as_bool())
        == Some(true)
}

/// Whether the desktop app's network listener opened at start: `false` when the
/// setting was on but its port was taken, and when the setting was off.
pub fn desktop_network_open() -> bool {
    global()
        .and_then(|global| Reflect::get(&global, &JsValue::from_str("networkOpen")).ok())
        .and_then(|value| value.as_bool())
        == Some(true)
}

/// The desktop app's "Receive from JTunnel senders" setting as it was when the app
/// started (ADR-0123 decisions 1 and 3), if the page runs in an app that has it.
pub fn desktop_jtunnel() -> Option<JtunnelSetting> {
    let global = global()?;
    Reflect::get(&global, &JsValue::from_str("setJtunnel"))
        .ok()
        .filter(JsValue::is_function)?;
    let value = Reflect::get(&global, &JsValue::from_str("jtunnel")).ok()?;
    let text = leptos::web_sys::js_sys::JSON::stringify(&value)
        .ok()?
        .as_string()?;
    Some(JtunnelSetting::parse(&text))
}

/// "Receive from JTunnel senders": whether it is on, and the senders this machine
/// accepts, each a tunnel name and the address and port it sends from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JtunnelSetting {
    /// Whether receiving is on.
    pub on: bool,
    /// The accepted senders: `(name, address)`.
    pub accepted: Vec<(String, String)>,
}

impl JtunnelSetting {
    /// The setting from the desktop global's JSON; what it can't read is left out.
    pub fn parse(text: &str) -> Self {
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let accepted = value
            .get("accepted")
            .and_then(serde_json::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|entry| {
                        let name = entry.get("name")?.as_str()?;
                        let address = entry.get("address")?.as_str()?;
                        Some((name.to_owned(), address.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            on: value.get("on").and_then(serde_json::Value::as_bool) == Some(true),
            accepted,
        }
    }

    /// The setting as the desktop app's `setJtunnel` takes it.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "on": self.on,
            "accepted": self
                .accepted
                .iter()
                .map(|(name, address)| serde_json::json!({ "name": name, "address": address }))
                .collect::<Vec<_>>(),
        })
        .to_string()
    }
}

/// Asks the desktop app to save "Receive from JTunnel senders", which applies when it
/// next starts. Resolves to `None` when it was saved, or why it wasn't.
pub async fn set_jtunnel(setting: &JtunnelSetting) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        set_jtunnel_js(setting).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = setting;
        Some("only the JStream app can save this".to_owned())
    }
}

/// Calls `setJtunnel` on the desktop global and awaits its promise.
#[cfg(target_arch = "wasm32")]
async fn set_jtunnel_js(setting: &JtunnelSetting) -> Option<String> {
    save_setting_js("setJtunnel", &setting.to_json()).await
}

/// Calls the desktop global's setting saver `name` with the setting `json`, and awaits
/// its promise: `None` when it was saved, or why it wasn't.
#[cfg(target_arch = "wasm32")]
async fn save_setting_js(name: &str, json: &str) -> Option<String> {
    use leptos::wasm_bindgen::JsCast;
    use leptos::web_sys::js_sys::{Function, JSON, Promise};

    let failed = || Some("The setting could not be saved.".to_owned());
    let Some(global) = global() else {
        return failed();
    };
    let Some(function) = Reflect::get(&global, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
    else {
        return failed();
    };
    let Ok(argument) = JSON::parse(json) else {
        return failed();
    };
    let Some(promise) = function
        .call1(&global, &argument)
        .ok()
        .and_then(|value| value.dyn_into::<Promise>().ok())
    else {
        return failed();
    };
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(value) if value.is_null() => None,
        Ok(value) => value.as_string().or_else(failed),
        Err(_) => failed(),
    }
}

/// The desktop app's "Accept a PTZ controller" setting as it was when the app started
/// (ADR-0153 §3), if the page runs in an app that has it.
pub fn desktop_ptz() -> Option<PtzSetting> {
    let global = global()?;
    Reflect::get(&global, &JsValue::from_str("setPtz"))
        .ok()
        .filter(JsValue::is_function)?;
    let value = Reflect::get(&global, &JsValue::from_str("ptz")).ok()?;
    let text = leptos::web_sys::js_sys::JSON::stringify(&value)
        .ok()?
        .as_string()?;
    Some(PtzSetting::parse(&text))
}

/// One PTZ controller endpoint of [`PtzSetting`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtzEndpoint {
    /// The address and port it listens on, as `192.168.1.20:52381`.
    pub address: String,
    /// `visca_ip`, `udp` or `tcp`.
    pub transport: String,
    /// The VISCA address it answers as, 1 to 7.
    pub visca: u8,
    /// `program`, `preview` or `scene:<scene id>`.
    pub target: String,
}

/// "Accept a PTZ controller": whether the endpoints open, the endpoints, and the
/// controller addresses they answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtzSetting {
    /// Whether the endpoints open.
    pub on: bool,
    /// The endpoints, in the order the person added them.
    pub endpoints: Vec<PtzEndpoint>,
    /// The controller addresses allowed.
    pub allow: Vec<String>,
}

impl PtzSetting {
    /// The setting from the desktop global's JSON; what it can't read is left out.
    pub fn parse(text: &str) -> Self {
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let endpoints = value
            .get("endpoints")
            .and_then(serde_json::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|entry| {
                        Some(PtzEndpoint {
                            address: entry.get("address")?.as_str()?.to_owned(),
                            transport: entry.get("transport")?.as_str()?.to_owned(),
                            visca: u8::try_from(entry.get("visca")?.as_u64()?).ok()?,
                            target: entry.get("target")?.as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let allow = value
            .get("allow")
            .and_then(serde_json::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|ip| ip.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            on: value.get("on").and_then(serde_json::Value::as_bool) == Some(true),
            endpoints,
            allow,
        }
    }

    /// The setting as the desktop app's `setPtz` takes it.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "on": self.on,
            "endpoints": self
                .endpoints
                .iter()
                .map(|e| serde_json::json!({
                    "address": e.address,
                    "transport": e.transport,
                    "visca": e.visca,
                    "target": e.target,
                }))
                .collect::<Vec<_>>(),
            "allow": self.allow,
        })
        .to_string()
    }
}

/// Asks the desktop app to save "Accept a PTZ controller", which applies when it next
/// starts. Resolves to `None` when it was saved, or why it wasn't.
pub async fn set_ptz(setting: &PtzSetting) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        save_setting_js("setPtz", &setting.to_json()).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = setting;
        Some("only the JStream app can save this".to_owned())
    }
}

/// Asks the desktop app to save "Allow devices on my network", which applies when it
/// next starts. Resolves to whether it was saved.
pub async fn allow_network(allow: bool) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        allow_network_js(allow).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = allow;
        false
    }
}

/// Calls `allowNetwork` on the desktop global and awaits its promise.
#[cfg(target_arch = "wasm32")]
async fn allow_network_js(allow: bool) -> bool {
    use leptos::wasm_bindgen::JsCast;
    use leptos::web_sys::js_sys::{Function, Promise};

    let Some(global) = global() else {
        return false;
    };
    let Some(function) = Reflect::get(&global, &JsValue::from_str("allowNetwork"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
    else {
        return false;
    };
    let Some(promise) = function
        .call1(&global, &JsValue::from_bool(allow))
        .ok()
        .and_then(|value| value.dyn_into::<Promise>().ok())
    else {
        return false;
    };
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .ok()
        .and_then(|value| value.as_bool())
        == Some(true)
}

/// Asks the desktop app to open a dialog. `kind` is `open`, `save` or `folder`.
/// `accept` is the extensions, without dots. Resolves to the path the person picked,
/// or nothing if they cancelled, the page has no `browse`, or the call failed.
pub async fn browse(
    kind: &str,
    accept: &[String],
    title: &str,
    name: &str,
    folder: &str,
) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        browse_js(kind, accept, title, name, folder).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (kind, accept, title, name, folder);
        None
    }
}

/// Calls `browse` on the desktop global and awaits its promise.
#[cfg(target_arch = "wasm32")]
async fn browse_js(
    kind: &str,
    accept: &[String],
    title: &str,
    name: &str,
    folder: &str,
) -> Option<String> {
    use leptos::wasm_bindgen::JsCast;
    use leptos::web_sys::js_sys::{Array, Function, Object, Promise, Reflect};

    let global = global()?;
    let function = Reflect::get(&global, &JsValue::from_str("browse"))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    let request = Object::new();
    let set = |key: &str, value: JsValue| {
        let _ = Reflect::set(&request, &JsValue::from_str(key), &value);
    };
    set("kind", JsValue::from_str(kind));
    let list = Array::new();
    for ext in accept {
        list.push(&JsValue::from_str(ext));
    }
    set("accept", list.into());
    set("title", JsValue::from_str(title));
    set("name", JsValue::from_str(name));
    set("folder", JsValue::from_str(folder));
    let result = function.call1(&JsValue::UNDEFINED, &request).ok()?;
    let promise = result.dyn_into::<Promise>().ok()?;
    let value = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    value.as_string().filter(|path| !path.is_empty())
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

    #[test]
    fn the_same_engine_is_the_same_url() {
        assert!(super::same_engine(
            "ws://127.0.0.1:1/api/v1/ws",
            "ws://127.0.0.1:1/api/v1/ws"
        ));
        assert!(!super::same_engine(
            "ws://127.0.0.1:1/api/v1/ws",
            "ws://127.0.0.1:2/api/v1/ws"
        ));
        assert!(!super::same_engine("", "ws://127.0.0.1:1/api/v1/ws"));
        assert!(!super::same_engine("ws://127.0.0.1:1/api/v1/ws", ""));
    }
}
