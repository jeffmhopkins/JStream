//! `cargo xtask browser-linux`: lays out the browser helper for Linux (ADR-0095), the
//! `browser` folder a Linux host finds beside its executable. It builds the helper
//! executable with CEF (which downloads CEF's pinned distribution) and copies it, CEF's
//! runtime files and `locales`, and `CEF-VERSION.txt` into `<out>/browser`. Build the
//! host with its `cef` feature and put that folder beside it, as the Windows zip does.
//!
//! Docs: docs/ARCHITECTURE.md#packaging, docs/adr/0095-browser-helper-on-linux.md

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::dist::{cef_dir, cef_version_text, copy, copy_dir};

const USAGE: &str = "usage: cargo xtask browser-linux [--out <folder>] [--debug]";

/// The helper's package and executable.
const HELPER: &str = "jstream-browser-helper";

/// The library the helper links, at the top of CEF's distribution folder.
const LIBCEF: &str = "libcef.so";

/// Chromium's setuid sandbox helper, for machines where user namespaces are off.
pub(crate) const CHROME_SANDBOX: &str = "chrome-sandbox";

/// The files at the top of CEF's distribution folder that only building needs.
const CEF_BUILD_ONLY: &[&str] = &["archive.json", "CMakeLists.txt"];

/// Runs `cargo xtask browser-linux`.
pub fn run(args: &[String], root: &Path) -> Result<bool> {
    let mut out = None;
    let mut release = true;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    rest.next()
                        .with_context(|| format!("--out needs a value\n{USAGE}"))?,
                ));
            }
            "--debug" => release = false,
            _ => bail!("unknown argument `{arg}`\n{USAGE}"),
        }
    }
    if !cfg!(target_os = "linux") {
        bail!("the Linux browser helper is built on Linux");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let profile = if release { "release" } else { "debug" };
    let built = target.join(profile);
    let out = out.unwrap_or_else(|| built.clone());
    let cef = build(root, release)?;
    let to = out.join("browser");
    if to.exists() {
        fs::remove_dir_all(&to).with_context(|| format!("removing {}", to.display()))?;
    }
    stage(&cef, &built.join(HELPER), &to)?;
    eprintln!(
        "wrote {}: put it beside a host built with its `cef` feature",
        to.display()
    );
    Ok(true)
}

/// Builds the release helper and lays the `browser` folder out at `to`, replacing what is
/// there: what `cargo xtask dist-linux` puts in the package.
pub(crate) fn build_release(root: &Path, target: &Path, to: &Path) -> Result<()> {
    let cef = build(root, true)?;
    if to.exists() {
        fs::remove_dir_all(to).with_context(|| format!("removing {}", to.display()))?;
    }
    stage(&cef, &target.join("release").join(HELPER), to)
}

/// Builds the helper executable with CEF, and returns CEF's distribution folder.
fn build(root: &Path, release: bool) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.args([
        "build",
        "--locked",
        "-p",
        HELPER,
        "--bin",
        HELPER,
        "--features",
        "cef",
    ]);
    if release {
        command.arg("--release");
    }
    let output = command
        .arg("--message-format=json-render-diagnostics")
        .current_dir(root)
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("running cargo build for the browser helper")?;
    if !output.status.success() {
        bail!(
            "cargo build of the browser helper failed: {}",
            output.status
        );
    }
    let messages = String::from_utf8_lossy(&output.stdout);
    cef_dir(&messages, |dir| dir.join(LIBCEF).is_file())
        .context("the build named no CEF distribution folder holding libcef.so")
}

/// Whether the file `name`, at the top of CEF's distribution folder, ships.
fn ships_from_cef(name: &str) -> bool {
    !(name.ends_with(".a") || CEF_BUILD_ONLY.contains(&name))
}

/// Lays the helper out in `to`: CEF's runtime files from `cef` and its `locales`, the
/// helper executable `helper`, and `CEF-VERSION.txt`. `chrome-sandbox` keeps its mode,
/// but is only setuid where someone makes it so (ADR-0095).
fn stage(cef: &Path, helper: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(cef).with_context(|| format!("reading {}", cef.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file() && name.to_str().is_some_and(ships_from_cef) {
            copy(&entry.path(), &to.join(&name))?;
        }
    }
    if !to.join(CHROME_SANDBOX).is_file() {
        eprintln!("note: CEF's distribution has no {CHROME_SANDBOX}");
    }
    copy_dir(&cef.join("locales"), &to.join("locales"))?;
    copy(helper, &to.join(HELPER))?;
    let archive = fs::read_to_string(cef.join("archive.json"))
        .with_context(|| format!("reading {}", cef.join("archive.json").display()))?;
    fs::write(to.join("CEF-VERSION.txt"), cef_version_text(&archive)?)
        .context("writing CEF-VERSION.txt")
}

#[cfg(test)]
mod tests {
    use super::ships_from_cef;

    #[test]
    fn only_the_runtime_ships() {
        for name in ["libcef.so", "chrome-sandbox", "icudtl.dat", "resources.pak"] {
            assert!(ships_from_cef(name), "{name}");
        }
        for name in ["archive.json", "CMakeLists.txt", "libcef_dll_wrapper.a"] {
            assert!(!ships_from_cef(name), "{name}");
        }
    }
}
