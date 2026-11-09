//! `cargo xtask dist-linux`: builds the Linux desktop app's `.deb` (ADR-0141 decision 5).
//! It builds the web UI (or takes a built one) and the release binaries, the desktop app
//! with its `webkitgtk` window, and lays them out as a Debian package: the programs and
//! the UI in `/usr/lib/jstream`, links to them in `/usr/bin`, an application-menu entry
//! and its icon, then `dpkg-deb` packs it. The package depends on the distribution's
//! WebKitGTK 4.1, so it installs on the Ubuntu and Debian releases that ship it. It is a
//! CI artifact; publishing it with a release, and signing it, are later decisions.
//!
//! Docs: docs/ARCHITECTURE.md#packaging

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::dist::{build_ui, copy, copy_dir, describe};

const USAGE: &str = "usage: cargo xtask dist-linux [--ui-dir <built UI folder>] [--out <folder>] [--revision <name>]";

/// The packages `cargo build` builds, and the features: the desktop app's window.
const PACKAGES: &[&str] = &["jstream-desktop", "jstream-daemon", "jstream-cli"];
const FEATURES: &str = "jstream-desktop/webkitgtk";

/// The programs the build leaves in `target/release`, each linked from `/usr/bin`.
const BINARIES: &[&str] = &["jstream", "jstream-daemon", "jstream-cli"];

/// Where the package keeps the programs and the UI: `jstream` finds the `ui` folder
/// beside itself (`/proc/self/exe` resolves the `/usr/bin` link).
const LIB_DIR: &str = "usr/lib/jstream";

/// The application id, the Tauri identifier: the desktop entry's and the icon's name.
const APP_ID: &str = "dev.jstream.desktop";

/// The package's name.
const PACKAGE: &str = "jstream";

/// The run-time libraries the window needs, as Debian and Ubuntu name them.
const DEPENDS: &str = "libwebkit2gtk-4.1-0, libgtk-3-0";

/// Repository files copied into the package's doc folder.
const LICENSES: &[&str] = &["LICENSE-MIT", "LICENSE-APACHE"];

/// Runs `cargo xtask dist-linux`.
pub fn run(args: &[String], root: &Path) -> Result<bool> {
    let mut ui_dir = None;
    let mut out = None;
    let mut revision = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .map(PathBuf::from)
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--ui-dir" => ui_dir = Some(value()?),
            "--out" => out = Some(value()?),
            "--revision" => revision = Some(value()?.to_string_lossy().into_owned()),
            _ => bail!("unknown argument `{arg}`\n{USAGE}"),
        }
    }
    if !cfg!(target_os = "linux") {
        bail!("the Linux package is built on Linux");
    }
    let arch = deb_arch(std::env::consts::ARCH)?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let out = out.unwrap_or_else(|| target.join("dist-linux"));
    let ui = match ui_dir {
        Some(dir) => dir,
        None => build_ui(root, &target.join("dist-ui"))?,
    };
    if !ui.join("index.html").is_file() {
        bail!(
            "`{}` holds no index.html: it is not a built UI",
            ui.display()
        );
    }
    build_binaries(root)?;
    let version = deb_version(
        env!("CARGO_PKG_VERSION"),
        revision.unwrap_or_else(|| describe(root)).as_str(),
    );
    let stage = out.join(format!("{PACKAGE}_{version}_{arch}"));
    if stage.exists() {
        fs::remove_dir_all(&stage).with_context(|| format!("removing {}", stage.display()))?;
    }
    lay_out(root, &target.join("release"), &ui, &stage)?;
    let deb = pack(&stage, &version, arch, &out)?;
    eprintln!("wrote {}", deb.display());
    Ok(true)
}

/// Writes `DEBIAN/control` into the laid-out `stage` and packs it with `dpkg-deb` into
/// `out`, returning the `.deb`.
fn pack(stage: &Path, version: &str, arch: &str, out: &Path) -> Result<PathBuf> {
    let installed_kib = size_kib(stage)?;
    let debian = stage.join("DEBIAN");
    fs::create_dir_all(&debian).with_context(|| format!("creating {}", debian.display()))?;
    fs::write(
        debian.join("control"),
        control(version, arch, installed_kib),
    )
    .context("writing DEBIAN/control")?;
    let deb = out.join(format!("{PACKAGE}_{version}_{arch}.deb"));
    let status = Command::new("dpkg-deb")
        .args(["--build", "--root-owner-group"])
        .arg(stage)
        .arg(&deb)
        .status()
        .context("running dpkg-deb (install the dpkg package)")?;
    if !status.success() {
        bail!("dpkg-deb failed: {status}");
    }
    Ok(deb)
}

/// Builds the release binaries, the desktop app with its window.
fn build_binaries(root: &Path) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "--release", "--locked", "--features", FEATURES])
        .current_dir(root);
    for package in PACKAGES {
        command.args(["-p", package]);
    }
    let status = command.status().context("running cargo build")?;
    if !status.success() {
        bail!("cargo build failed: {status}");
    }
    Ok(())
}

/// Lays the package's files out under `stage`, from the release binaries in `release`
/// and the built UI in `ui`.
fn lay_out(root: &Path, release: &Path, ui: &Path, stage: &Path) -> Result<()> {
    let lib = stage.join(LIB_DIR);
    let bin = stage.join("usr/bin");
    fs::create_dir_all(&lib).with_context(|| format!("creating {}", lib.display()))?;
    fs::create_dir_all(&bin).with_context(|| format!("creating {}", bin.display()))?;
    for binary in BINARIES {
        copy(&release.join(binary), &lib.join(binary))?;
        let link = bin.join(binary);
        std::os::unix::fs::symlink(format!("../lib/jstream/{binary}"), &link)
            .with_context(|| format!("linking {}", link.display()))?;
    }
    copy_dir(ui, &lib.join("ui"))?;
    let doc = stage.join("usr/share/doc").join(PACKAGE);
    fs::create_dir_all(&doc).with_context(|| format!("creating {}", doc.display()))?;
    for license in LICENSES {
        copy(&root.join(license), &doc.join(license))?;
    }
    let apps = stage.join("usr/share/applications");
    fs::create_dir_all(&apps).with_context(|| format!("creating {}", apps.display()))?;
    fs::write(apps.join(format!("{APP_ID}.desktop")), desktop_entry())
        .context("writing the desktop entry")?;
    let icons = stage.join("usr/share/icons/hicolor/256x256/apps");
    fs::create_dir_all(&icons).with_context(|| format!("creating {}", icons.display()))?;
    copy(
        &root.join("apps/jstream-desktop/icons/icon.png"),
        &icons.join(format!("{APP_ID}.png")),
    )?;
    Ok(())
}

/// The application-menu entry.
fn desktop_entry() -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=JStream\n\
         GenericName=Live streaming studio\n\
         Comment=Compose scenes and stream them live, by hand or with an agent\n\
         Exec=jstream\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=AudioVideo;Video;Recorder;\n"
    )
}

/// The `DEBIAN/control` file.
fn control(version: &str, arch: &str, installed_kib: u64) -> String {
    let mut text = String::new();
    let _ = write!(
        text,
        "Package: {PACKAGE}\n\
         Version: {version}\n\
         Architecture: {arch}\n\
         Maintainer: JStream <https://github.com/jeffmhopkins/JStream>\n\
         Installed-Size: {installed_kib}\n\
         Depends: {DEPENDS}\n\
         Recommends: xdg-desktop-portal, pipewire, mesa-vulkan-drivers\n\
         Section: video\n\
         Priority: optional\n\
         Homepage: https://github.com/jeffmhopkins/JStream\n\
         Description: live-streaming and compositing studio\n \
         JStream composes scenes and streams them live over RTMP(S), driven by hand\n \
         or by an AI agent over MCP. This package is the desktop app (jstream), the\n \
         engine without a window (jstream-daemon) and its command line (jstream-cli).\n"
    );
    text
}

/// The Debian version for the crate's `version` built from `revision`: a pre-release
/// sorts before its release (`0.1.0-alpha.4` → `0.1.0~alpha.4`), and the revision is
/// kept after a `+` with only the characters a Debian version allows.
pub(crate) fn deb_version(version: &str, revision: &str) -> String {
    let upstream = version.replacen('-', "~", 1);
    let revision: String = revision
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '.'
            }
        })
        .collect();
    let revision = revision.trim_matches('.');
    if revision.is_empty() {
        upstream
    } else {
        format!("{upstream}+{revision}")
    }
}

/// The Debian architecture for Rust's.
fn deb_arch(arch: &str) -> Result<&'static str> {
    Ok(match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("no Debian architecture for `{other}`"),
    })
}

/// The size of the files under `dir`, in KiB, rounded up, as `Installed-Size` takes it.
fn size_kib(dir: &Path) -> Result<u64> {
    let mut bytes = 0;
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len();
            }
        }
    }
    Ok(bytes.div_ceil(1024))
}

#[cfg(test)]
mod tests {
    use super::{BINARIES, LIB_DIR, control, deb_arch, deb_version, desktop_entry, lay_out, pack};
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    /// Lays out a package from stand-in binaries and a one-page UI, and, where
    /// `dpkg-deb` is installed, packs it and reads the package back.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_package_holds_the_programs_their_links_the_ui_and_the_menu_entry() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root");
        let scratch =
            std::env::temp_dir().join(format!("jstream-dist-linux-{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        let release = scratch.join("release");
        let ui = scratch.join("ui");
        let stage = scratch.join("stage");
        fs::create_dir_all(&release).expect("release");
        fs::create_dir_all(ui.join("assets")).expect("ui");
        for binary in BINARIES {
            fs::write(release.join(binary), b"#!/bin/sh\n").expect("a stand-in binary");
        }
        fs::write(ui.join("index.html"), b"<!doctype html>").expect("index");
        fs::write(ui.join("assets/app.js"), b"").expect("asset");
        lay_out(root, &release, &ui, &stage).expect("laid out");

        let lib = stage.join(LIB_DIR);
        for binary in BINARIES {
            assert!(lib.join(binary).is_file(), "{binary} in {LIB_DIR}");
            let link = fs::read_link(stage.join("usr/bin").join(binary)).expect("a link");
            assert_eq!(link, Path::new("../lib/jstream").join(binary));
            assert!(
                stage.join("usr/bin").join(binary).is_file(),
                "the link resolves"
            );
        }
        assert!(lib.join("ui/index.html").is_file());
        assert!(lib.join("ui/assets/app.js").is_file());
        assert!(
            stage
                .join("usr/share/applications/dev.jstream.desktop.desktop")
                .is_file()
        );
        assert!(
            stage
                .join("usr/share/icons/hicolor/256x256/apps/dev.jstream.desktop.png")
                .is_file()
        );
        assert!(stage.join("usr/share/doc/jstream/LICENSE-MIT").is_file());

        if Command::new("dpkg-deb").arg("--version").output().is_ok() {
            let deb = pack(&stage, "1.0.0~test", "amd64", &scratch).expect("packed");
            let info = Command::new("dpkg-deb")
                .arg("--field")
                .arg(&deb)
                .arg("Package")
                .output()
                .expect("dpkg-deb");
            assert_eq!(String::from_utf8_lossy(&info.stdout).trim(), "jstream");
            let contents = Command::new("dpkg-deb")
                .arg("--contents")
                .arg(&deb)
                .output()
                .expect("dpkg-deb");
            let contents = String::from_utf8_lossy(&contents.stdout);
            assert!(
                contents.contains("./usr/bin/jstream -> ../lib/jstream/jstream"),
                "{contents}"
            );
            assert!(
                contents.contains("root/root"),
                "files are root's: {contents}"
            );
        }
        let _ = fs::remove_dir_all(&scratch);
    }

    #[test]
    fn versions_sort_as_debian_sorts_them() {
        assert_eq!(
            deb_version("0.1.0-alpha.4.2", "v0.1.0-12-gabc123"),
            "0.1.0~alpha.4.2+v0.1.0.12.gabc123"
        );
        assert_eq!(deb_version("1.0.0", ""), "1.0.0");
        assert_eq!(deb_version("1.0.0", "--"), "1.0.0");
        assert_eq!(deb_version("1.0.0", "pr/1419"), "1.0.0+pr.1419");
    }

    #[test]
    fn architectures_have_their_debian_names() {
        assert_eq!(deb_arch("x86_64").expect("x86_64"), "amd64");
        assert_eq!(deb_arch("aarch64").expect("aarch64"), "arm64");
        assert!(deb_arch("riscv64").is_err());
    }

    #[test]
    fn the_control_file_names_webkitgtk_and_ends_its_description() {
        let text = control("1.0.0", "amd64", 42);
        assert!(text.contains("Depends: libwebkit2gtk-4.1-0, libgtk-3-0\n"));
        assert!(text.contains("Installed-Size: 42\n"));
        assert!(text.ends_with("(jstream-cli).\n"));
        // Every continuation line of the description starts with a space.
        let description = text.split("Description:").nth(1).expect("a description");
        assert!(
            description
                .lines()
                .skip(1)
                .all(|line| line.starts_with(' '))
        );
    }

    #[test]
    fn the_desktop_entry_starts_the_app_with_its_icon() {
        let entry = desktop_entry();
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("Exec=jstream\n"));
        assert!(entry.contains("Icon=dev.jstream.desktop\n"));
    }
}
