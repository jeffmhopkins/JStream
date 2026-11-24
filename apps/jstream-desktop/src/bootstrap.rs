//! The script the webview runs before the UI's own, which hands the UI the in-process
//! engine's URL and control token, and what the app knows about its virtual camera.
//!
//! It defines `window.__JSTREAM_DESKTOP__` as a frozen `{ url, token, vcam, collection,
//! network, networkOpen, wheelFocusOnly, browse, allowNetwork, jtunnel, setJtunnel, ptz,
//! setPtz, ptzCameras, setPtzCameras, saveScreenshot, openSaved, showSaved, setZoom }` that can't be reassigned, and only on a page from the
//! app's own origin: the webview also refuses to navigate anywhere else (see `shell.rs`),
//! so no other page ever sees the token. `collection` is the default project's folder
//! (ADR-0163), which `collection` asset paths are relative to until the engine names
//! the open project. `browse` asks the app's `pick_path` command
//! to open a native dialog and resolves to the path the person picked, or null.
//! `wheelFocusOnly` is whether, at the start, wheel chords were heard only while JStream
//! had the focus, because the app lacked macOS's Accessibility permission.
//! `network` is whether "Allow devices on my network" was on when the app started,
//! `networkOpen` whether the network listener then opened, and `allowNetwork(allow)`
//! asks the `set_network` command to save it (`network.rs`), resolving to whether it was
//! saved (false when the command fails too); it applies at the next start.
//! `jtunnel` is "Receive from JTunnel senders" as the app read it at start (`{on,
//! accepted: [{name, address, state}], live}`, `jtunnel.rs`), and `setJtunnel(setting)` asks the
//! `set_jtunnel` command to save one, resolving to `null` when it was saved or the
//! reason it wasn't; it too applies at the next start. `ptz` and `setPtz(setting)` are
//! "Accept a PTZ controller" the same way (`{on, endpoints: [{address, transport, visca,
//! target}], allow: [ip]}`, `ptz.rs`), through the `set_ptz_controllers` command.
//! `ptzCameras` and `setPtzCameras(setting)` are "Control PTZ cameras" (`{on, cameras:
//! [{name, address, transport, visca}]}`, `ptz_cameras.rs`), through the
//! `set_ptz_cameras` command, which applies a saved list at once.
//! `saveScreenshot(png, name)` sends a screenshot's bytes to the `save_screenshot`
//! command, its name percent-encoded in a header, and resolves to the path it saved in
//! Downloads, or rejects with why not; `openSaved(path)` and `showSaved(path)` ask
//! `open_saved` and `show_saved` to open such a file or show it in its folder
//! (`screenshots.rs`).
//! `setZoom(factor)` asks the `set_zoom` command to zoom the window's webview for the
//! UI scale (`zoom.rs`) and returns at once: true when it could ask, false when the page
//! has no Tauri bridge (the UI then zooms itself).
//! `jstream-ui`'s host hook (`host.rs`) reads it and connects without the connect form.
//! The token is the in-memory one the app made at startup, or with `--mcp` the data
//! folder's user-only token file's; it is never written to a log (R-SEC-001).
//!
//! Files dragged from the desktop onto the main window don't reach the page as an HTML
//! drop: Tauri handles the drag natively. `shell.rs` tells the page each step of it
//! with [`drop_script`], which dispatches [`DROP_EVENT`] on `window` with a `detail` of
//! `{ kind, paths, x, y }`: `kind` is `over`, `drop` or `leave`, `paths` the dragged
//! files' paths, and `x`, `y` where the pointer is, in the page's CSS pixels.
//!
//! Docs: apps/jstream-desktop/README.md#design

use std::fmt::Write as _;

/// The global the UI looks for. `jstream-ui`'s `host::DESKTOP_GLOBAL` names it too.
pub const GLOBAL: &str = "__JSTREAM_DESKTOP__";

/// The event [`drop_script`] dispatches on the page's `window`. `jstream-ui`'s
/// `host::FILE_DROP_EVENT` names it too.
pub const DROP_EVENT: &str = "jstream-desktop-drop";

/// A step of a drag of files from the desktop over the window, as the page is told it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    /// The drag entered the window or moved over it.
    Over,
    /// The files were dropped.
    Drop,
    /// The drag left the window or was cancelled.
    Leave,
}

impl DropKind {
    /// The `kind` the page reads.
    pub fn as_str(self) -> &'static str {
        match self {
            DropKind::Over => "over",
            DropKind::Drop => "drop",
            DropKind::Leave => "leave",
        }
    }
}

/// The script that tells the page about a step of a file drag: it dispatches
/// [`DROP_EVENT`] on `window` with `{ kind, paths, x, y }`. `x` and `y` are already in
/// the page's CSS pixels; a coordinate that isn't finite is sent as 0. The detail
/// travels as a JSON string the page parses, so nothing in a file's name can end the
/// script.
pub fn drop_script(kind: DropKind, paths: &[String], x: f64, y: f64) -> String {
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let detail = serde_json::json!({
        "kind": kind.as_str(),
        "paths": paths,
        "x": finite(x),
        "y": finite(y),
    })
    .to_string();
    format!(
        "window.dispatchEvent(new CustomEvent({event}, {{ detail: JSON.parse({detail}) }}));",
        event = js_string(DROP_EVENT),
        detail = js_string(&detail),
    )
}

/// The position Tauri reports for a drag, in the page's CSS pixels. Tauri calls it a
/// physical position, but only WebView2 (Windows) reports physical pixels; WKWebView
/// (macOS) and WebKitGTK (Linux) report the view's logical points already. `scale` is
/// the window's scale factor where the position is physical, else 1; one that isn't a
/// positive number counts as 1.
pub fn css_position(x: f64, y: f64, scale: f64) -> (f64, f64) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (x / scale, y / scale)
}

/// What the app tells the page beside its engine's URL and token.
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    /// The virtual camera's status ([`crate::host::VcamStatus::as_str`]).
    pub vcam: &'a str,
    /// The default project's folder, which `collection` asset paths are relative to.
    pub collection: &'a str,
    /// The "Allow devices on my network" setting.
    pub network: bool,
    /// Whether the network listener opened.
    pub network_open: bool,
    /// Whether wheel chords are heard only while JStream has the focus, because the app
    /// lacks a permission the OS asks for (macOS's Accessibility), as of the start.
    pub wheel_focus_only: bool,
    /// "Receive from JTunnel senders" as the app read it.
    pub jtunnel: &'a crate::jtunnel::Setting,
    /// "Accept a PTZ controller" as the app read it.
    pub ptz: &'a crate::ptz::Setting,
    /// "Control PTZ cameras" as the app read it.
    pub ptz_cameras: &'a crate::ptz_cameras::Setting,
}

/// The script, for a page from `origin`, connecting to `url` with `token`. `vcam` is
/// the virtual camera's status ([`crate::host::VcamStatus::as_str`]). `collection` is
/// the default project's folder. `network` is the "Allow devices on my network" setting, and
/// `network_open` whether the network listener opened. `jtunnel` is the JTunnel
/// setting as the app read it, `ptz` the PTZ controller one and `ptz_cameras` the PTZ
/// camera one.
pub fn script(origin: &str, url: &str, token: &str, page: &Page<'_>) -> String {
    let Page {
        vcam,
        collection,
        network,
        network_open,
        wheel_focus_only,
        jtunnel,
        ptz,
        ptz_cameras,
    } = *page;
    // As a string the page parses, so nothing in a sender's name can end the script.
    let jtunnel = serde_json::to_string(jtunnel).unwrap_or_else(|_| "{}".to_owned());
    let ptz = serde_json::to_string(ptz).unwrap_or_else(|_| "{}".to_owned());
    let ptz_cameras = serde_json::to_string(ptz_cameras).unwrap_or_else(|_| "{}".to_owned());
    format!(
        "if (window.location.origin === {origin}) {{\
         Object.defineProperty(window, {global}, {{\
         value: Object.freeze({{ url: {url}, token: {token}, vcam: {vcam}, collection: {collection},\
         network: {network}, networkOpen: {network_open}, wheelFocusOnly: {wheel_focus_only},\
         browse: function (request) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(null);\
         return internals.invoke(\"pick_path\", request);\
         }},\
         allowNetwork: function (allow) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(false);\
         return internals.invoke(\"set_network\", {{ allow: allow === true }})\
         .then(function (saved) {{ return saved === true; }}, function () {{ return false; }});\
         }},\
         jtunnel: Object.freeze(JSON.parse({jtunnel})),\
         setJtunnel: function (setting) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(\"the app can't save settings here\");\
         return internals.invoke(\"set_jtunnel\", {{ setting: setting }})\
         .then(function (problem) {{ return problem === null ? null : String(problem); }},\
         function (error) {{ return String(error); }});\
         }},\
         ptz: Object.freeze(JSON.parse({ptz})),\
         setPtz: function (setting) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(\"the app can't save settings here\");\
         return internals.invoke(\"set_ptz_controllers\", {{ setting: setting }})\
         .then(function (problem) {{ return problem === null ? null : String(problem); }},\
         function (error) {{ return String(error); }});\
         }},\
         ptzCameras: Object.freeze(JSON.parse({ptz_cameras})),\
         setPtzCameras: function (setting) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(\"the app can't save settings here\");\
         return internals.invoke(\"set_ptz_cameras\", {{ setting: setting }})\
         .then(function (problem) {{ return problem === null ? null : String(problem); }},\
         function (error) {{ return String(error); }});\
         }},\
         saveScreenshot: function (png, name) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.reject(\"the app can't save files here\");\
         return internals.invoke(\"save_screenshot\", png, {{ headers: {{ \"x-jstream-name\": encodeURIComponent(String(name)) }} }})\
         .then(function (path) {{ return String(path); }}, function (error) {{ return Promise.reject(String(error)); }});\
         }},\
         openSaved: function (path) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(false);\
         return internals.invoke(\"open_saved\", {{ path: String(path) }}).then(function (opened) {{ return opened === true; }}, function () {{ return false; }});\
         }},\
         showSaved: function (path) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return Promise.resolve(false);\
         return internals.invoke(\"show_saved\", {{ path: String(path) }}).then(function (shown) {{ return shown === true; }}, function () {{ return false; }});\
         }},\
         setZoom: function (factor) {{\
         var internals = window.__TAURI_INTERNALS__;\
         if (!internals || typeof internals.invoke !== \"function\") return false;\
         internals.invoke(\"set_zoom\", {{ factor: Number(factor) }}).catch(function () {{}});\
         return true;\
         }} }}),\
         writable: false, configurable: false, enumerable: false }});\
         }}",
        origin = js_string(origin),
        global = js_string(GLOBAL),
        url = js_string(url),
        token = js_string(token),
        vcam = js_string(vcam),
        collection = js_string(collection),
        network = if network { "true" } else { "false" },
        network_open = if network_open { "true" } else { "false" },
        wheel_focus_only = if wheel_focus_only { "true" } else { "false" },
        jtunnel = js_string(&jtunnel),
        ptz = js_string(&ptz),
        ptz_cameras = js_string(&ptz_cameras),
    )
}

/// `text` as a JavaScript string literal. Besides quotes and backslashes, `<`, `>` and
/// every control or non-ASCII character are escaped, so the literal can't end a script
/// or a line whatever `text` holds.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c.is_ascii_graphic() && c != '<' && c != '>') || c == ' ' => out.push(c),
            c => {
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{DropKind, Page, css_position, drop_script, js_string, script};

    /// The `detail` a drop script hands the page, unescaped as the page would.
    fn detail(script: &str) -> serde_json::Value {
        let start = script.find("JSON.parse(").map(|i| i + "JSON.parse(".len());
        let end = script.rfind(") }));");
        let (Some(start), Some(end)) = (start, end) else {
            panic!("no JSON.parse in {script}");
        };
        let literal: String = serde_json::from_str(&script[start..end]).expect("a JS string");
        serde_json::from_str(&literal).expect("JSON")
    }

    #[test]
    fn a_drop_is_an_event_the_page_parses() {
        let paths = vec![
            "/home/me/clip.mp4".to_owned(),
            "C:\\a\"</script>\u{2028}é.png".to_owned(),
        ];
        let script = drop_script(DropKind::Drop, &paths, 12.5, 40.0);
        assert!(script.starts_with(
            "window.dispatchEvent(new CustomEvent(\"jstream-desktop-drop\", { detail: JSON.parse(\""
        ));
        assert!(
            !script.contains("</script>") && !script.contains('\u{2028}'),
            "a file name can't end the script or a line"
        );
        assert_eq!(
            detail(&script),
            serde_json::json!({ "kind": "drop", "paths": paths, "x": 12.5, "y": 40.0 })
        );
    }

    #[test]
    fn every_step_names_its_kind_and_a_bad_position_is_zero() {
        assert_eq!(
            detail(&drop_script(DropKind::Over, &[], 1.0, 2.0)),
            serde_json::json!({ "kind": "over", "paths": [], "x": 1.0, "y": 2.0 })
        );
        assert_eq!(
            detail(&drop_script(DropKind::Leave, &[], f64::NAN, f64::INFINITY)),
            serde_json::json!({ "kind": "leave", "paths": [], "x": 0.0, "y": 0.0 })
        );
    }

    #[test]
    fn a_physical_position_is_scaled_to_css_pixels() {
        assert_eq!(css_position(300.0, 150.0, 1.5), (200.0, 100.0));
        assert_eq!(css_position(300.0, 150.0, 1.0), (300.0, 150.0));
        assert_eq!(css_position(300.0, 150.0, 0.0), (300.0, 150.0));
        assert_eq!(css_position(300.0, 150.0, f64::NAN), (300.0, 150.0));
    }

    #[test]
    fn the_script_defines_the_global_only_on_the_app_origin() {
        let script = script(
            "http://jstream.localhost",
            "ws://127.0.0.1:4455/api/v1/ws",
            "0a1b",
            &Page {
                vcam: "registered",
                collection: r"C:\Users\me\AppData\Roaming\JStream",
                network: true,
                network_open: false,
                wheel_focus_only: true,
                jtunnel: &crate::jtunnel::Setting {
                    on: true,
                    accepted: vec![crate::jtunnel::Entry {
                        name: "</script>".into(),
                        address: "192.168.1.5:9000".into(),
                        state: false,
                        midi: true,
                    }],
                    live: Some("</script>".into()),
                },
                ptz: &crate::ptz::Setting {
                    on: true,
                    endpoints: vec![crate::ptz::Endpoint {
                        address: "192.168.1.20:52381".into(),
                        transport: "visca_ip".into(),
                        visca: 1,
                        target: "scene:</script>".into(),
                    }],
                    allow: vec!["192.168.1.50".into()],
                },
                ptz_cameras: &crate::ptz_cameras::Setting {
                    on: true,
                    cameras: vec![crate::ptz_cameras::Camera {
                        name: "</script>".into(),
                        address: "192.168.1.40:52381".into(),
                        transport: "visca_ip".into(),
                        visca: 1,
                    }],
                },
            },
        );
        assert!(script.starts_with("if (window.location.origin === \"http://jstream.localhost\")"));
        assert!(script.contains("\"__JSTREAM_DESKTOP__\""));
        assert!(script.contains("url: \"ws://127.0.0.1:4455/api/v1/ws\""));
        assert!(script.contains("token: \"0a1b\""));
        assert!(script.contains("vcam: \"registered\""));
        assert!(script.contains(r#"collection: "C:\\Users\\me\\AppData\\Roaming\\JStream""#));
        assert!(script.contains("invoke(\"pick_path\", request)"));
        assert!(script.contains("network: true, networkOpen: false, wheelFocusOnly: true,"));
        assert!(script.contains("invoke(\"set_network\", { allow: allow === true })"));
        assert!(
            script.contains("function () { return false; }"),
            "a failed command resolves to false"
        );
        assert!(script.contains("invoke(\"set_jtunnel\", { setting: setting })"));
        assert!(script.contains("jtunnel: Object.freeze(JSON.parse(\"{\\\"on\\\":true"));
        assert!(script.contains("invoke(\"set_ptz_controllers\", { setting: setting })"));
        assert!(script.contains("ptz: Object.freeze(JSON.parse("));
        assert!(script.contains("invoke(\"set_ptz_cameras\", { setting: setting })"));
        assert!(script.contains("ptzCameras: Object.freeze(JSON.parse("));
        assert!(script.contains("invoke(\"set_zoom\", { factor: Number(factor) })"));
        assert!(
            !script.contains("</script>"),
            "a sender's name can't end the script"
        );
        assert!(script.contains("writable: false, configurable: false"));
    }

    #[test]
    fn strings_cannot_break_out_of_their_literal() {
        assert_eq!(js_string("plain text"), "\"plain text\"");
        assert_eq!(js_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(
            js_string("</script>\n\u{2028}é"),
            "\"\\u003c/script\\u003e\\u000a\\u2028\\u00e9\""
        );
        assert_eq!(js_string("😀"), "\"\\ud83d\\ude00\"");
    }
}
