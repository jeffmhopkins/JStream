//! The JStream web UI: a Leptos app, rendered client-side and compiled to `wasm32`, that
//! drives the engine through `jstream-control`'s WebSocket client (ADR-0045). The daemon
//! serves it over HTTP and the desktop app shows it in a Tauri webview. Like every client
//! it never links the engine (R-ARCH-003).
//!
//! This is the skeleton: connecting, the session, and the shell's regions. The panels
//! (`panels`) and the preview canvas (`canvas`) fill them in.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-ui/README.md#purpose, crates/jstream-ui/README.md#design, crates/jstream-ui/README.md#canvas, crates/jstream-ui/README.md#build, docs/adr/0045-leptos-web-ui.md

#![forbid(unsafe_code)]

pub mod app;
pub mod canvas;
pub mod host;
pub mod panels;
pub mod session;

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

    /// The hosts' CSP allows styles only from `'self'` and a nonce, so the browser refuses
    /// a `style` attribute (#261). Static styles go in `style.css`; dynamic ones use a
    /// `style:` binding, which goes through the CSSOM.
    #[test]
    fn views_set_no_inline_style_attributes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        files.push(root.join("index.html"));
        let needle = concat!("style", "=\"");
        let offenders: Vec<String> = files
            .iter()
            .flat_map(|path| {
                let text = std::fs::read_to_string(path).expect("read source");
                text.lines()
                    .enumerate()
                    .filter(|(_, line)| line.contains(needle))
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
