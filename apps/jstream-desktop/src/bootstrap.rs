//! The script the webview runs before the UI's own, which hands the UI the in-process
//! engine's URL and control token, and what the app knows about its virtual camera.
//!
//! It defines `window.__JSTREAM_DESKTOP__` as a frozen `{ url, token, vcam, collection,
//! network, networkOpen, globalWheel, browse, allowNetwork }` that can't be reassigned, and only on a page from the
//! app's own origin: the webview also refuses to navigate anywhere else (see `shell.rs`),
//! so no other page ever sees the token. `collection` is the data folder, which
//! `collection` asset paths are relative to. `browse` asks the app's `pick_path` command
//! to open a native dialog and resolves to the path the person picked, or null.
//! `network` is whether "Allow devices on my network" was on when the app started,
//! `networkOpen` whether the network listener then opened, and `allowNetwork(allow)`
//! asks the `set_network` command to save it (`network.rs`), resolving to whether it was
//! saved (false when the command fails too); it applies at the next start.
//! `globalWheel` is whether wheel chords are heard while another app has the focus
//! (false on Wayland, ADR-0112 §5), so the UI can say when they are not.
//! `jstream-ui`'s host hook (`host.rs`) reads it and connects without the connect form.
//! The token is the in-memory one the app made at startup, or with `--mcp` the data
//! folder's user-only token file's; it is never written to a log (R-SEC-001).
//!
//! Docs: apps/jstream-desktop/README.md#design

use std::fmt::Write as _;

/// The global the UI looks for. `jstream-ui`'s `host::DESKTOP_GLOBAL` names it too.
pub const GLOBAL: &str = "__JSTREAM_DESKTOP__";

/// What the script tells the page about the host, beside the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// The "Allow devices on my network" setting.
    pub network: bool,
    /// Whether the network listener opened.
    pub network_open: bool,
    /// Whether wheel chords are heard while another app has the focus
    /// ([`crate::host::Host::global_wheel`]).
    pub global_wheel: bool,
}

/// The script, for a page from `origin`, connecting to `url` with `token`. `vcam` is
/// the virtual camera's status ([`crate::host::VcamStatus::as_str`]). `collection` is
/// the data folder.
pub fn script(
    origin: &str,
    url: &str,
    token: &str,
    vcam: &str,
    collection: &str,
    flags: Flags,
) -> String {
    let Flags {
        network,
        network_open,
        global_wheel,
    } = flags;
    let bool_js = |b: bool| if b { "true" } else { "false" };
    format!(
        "if (window.location.origin === {origin}) {{\
         Object.defineProperty(window, {global}, {{\
         value: Object.freeze({{ url: {url}, token: {token}, vcam: {vcam}, collection: {collection},\
         network: {network}, networkOpen: {network_open}, globalWheel: {global_wheel},\
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
         }} }}),\
         writable: false, configurable: false, enumerable: false }});\
         }}",
        origin = js_string(origin),
        global = js_string(GLOBAL),
        url = js_string(url),
        token = js_string(token),
        vcam = js_string(vcam),
        collection = js_string(collection),
        network = bool_js(network),
        network_open = bool_js(network_open),
        global_wheel = bool_js(global_wheel),
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
    use super::{Flags, js_string, script};

    #[test]
    fn the_script_defines_the_global_only_on_the_app_origin() {
        let script = script(
            "http://jstream.localhost",
            "ws://127.0.0.1:4455/api/v1/ws",
            "0a1b",
            "registered",
            r"C:\Users\me\AppData\Roaming\JStream",
            Flags {
                network: true,
                network_open: false,
                global_wheel: true,
            },
        );
        assert!(script.starts_with("if (window.location.origin === \"http://jstream.localhost\")"));
        assert!(script.contains("\"__JSTREAM_DESKTOP__\""));
        assert!(script.contains("url: \"ws://127.0.0.1:4455/api/v1/ws\""));
        assert!(script.contains("token: \"0a1b\""));
        assert!(script.contains("vcam: \"registered\""));
        assert!(script.contains(r#"collection: "C:\\Users\\me\\AppData\\Roaming\\JStream""#));
        assert!(script.contains("invoke(\"pick_path\", request)"));
        assert!(script.contains("network: true, networkOpen: false, globalWheel: true,"));
        assert!(script.contains("invoke(\"set_network\", { allow: allow === true })"));
        assert!(
            script.contains("function () { return false; }"),
            "a failed command resolves to false"
        );
        assert!(
            script.contains("function () { return false; }"),
            "a failed command resolves to false"
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
