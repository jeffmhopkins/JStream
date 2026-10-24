//! The JStream web UI: a Leptos app, rendered client-side and compiled to `wasm32`, that
//! drives the engine through `jstream-control`'s WebSocket client (ADR-0045). The daemon
//! serves it over HTTP and the desktop app shows it in a Tauri webview. Like every client
//! it never links the engine (R-ARCH-003).
//!
//! It holds connecting and the session (`session`), the shell (`app`), the header bar
//! (`header`), the dock model (`dock`) and the workspace that draws it (`workspace`),
//! the state the panels share (`store`), the panels (`panels`), the pairing panel
//! (`pairing`), the preview canvas (`canvas`), the right-click menu (`menu`), the modal
//! dialog frame (`dialog`) and the prototype's icons (`icons`).
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-ui/README.md#purpose, crates/jstream-ui/README.md#design, crates/jstream-ui/README.md#canvas, crates/jstream-ui/README.md#build, crates/jstream-ui/tests/e2e/README.md#harness, crates/jstream-ui/tests/e2e/README.md#tests, crates/jstream-ui/tests/e2e/README.md#running, docs/UI-FEATURES.md#theme, docs/UI-FEATURES.md#header, docs/UI-FEATURES.md#docking, docs/UI-FEATURES.md#canvas, docs/UI-FEATURES.md#menus, docs/UI-FEATURES.md#dialogs, docs/UI-FEATURES.md#scenes, docs/UI-FEATURES.md#objects, docs/UI-FEATURES.md#inspector, docs/UI-FEATURES.md#mixer, docs/UI-FEATURES.md#outputs, docs/UI-FEATURES.md#keyboard, docs/UI-FEATURES.md#beyond, docs/adr/0045-leptos-web-ui.md

#![forbid(unsafe_code)]

pub mod app;
pub mod canvas;
pub mod dialog;
pub mod dock;
pub mod header;
pub mod host;
pub mod icons;
pub mod menu;
pub mod pairing;
pub mod panels;
pub mod session;
pub mod store;
pub mod workspace;

pub use app::{App, SessionContext};

/// Mounts the app on the page's `<body>`. The `wasm32` binary's entry point.
pub fn start() {
    leptos::mount::mount_to_body(App);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Collects every `.rs` file under `dir`.
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// Whether `line` sets a `style` attribute: the word `style`, then optional spaces and
    /// `=`. That covers `style="…"`, `style='…'`, `style=move || …` and `attr:style=…`,
    /// which all reach the element through `setAttribute`; a `style:` binding doesn't match.
    fn sets_style_attribute(line: &str) -> bool {
        line.match_indices("style").any(|(at, word)| {
            let before = line[..at].chars().next_back();
            let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
            !before.is_some_and(is_word) && line[at + word.len()..].trim_start().starts_with('=')
        })
    }

    #[test]
    fn the_style_attribute_check_matches_attributes_not_bindings() {
        for attribute in [
            r#"<div style="left:0">"#,
            "<div style='left:0'>",
            r#"<div style = "left:0">"#,
            r#"<div style=move || format!("left:{x}px")>"#,
            r#"<div attr:style="left:0">"#,
        ] {
            assert!(sets_style_attribute(attribute), "{attribute}");
        }
        for allowed in [
            "<div style:left=move || x>",
            "let style_sheet = 1;",
            "a `style` attribute",
            r#"<div data-style="x">"#,
        ] {
            assert!(!sets_style_attribute(allowed), "{allowed}");
        }
    }

    /// The hosts' CSP allows styles only from `'self'` and a nonce, so the browser refuses
    /// a `style` attribute (#261), static or set at run time. Static styles go in
    /// `style.css`; dynamic ones use a `style:` binding, which goes through the CSSOM.
    #[test]
    fn views_set_no_inline_style_attributes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        files.push(root.join("index.html"));
        let offenders: Vec<String> = files
            .iter()
            .flat_map(|path| {
                let text = std::fs::read_to_string(path).expect("read source");
                // Test modules don't render, and this one spells the attributes out.
                text.lines()
                    .take_while(|line| line.trim() != "#[cfg(test)]")
                    .enumerate()
                    .filter(|(_, line)| sets_style_attribute(line))
                    .map(|(n, _)| format!("{}:{}", path.display(), n + 1))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "inline style attributes: {offenders:?}"
        );
    }
}
