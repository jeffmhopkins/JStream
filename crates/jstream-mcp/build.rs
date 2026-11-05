//! Embeds the workspace release version so `serverInfo.version` is the app's
//! version. This crate stays at `0.0.0` (docs/runbooks/release.md); `CARGO_PKG_VERSION`
//! would report that.
//!
//! Docs: crates/jstream-mcp/README.md#design

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let workspace = manifest_dir.join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", workspace.display());
    let text = fs::read_to_string(&workspace).unwrap_or_else(|error| {
        panic!(
            "reading {} for the release version: {error}",
            workspace.display()
        )
    });
    let version = workspace_package_version(&text)
        .unwrap_or_else(|| panic!("{} has no [workspace.package] version", workspace.display()));
    println!("cargo:rustc-env=JSTREAM_VERSION={version}");
}

/// The `version` under `[workspace.package]`, without quotes.
fn workspace_package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[workspace.package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("version") else {
            continue;
        };
        let rest = rest.trim().strip_prefix('=')?.trim();
        let rest = rest.trim_matches('"');
        if !rest.is_empty() && !rest.contains(' ') {
            return Some(rest.to_owned());
        }
    }
    None
}
