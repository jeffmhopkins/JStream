//! Host detection: whether the page runs inside a host that hands it the engine, as the
//! desktop app does.
//!
//! The desktop app (`apps/jstream-desktop`) runs a script before the UI's own that
//! defines [`DESKTOP_GLOBAL`] as a frozen `{ url, token, vcam, collection, network,
//! networkOpen, browse, allowNetwork, jtunnel, setJtunnel, ptz, setPtz, ptzCameras,
//! setPtzCameras, saveScreenshot, openSaved, showSaved, setZoom }`: its in-process engine's WebSocket URL and control token,
//! whether its virtual camera can be used, the default project's folder `collection`
//! asset paths are relative to, whether "Allow devices on my network" was on at start and its listener
//! opened, `browse`, which
//! opens a native file dialog, and `allowNetwork`, which saves that setting; `jtunnel` and
//! `ptz` are the JTunnel and PTZ controller settings as the app read them, and
//! `setJtunnel` and `setPtz` save them; `ptzCameras` is the machine's PTZ camera list
//! (ADR-0209 decision 1), which `setPtzCameras` saves and the app applies at once.
//! `saveScreenshot` saves a screenshot in Downloads and
//! answers its path, and `openSaved` and `showSaved` open such a file or show it in its
//! folder (ADR-0163 §3). `setZoom` zooms the app's webview, for the UI scale
//! (`ui_scale.rs`). In a browser the global is absent and the UI shows
//! the connect form.
//!
//! A projector window the app opens for `window.open` (`projector.rs`) may not run that
//! script: on Windows, WebView2 doesn't run it on a window handed to its opener. Such a
//! window reads the global from its opener instead, which the browser allows only when
//! the opener is a page of the same origin, the app's own.
//!
//! Files dragged from the desktop onto the app's main window reach the page as
//! [`FILE_DROP_EVENT`]s on `window`, not as an HTML drop (the app handles the drag
//! natively, so the webview's own drop carries no paths). [`on_file_drop`] hands any
//! panel each step as a [`FileDrop`], and [`FileDrop::lands_on`] says whether it is
//! over a given element.
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
/// Off wasm (native unit tests) there is no page, so no desktop app.
fn global() -> Option<JsValue> {
    if cfg!(not(target_arch = "wasm32")) {
        return None;
    }
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

/// Whether the open session (`connected`) is the desktop app's own engine: false in a
/// browser, and false in the desktop app connected to another engine.
pub fn own_engine(connected: Option<ConnectedEngine>) -> bool {
    let Some(engine) = desktop() else {
        return false;
    };
    use leptos::prelude::With;
    connected.is_some_and(|connected| connected.0.with(|url| same_engine(&engine.url, url)))
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
    /// The accepted senders.
    pub accepted: Vec<JtunnelSender>,
    /// The accepted sender JStream receives from, one at a time; the others stand by.
    /// `None`, or a name not accepted, means the first accepted.
    pub live: Option<String>,
}

/// One accepted JTunnel sender.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JtunnelSender {
    /// Its tunnel name.
    pub name: String,
    /// The address and port it sends from, as `192.168.1.5:9000`.
    pub address: String,
    /// "Use this sender's state" (ADR-0171 decision 1): on unless the person turns it
    /// off, and on when the JSON leaves it out.
    pub state: bool,
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
                        let state =
                            entry.get("state").and_then(serde_json::Value::as_bool) != Some(false);
                        Some(JtunnelSender {
                            name: name.to_owned(),
                            address: address.to_owned(),
                            state,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            on: value.get("on").and_then(serde_json::Value::as_bool) == Some(true),
            accepted,
            live: value
                .get("live")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        }
    }

    /// The name of the sender JStream receives from: the one `live` names if it is
    /// accepted, else the first accepted, as the desktop app reads it.
    pub fn live_name(&self) -> Option<&str> {
        self.live
            .as_deref()
            .filter(|name| self.accepted.iter().any(|sender| sender.name == *name))
            .or_else(|| self.accepted.first().map(|sender| sender.name.as_str()))
    }

    /// The setting as the desktop app's `setJtunnel` takes it.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "on": self.on,
            "accepted": self
                .accepted
                .iter()
                .map(|sender| {
                    serde_json::json!({
                        "name": sender.name,
                        "address": sender.address,
                        "state": sender.state,
                    })
                })
                .collect::<Vec<_>>(),
            "live": self.live,
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

/// The desktop app's "Control PTZ cameras" setting as it was when the app started
/// (ADR-0209 decision 1), if the page runs in an app that has it.
pub fn desktop_ptz_cameras() -> Option<PtzCameraSetting> {
    let global = global()?;
    Reflect::get(&global, &JsValue::from_str("setPtzCameras"))
        .ok()
        .filter(JsValue::is_function)?;
    let value = Reflect::get(&global, &JsValue::from_str("ptzCameras")).ok()?;
    let text = leptos::web_sys::js_sys::JSON::stringify(&value)
        .ok()?
        .as_string()?;
    Some(PtzCameraSetting::parse(&text))
}

/// One camera of [`PtzCameraSetting`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtzCameraEntry {
    /// Its name, which shots, `ptz.*` actions and value bindings take.
    pub name: String,
    /// Its address and port, as `192.168.1.40:52381`.
    pub address: String,
    /// `visca_ip`, `udp` or `tcp`.
    pub transport: String,
    /// Its VISCA address on that link, 1 to 7.
    pub visca: u8,
}

/// "Control PTZ cameras": whether JStream drives this machine's heads, and which.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtzCameraSetting {
    /// Whether it drives them.
    pub on: bool,
    /// The cameras, in the order the person added them.
    pub cameras: Vec<PtzCameraEntry>,
}

impl PtzCameraSetting {
    /// The setting from the desktop global's JSON; what it can't read is left out.
    pub fn parse(text: &str) -> Self {
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let cameras = value
            .get("cameras")
            .and_then(serde_json::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|entry| {
                        Some(PtzCameraEntry {
                            name: entry.get("name")?.as_str()?.to_owned(),
                            address: entry.get("address")?.as_str()?.to_owned(),
                            transport: entry.get("transport")?.as_str()?.to_owned(),
                            visca: u8::try_from(entry.get("visca")?.as_u64()?).ok()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            on: value.get("on").and_then(serde_json::Value::as_bool) == Some(true),
            cameras,
        }
    }

    /// The setting as the desktop app's `setPtzCameras` takes it.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "on": self.on,
            "cameras": self
                .cameras
                .iter()
                .map(|c| serde_json::json!({
                    "name": c.name,
                    "address": c.address,
                    "transport": c.transport,
                    "visca": c.visca,
                }))
                .collect::<Vec<_>>(),
        })
        .to_string()
    }
}

/// Asks the desktop app to save "Control PTZ cameras", which it applies at once.
/// Resolves to `None` when it was saved and applied, or why not.
pub async fn set_ptz_cameras(setting: &PtzCameraSetting) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        save_setting_js("setPtzCameras", &setting.to_json()).await
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

/// Asks the desktop app to zoom its webview to `factor` (1.0 is 100 %), as the UI scale
/// does (`ui_scale.rs`), through the global's `setZoom`. Whether the app has it: in a
/// browser, and in an app without it, the page zooms itself instead.
pub fn set_desktop_zoom(factor: f64) -> bool {
    use leptos::wasm_bindgen::JsCast;
    use leptos::web_sys::js_sys::Function;

    let Some(global) = global() else {
        return false;
    };
    let Some(function) = Reflect::get(&global, &JsValue::from_str("setZoom"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
    else {
        return false;
    };
    function.call1(&global, &JsValue::from_f64(factor)).is_ok()
}

/// Whether the desktop app saves screenshots itself (the global's `saveScreenshot`,
/// ADR-0163 §3), in Downloads, and says where. A browser saves one as a download.
pub fn saves_screenshots() -> bool {
    global()
        .and_then(|global| Reflect::get(&global, &JsValue::from_str("saveScreenshot")).ok())
        .is_some_and(|value| value.is_function())
}

/// Asks the desktop app to save `png` under the file name `name` in Downloads. Resolves
/// to the path it saved, or why it didn't.
pub async fn save_screenshot(png: &[u8], name: &str) -> Result<String, String> {
    #[cfg(target_arch = "wasm32")]
    {
        save_screenshot_js(png, name).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (png, name);
        Err("only the desktop app saves screenshots itself".to_owned())
    }
}

/// Calls `saveScreenshot` on the desktop global and awaits its promise.
#[cfg(target_arch = "wasm32")]
async fn save_screenshot_js(png: &[u8], name: &str) -> Result<String, String> {
    use leptos::wasm_bindgen::JsCast;
    use leptos::web_sys::js_sys::{Function, Promise, Uint8Array};

    let failed = || "the app could not save it".to_owned();
    let function = global()
        .and_then(|global| Reflect::get(&global, &JsValue::from_str("saveScreenshot")).ok())
        .and_then(|value| value.dyn_into::<Function>().ok())
        .ok_or_else(failed)?;
    let promise = function
        .call2(
            &JsValue::UNDEFINED,
            &Uint8Array::from(png),
            &JsValue::from_str(name),
        )
        .ok()
        .and_then(|value| value.dyn_into::<Promise>().ok())
        .ok_or_else(failed)?;
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(path) => path
            .as_string()
            .filter(|path| !path.is_empty())
            .ok_or_else(failed),
        Err(reason) => Err(reason.as_string().unwrap_or_else(failed)),
    }
}

/// Asks the desktop app to open a file it saved this run (`openSaved`), or, with
/// `reveal`, to show it in its folder (`showSaved`). The app refuses any other file.
pub fn open_saved(path: &str, reveal: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        use leptos::wasm_bindgen::JsCast;
        use leptos::web_sys::js_sys::Function;
        let name = if reveal { "showSaved" } else { "openSaved" };
        if let Some((global, function)) = global().and_then(|global| {
            let function = Reflect::get(&global, &JsValue::from_str(name))
                .ok()?
                .dyn_into::<Function>()
                .ok()?;
            Some((global, function))
        }) {
            let _called = function.call1(&global, &JsValue::from_str(path));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (path, reveal);
    }
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

/// The event the desktop app dispatches on `window` for each step of a drag of files
/// from the desktop over its main window. Its bootstrap script names it too.
pub const FILE_DROP_EVENT: &str = "jstream-desktop-drop";

/// Which step of a file drag a [`FileDrop`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPhase {
    /// The drag entered the window or moved over it: show where it would land.
    Over,
    /// The files were dropped.
    Drop,
    /// The drag left the window or was cancelled: clear any highlight.
    Leave,
}

/// A step of a drag of files from the desktop over the desktop app's window.
#[derive(Clone, Debug, PartialEq)]
pub struct FileDrop {
    /// The step.
    pub phase: DropPhase,
    /// The dragged files' paths, on the app's machine (its own engine's). Empty on
    /// [`DropPhase::Leave`].
    pub paths: Vec<String>,
    /// Where the pointer is, in the page's CSS pixels from the viewport's left (0 on
    /// [`DropPhase::Leave`]).
    pub x: f64,
    /// Where the pointer is, in CSS pixels from the viewport's top.
    pub y: f64,
}

impl FileDrop {
    /// A step from the event's `detail` as JSON (`{ kind, paths, x, y }`): `None` when
    /// it has no known `kind`. Paths that aren't strings are left out, and a missing
    /// or non-numeric position is 0.
    pub fn parse(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let phase = match value.get("kind")?.as_str()? {
            "over" => DropPhase::Over,
            "drop" => DropPhase::Drop,
            "leave" => DropPhase::Leave,
            _ => return None,
        };
        let paths = value
            .get("paths")
            .and_then(serde_json::Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let coordinate = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_f64)
                .filter(|v| v.is_finite())
                .unwrap_or(0.0)
        };
        Some(Self {
            phase,
            paths,
            x: coordinate("x"),
            y: coordinate("y"),
        })
    }

    /// The element under the drop's position ([`element_at`]).
    pub fn element(&self) -> Option<leptos::web_sys::Element> {
        element_at(self.x, self.y)
    }

    /// Whether the drop is over `target` or one of its descendants, so a panel can tell
    /// a drop on itself from one elsewhere. False on [`DropPhase::Leave`].
    pub fn lands_on(&self, target: &leptos::web_sys::Node) -> bool {
        self.phase != DropPhase::Leave
            && self
                .element()
                .is_some_and(|element| target.contains(Some(&element)))
    }
}

/// The topmost element at `(x, y)`, in the page's CSS pixels (`document.elementFromPoint`),
/// if any.
pub fn element_at(x: f64, y: f64) -> Option<leptos::web_sys::Element> {
    leptos::prelude::document().element_from_point(x as f32, y as f32)
}

/// Calls `f` with each step of a drag of files from the desktop over the desktop app's
/// main window, until the returned handle is removed (`WindowListenerHandle::remove`,
/// e.g. in `on_cleanup`). Any panel may listen; each decides with
/// [`FileDrop::lands_on`] whether a drop is its own. Outside the desktop app nothing
/// ever sends one, so this registers nothing and returns `None`. A step the page can't
/// read is ignored.
pub fn on_file_drop(
    f: impl Fn(FileDrop) + 'static,
) -> Option<leptos::prelude::WindowListenerHandle> {
    global()?;
    Some(leptos::prelude::window_event_listener_untyped(
        FILE_DROP_EVENT,
        move |event| {
            let drop = Reflect::get(&event, &JsValue::from_str("detail"))
                .ok()
                .and_then(|detail| leptos::web_sys::js_sys::JSON::stringify(&detail).ok())
                .and_then(|text| text.as_string())
                .and_then(|text| FileDrop::parse(&text));
            if let Some(drop) = drop {
                f(drop);
            }
        },
    ))
}

/// The engine from the global's fields: both must be non-empty strings.
fn engine(url: Option<String>, token: Option<String>) -> Option<HostEngine> {
    let url = url.filter(|url| !url.is_empty())?;
    let token = token.filter(|token| !token.is_empty())?;
    Some(HostEngine { url, token })
}

#[cfg(test)]
mod tests {
    use super::{DropPhase, FileDrop, HostEngine, engine};

    #[test]
    fn a_file_drop_reads_the_apps_detail() {
        assert_eq!(
            FileDrop::parse(
                r#"{"kind":"drop","paths":["/home/me/clip.mp4","C:\\v\\a.png"],"x":12.5,"y":40}"#
            ),
            Some(FileDrop {
                phase: DropPhase::Drop,
                paths: vec!["/home/me/clip.mp4".into(), r"C:\v\a.png".into()],
                x: 12.5,
                y: 40.0,
            })
        );
        assert_eq!(
            FileDrop::parse(r#"{"kind":"over","paths":["/a",3,null],"x":"1"}"#),
            Some(FileDrop {
                phase: DropPhase::Over,
                paths: vec!["/a".into()],
                x: 0.0,
                y: 0.0,
            })
        );
        assert_eq!(
            FileDrop::parse(r#"{"kind":"leave","paths":[],"x":0,"y":0}"#).map(|d| d.phase),
            Some(DropPhase::Leave)
        );
        assert_eq!(FileDrop::parse(r#"{"kind":"enter","paths":[]}"#), None);
        assert_eq!(FileDrop::parse(r#"{"paths":[]}"#), None);
        assert_eq!(FileDrop::parse("not json"), None);
    }

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

    /// The PTZ camera list reads back as the app wrote it, skipping what it can't read.
    #[test]
    fn the_ptz_camera_setting_round_trips() {
        use super::{PtzCameraEntry, PtzCameraSetting};
        let setting = PtzCameraSetting {
            on: true,
            cameras: vec![PtzCameraEntry {
                name: "Wide".into(),
                address: "192.168.1.40:52381".into(),
                transport: "visca_ip".into(),
                visca: 2,
            }],
        };
        assert_eq!(PtzCameraSetting::parse(&setting.to_json()), setting);
        let partial = PtzCameraSetting::parse(
            r#"{"on":"yes","cameras":[{"name":"x"},{"name":"Close","address":"10.0.0.9:5678","transport":"tcp","visca":1}]}"#,
        );
        assert!(!partial.on);
        assert_eq!(partial.cameras.len(), 1);
        assert_eq!(
            PtzCameraSetting::parse("not json"),
            PtzCameraSetting::default()
        );
    }
}
