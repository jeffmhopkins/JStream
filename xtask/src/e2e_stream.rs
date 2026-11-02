//! `cargo xtask e2e-stream`: streams to a real RTMP server and checks what it received
//! (`apps/jstream-daemon/tests/receiver`), on Linux.
//!
//! It checks that `ffmpeg` and `ffprobe` are installed, points Vulkan at lavapipe when
//! `VK_DRIVER_FILES` isn't set and lavapipe is installed, then runs the harness's
//! ignored tests one at a time. `--openh264 <library>` runs them on Cisco's OpenH264
//! instead of the libx264 stand-in.
//!
//! Docs: apps/jstream-daemon/README.md#receiver-e2e

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Where Mesa installs its Vulkan drivers' manifests.
const ICD_DIR: &str = "/usr/share/vulkan/icd.d";

/// Runs the harness; `Ok(false)` when a test failed.
pub fn run(args: &[String], root: &Path) -> Result<bool> {
    let mut openh264 = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--openh264" => {
                let path = rest.next().context("--openh264 needs the library's path")?;
                let path = Path::new(path)
                    .canonicalize()
                    .with_context(|| format!("{path} doesn't exist"))?;
                openh264 = Some(path);
            }
            other => bail!("unknown argument {other}; usage: e2e-stream [--openh264 <library>]"),
        }
    }
    if !cfg!(target_os = "linux") {
        bail!("the receiver harness runs on Linux only");
    }
    for tool in ["ffmpeg", "ffprobe"] {
        let found = Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            bail!("`{tool}` isn't installed; install FFmpeg (e.g. `sudo apt-get install ffmpeg`)");
        }
    }
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(root).args([
        "test",
        "-p",
        "jstream-daemon",
        "--test",
        "receiver",
        "--locked",
        "--",
        "--ignored",
        "--test-threads=1",
    ]);
    if std::env::var_os("VK_DRIVER_FILES").is_none()
        && let Some(icd) = lavapipe()
    {
        eprintln!("xtask e2e-stream: Vulkan on lavapipe ({})", icd.display());
        cargo.env("VK_DRIVER_FILES", icd);
    }
    if let Some(library) = &openh264 {
        cargo.env("JSTREAM_E2E_OPENH264", library);
    } else {
        eprintln!("xtask e2e-stream: H.264 through the libx264 stand-in (no --openh264)");
    }
    let status = cargo.status().context("running cargo test")?;
    Ok(status.success())
}

/// Mesa's lavapipe manifest, if it is installed.
fn lavapipe() -> Option<std::path::PathBuf> {
    std::fs::read_dir(ICD_DIR)
        .ok()?
        .flatten()
        .find_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("lvp_icd") && name.ends_with(".json")).then(|| entry.path())
        })
}
