//! The script the webview runs before the UI's own, which hands the UI the in-process
//! engine's URL and control token.
//!
//! It defines `window.__JSTREAM_DESKTOP__` as a frozen `{ url, token }` that can't be
//! reassigned, and only on a page from the app's own origin: the webview also refuses
//! to navigate anywhere else (see `shell.rs`), so no other page ever sees the token.
//! `jstream-ui`'s host hook (`host.rs`) reads it and connects without the connect form.
//! The token is the in-memory one the app made at startup; it is never written to a
//! file or a log (R-SEC-001).
//!
//! Docs: apps/jstream-desktop/README.md#design

use std::fmt::Write as _;

/// The global the UI looks for. `jstream-ui`'s `host::DESKTOP_GLOBAL` names it too.
pub const GLOBAL: &str = "__JSTREAM_DESKTOP__";

/// The script, for a page from `origin`, connecting to `url` with `token`.
pub fn script(origin: &str, url: &str, token: &str) -> String {
    format!(
        "if (window.location.origin === {origin}) {{\
         Object.defineProperty(window, {global}, {{\
         value: Object.freeze({{ url: {url}, token: {token} }}),\
         writable: false, configurable: false, enumerable: false }});\
         }}",
        origin = js_string(origin),
        global = js_string(GLOBAL),
        url = js_string(url),
        token = js_string(token),
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
    use super::{js_string, script};

    #[test]
    fn the_script_defines_the_global_only_on_the_app_origin() {
        let script = script(
            "http://jstream.localhost",
            "ws://127.0.0.1:4455/api/v1/ws",
            "0a1b",
        );
        assert!(script.starts_with("if (window.location.origin === \"http://jstream.localhost\")"));
        assert!(script.contains("\"__JSTREAM_DESKTOP__\""));
        assert!(script.contains(
            "Object.freeze({ url: \"ws://127.0.0.1:4455/api/v1/ws\", token: \"0a1b\" })"
        ));
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
