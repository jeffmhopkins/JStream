//! The Linux desktop app's AppImage (ADR-0141 decision 5). `cargo xtask dist-linux
//! --linuxdeploy <AppImage> --appimage-runtime <file>` builds it next to the `.deb`, from
//! the same release build, UI and browser helper's folder.
//!
//! The AppDir has `jstream` in `usr/bin` with `ui` and `browser` beside it, where it looks
//! for them, and linuxdeploy (its GTK plugin beside it) copies in the libraries it links,
//! GTK's modules, schemas and themes, and writes the AppImage over the pinned type 2 runtime.
//! WebKitGTK starts its web and network processes from a path compiled into
//! `libwebkit2gtk-4.1.so.0` (`/usr/lib/<multiarch>/webkit2gtk-4.1`), so the AppDir carries
//! those processes at the same path and a copy of the library with each `/usr` turned into
//! `././` (`/usr/lib/…` reads `././/lib/…`), and the AppImage's `AppRun` starts `jstream`
//! from the AppDir's `usr`, so the relative path resolves inside it (as Tauri's AppImages
//! do). `chrome-sandbox` is left out: an AppImage can't hold a setuid file, so its browser
//! sources need unprivileged user namespaces (ADR-0095).
//!
//! Docs: docs/ARCHITECTURE.md#packaging

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::browser_linux::CHROME_SANDBOX;
use crate::dist::{copy, copy_dir};

/// The program the AppImage starts: the desktop app, which runs the engine itself.
const PROGRAM: &str = "jstream";

/// WebKitGTK's library, by its soname.
const WEBKIT_LIB: &str = "libwebkit2gtk-4.1.so.0";

/// The folder beside the library that holds WebKitGTK's processes.
const WEBKIT_PROCESSES: &str = "webkit2gtk-4.1";

/// What `/usr` becomes in the library's copy: as long, and relative to the working
/// directory `AppRun` sets.
const RELATIVE_USR: &[u8] = b"././";

/// The AppImage's entry point. linuxdeploy keeps it as `AppRun.wrapped` and runs the GTK
/// plugin's hook first, which sets `APPDIR` too.
const APP_RUN: &str = "#!/bin/sh\n\
# JStream's AppImage (cargo xtask dist-linux). WebKitGTK finds its processes at\n\
# ././lib/..., so jstream starts from the AppDir's usr; $OWD is the caller's directory.\n\
APPDIR=\"${APPDIR:-$(dirname \"$(readlink -f \"$0\")\")}\"\n\
cd \"$APPDIR/usr\" || exit 1\n\
exec \"$APPDIR/usr/bin/jstream\" \"$@\"\n";

/// The tools the AppImage is built with, which CI downloads, pinned and checked.
pub(crate) struct Tools {
    /// linuxdeploy's AppImage, with `linuxdeploy-plugin-gtk.sh` beside it.
    pub(crate) linuxdeploy: PathBuf,
    /// The AppImage type 2 runtime the image starts with.
    pub(crate) runtime: PathBuf,
}

/// What goes into the AppImage, as laid out for the `.deb`.
pub(crate) struct Inputs<'a> {
    /// The workspace root, for the licences and the icon.
    pub(crate) root: &'a Path,
    /// `target/release`, which holds `jstream`.
    pub(crate) release: &'a Path,
    /// The built UI.
    pub(crate) ui: &'a Path,
    /// The browser helper's laid-out folder.
    pub(crate) browser: &'a Path,
    /// The application id, the desktop entry's and the icon's name.
    pub(crate) app_id: &'a str,
    /// The application-menu entry.
    pub(crate) desktop_entry: &'a str,
}

/// Builds `JStream-<version>-<arch>.AppImage` in `out`, with its AppDir in `work`, and
/// returns its path.
pub(crate) fn build(
    inputs: &Inputs<'_>,
    tools: &Tools,
    version: &str,
    work: &Path,
    out: &Path,
) -> Result<PathBuf> {
    let arch = std::env::consts::ARCH;
    if work.exists() {
        fs::remove_dir_all(work).with_context(|| format!("removing {}", work.display()))?;
    }
    let appdir = work.join("JStream.AppDir");
    lay_out(inputs, &appdir)?;
    let multiarch = format!("{arch}-linux-gnu");
    let processes = bundle_webkit(
        Path::new("/usr/lib").join(&multiarch).as_path(),
        &multiarch,
        &appdir,
    )?;
    let desktop = work.join(format!("{}.desktop", inputs.app_id));
    fs::write(&desktop, inputs.desktop_entry).context("writing the desktop entry")?;
    let icon = work.join(format!("{}.png", inputs.app_id));
    copy(
        &inputs.root.join("apps/jstream-desktop/icons/icon.png"),
        &icon,
    )?;
    let app_run = work.join("AppRun");
    fs::write(&app_run, APP_RUN).context("writing AppRun")?;
    crate::dist_linux::set_mode(&app_run, 0o755)?;
    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let image = out.join(format!("JStream-{version}-{arch}.AppImage"));
    let tools_dir = tools.linuxdeploy.parent().context("linuxdeploy's folder")?;
    let mut path = OsString::from(tools_dir);
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(":");
        path.push(rest);
    }
    let status = Command::new(&tools.linuxdeploy)
        // No FUSE on CI runners: linuxdeploy and appimagetool run extracted.
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .env("PATH", path)
        .env("ARCH", arch)
        .env("DEPLOY_GTK_VERSION", "3")
        // WebKitGTK's processes get their libraries from the AppDir's usr/lib.
        .env("ADDITIONAL_BIN_DIRS", &processes)
        .env("LDAI_RUNTIME_FILE", &tools.runtime)
        .env("LDAI_OUTPUT", &image)
        .env("LDAI_NO_APPSTREAM", "1")
        .arg("--appdir")
        .arg(&appdir)
        .arg("--desktop-file")
        .arg(&desktop)
        .arg("--icon-file")
        .arg(&icon)
        .arg("--custom-apprun")
        .arg(&app_run)
        .args(["--plugin", "gtk", "--output", "appimage"])
        .status()
        .with_context(|| format!("running {}", tools.linuxdeploy.display()))?;
    if !status.success() {
        bail!("linuxdeploy failed: {status}");
    }
    if !image.is_file() {
        bail!("linuxdeploy wrote no {}", image.display());
    }
    Ok(image)
}

/// Lays out the AppDir's own files: the program with `ui` and `browser` beside it (no
/// `chrome-sandbox`), and the licences.
fn lay_out(inputs: &Inputs<'_>, appdir: &Path) -> Result<()> {
    let bin = appdir.join("usr/bin");
    fs::create_dir_all(&bin).with_context(|| format!("creating {}", bin.display()))?;
    copy(&inputs.release.join(PROGRAM), &bin.join(PROGRAM))?;
    copy_dir(inputs.ui, &bin.join("ui"))?;
    let browser = bin.join("browser");
    copy_dir(inputs.browser, &browser)?;
    let sandbox = browser.join(CHROME_SANDBOX);
    if sandbox.exists() {
        fs::remove_file(&sandbox).with_context(|| format!("removing {}", sandbox.display()))?;
    }
    let doc = appdir.join("usr/share/doc/jstream");
    fs::create_dir_all(&doc).with_context(|| format!("creating {}", doc.display()))?;
    for license in ["LICENSE-MIT", "LICENSE-APACHE"] {
        copy(&inputs.root.join(license), &doc.join(license))?;
    }
    Ok(())
}

/// Copies WebKitGTK's processes from `lib_dir` (the system's `/usr/lib/<multiarch>`) into
/// the AppDir at the same path, and the library into its `usr/lib` with its paths made
/// relative. Returns the processes' folder in the AppDir.
fn bundle_webkit(lib_dir: &Path, multiarch: &str, appdir: &Path) -> Result<PathBuf> {
    let processes = lib_dir.join(WEBKIT_PROCESSES);
    if !processes.join("WebKitWebProcess").is_file() {
        bail!(
            "{} holds no WebKitWebProcess (install libwebkit2gtk-4.1-0)",
            processes.display()
        );
    }
    let relative = lib_dir
        .strip_prefix("/")
        .context("an absolute library folder")?;
    let bundled = appdir.join(relative).join(WEBKIT_PROCESSES);
    copy_dir(&processes, &bundled)?;
    let lib = lib_dir.join(WEBKIT_LIB);
    let mut bytes = fs::read(&lib).with_context(|| format!("reading {}", lib.display()))?;
    let expected = format!("/usr/lib/{multiarch}/{WEBKIT_PROCESSES}");
    if relocate(&mut bytes, expected.as_bytes()) == 0 {
        bail!(
            "{} doesn't name {expected}: its processes are elsewhere, so the AppImage can't \
             find them",
            lib.display()
        );
    }
    let target = appdir.join("usr/lib").join(WEBKIT_LIB);
    fs::create_dir_all(appdir.join("usr/lib")).context("creating usr/lib")?;
    fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
    crate::dist_linux::set_mode(&target, 0o755)?;
    Ok(bundled)
}

/// Turns each `/usr` in `bytes` into `././`, in place, and returns how many of them began
/// `expected` (the processes' folder), which must be found for the copy to work.
fn relocate(bytes: &mut [u8], expected: &[u8]) -> usize {
    let mut found = 0;
    let mut at = 0;
    while at + 4 <= bytes.len() {
        if &bytes[at..at + 4] == b"/usr" {
            if bytes[at..].starts_with(expected) {
                found += 1;
            }
            bytes[at..at + 4].copy_from_slice(RELATIVE_USR);
            at += 4;
        } else {
            at += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{APP_RUN, Inputs, lay_out, relocate};
    use std::fs;
    use std::path::Path;

    #[test]
    fn the_library_s_paths_turn_relative_and_keep_their_length() {
        let mut bytes =
            b"\0/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1\0/usr/share/x\0/opt/usr\0".to_vec();
        let length = bytes.len();
        let found = relocate(&mut bytes, b"/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1");
        assert_eq!(found, 1);
        assert_eq!(bytes.len(), length);
        assert_eq!(
            bytes,
            b"\0././/lib/x86_64-linux-gnu/webkit2gtk-4.1\0././/share/x\0/opt././\0".to_vec()
        );
        let mut other = b"/usr/lib/elsewhere\0".to_vec();
        assert_eq!(
            relocate(&mut other, b"/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1"),
            0
        );
    }

    #[test]
    fn app_run_starts_jstream_from_usr() {
        assert!(APP_RUN.starts_with("#!/bin/sh\n"));
        assert!(APP_RUN.contains("cd \"$APPDIR/usr\" || exit 1\n"));
        assert!(APP_RUN.ends_with("exec \"$APPDIR/usr/bin/jstream\" \"$@\"\n"));
    }

    #[test]
    fn the_appdir_holds_the_program_its_ui_and_browser_but_no_setuid_sandbox() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root");
        let scratch = std::env::temp_dir().join(format!("jstream-appimage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        let release = scratch.join("release");
        let ui = scratch.join("ui");
        let browser = scratch.join("browser");
        fs::create_dir_all(&release).expect("release");
        fs::create_dir_all(&ui).expect("ui");
        fs::create_dir_all(&browser).expect("browser");
        fs::write(release.join("jstream"), b"").expect("a stand-in program");
        fs::write(ui.join("index.html"), b"<!doctype html>").expect("index");
        for file in ["jstream-browser-helper", "libcef.so", "chrome-sandbox"] {
            fs::write(browser.join(file), b"").expect("a stand-in CEF file");
        }
        let appdir = scratch.join("JStream.AppDir");
        let inputs = Inputs {
            root,
            release: &release,
            ui: &ui,
            browser: &browser,
            app_id: "dev.jstream.desktop",
            desktop_entry: "",
        };
        lay_out(&inputs, &appdir).expect("laid out");
        let bin = appdir.join("usr/bin");
        assert!(bin.join("jstream").is_file());
        assert!(bin.join("ui/index.html").is_file());
        assert!(bin.join("browser/jstream-browser-helper").is_file());
        assert!(bin.join("browser/libcef.so").is_file());
        assert!(!bin.join("browser/chrome-sandbox").exists());
        assert!(
            appdir
                .join("usr/share/doc/jstream/LICENSE-APACHE")
                .is_file()
        );
        let _ = fs::remove_dir_all(&scratch);
    }
}
