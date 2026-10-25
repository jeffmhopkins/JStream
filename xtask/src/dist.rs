//! `cargo xtask dist`: builds the portable Windows zip (ADR-0020). It builds the web UI
//! (or takes a built one), builds the release binaries with the C runtime linked
//! statically and browser sources on, builds the sandboxed browser helper with CEF, refuses
//! any binary that still imports a Visual C++ runtime DLL (or a host that loads CEF), and
//! zips one folder that runs from wherever it is unzipped, with the helper and CEF's
//! runtime in its `browser` folder.
//!
//! Docs: docs/ARCHITECTURE.md#packaging, docs/adr/0048-sandboxed-browser-helper.md

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The packages `cargo build` builds for the zip.
const PACKAGES: &[&str] = &[
    "jstream-desktop",
    "jstream-daemon",
    "jstream-cli",
    "jstream-vcam-windows",
];

/// The hosts' features for browser sources: their CEF backend, which loads no CEF itself.
const HOST_FEATURES: &str = "jstream-desktop/cef,jstream-daemon/cef";

/// The browser helper's package. It is built on its own: built with the hosts, Cargo
/// would unify `jstream-browser`'s `helper` feature into them and link CEF into the
/// engine's process (ADR-0048).
const HELPER_PACKAGE: &str = "jstream-browser-helper";

/// The helper's DLL as Cargo names it, and the two names it ships under.
const HELPER_BUILT: &str = "jstream_browser_helper.dll";
const HELPER_DLL: &str = "jstream-browser-helper.dll";
const HELPER_EXE: &str = "jstream-browser-helper.exe";

/// CEF's bootstrap exe in its distribution, which ships renamed [`HELPER_EXE`].
const BOOTSTRAP: &str = "bootstrap.exe";

/// The folder in the zip that holds the helper and CEF's runtime, where the hosts look.
const BROWSER_DIR: &str = "browser";

/// The files in CEF's distribution folder that only building needs, and the console
/// variant of the bootstrap: none of them ship.
const CEF_BUILD_ONLY: &[&str] = &["archive.json", "CMakeLists.txt", "bootstrapc.exe"];

/// What the hosts must not import: CEF runs only in the helper (ADR-0048).
const CEF_DLL: &str = "libcef.dll";

/// The files the build leaves in `target/release` that go in the zip, beside each other.
const BINARIES: &[&str] = &[
    "jstream.exe",
    "jstream-daemon.exe",
    "jstream-cli.exe",
    "jstream_vcam_windows.dll",
];

/// Repository files copied into the zip as they are.
const LICENSES: &[&str] = &["LICENSE-MIT", "LICENSE-APACHE"];

/// The folder in the zip that holds the web UI, where `jstream.exe` looks by default.
const UI_DIR: &str = "ui";

/// The flag `.cargo/config.toml` sets for the MSVC targets, repeated here because a
/// `RUSTFLAGS` variable (CI sets one) replaces the config's flags instead of adding to them.
const CRT_STATIC: &str = "-Ctarget-feature=+crt-static";

/// The Visual C++ runtime DLLs a binary must not import: they come only with the Visual
/// C++ redistributable, and a statically linked runtime imports none of them. The
/// Universal CRT (`ucrtbase.dll`, `api-ms-win-crt-*`) is allowed: it is part of Windows
/// 10 and 11, and `jstream.exe` imports it even with `crt-static`, from a prebuilt static
/// library built with `/MD` (most likely WebView2's loader). So is the system's own
/// `msvcrt.dll`, which every Windows ships: `msvcr` refuses only the versioned
/// redistributable ones (`msvcr100.dll`, `msvcr120.dll`).
const CRT_DLLS: &[&str] = &["vcruntime", "msvcp", "concrt", "vccorlib"];

const USAGE: &str = "usage: cargo xtask dist [--ui-dir <built UI folder>] [--out <folder>] \
     [--revision <name>] [--no-browser]";

/// The text file at the top of the zip.
const README: &str = "\
JStream, portable

Run jstream.exe. Nothing is installed, and nothing needs admin rights.

Settings, scenes and logs go in %APPDATA%\\JStream. To keep them in this folder instead
(for a USB stick), create an empty file named `portable` (no extension) next to
jstream.exe: they then go in the `data` folder here.

jstream-daemon.exe is the engine without a window, and jstream-cli.exe controls it.
jstream_vcam_windows.dll is the virtual camera. Registering it is optional: one admin
prompt, only when you choose to.

The browser folder holds the browser helper that runs web-page sources, sandboxed, with
the Chromium Embedded Framework it uses (CEF; its licences are in browser\\CREDITS.html,
its version in browser\\CEF-VERSION.txt). Keep it next to jstream.exe.

Windows may warn that this app is from an unknown publisher: the alpha is not signed.
";

/// Runs `cargo xtask dist`.
pub fn run(args: &[String], root: &Path) -> Result<bool> {
    let mut ui_dir = None;
    let mut out = None;
    let mut revision = None;
    let mut browser = true;
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
            "--no-browser" => browser = false,
            _ => bail!("unknown argument `{arg}`\n{USAGE}"),
        }
    }
    if !cfg!(all(windows, target_env = "msvc")) {
        bail!("the portable zip is built on Windows with the MSVC toolchain");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let out = out.unwrap_or_else(|| target.join("dist"));
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
    build_binaries(root, browser)?;
    let cef = if browser {
        Some(build_helper(root)?)
    } else {
        None
    };
    let release = target.join("release");
    for binary in BINARIES {
        check_no_crt_imports(&release.join(binary))?;
        check_no_cef_imports(&release.join(binary))?;
    }
    if cef.is_some() {
        check_no_crt_imports(&release.join(HELPER_BUILT))?;
    }
    let name = format!(
        "jstream-{}-windows-{}",
        revision.unwrap_or_else(|| describe(root)),
        arch()
    );
    let stage = out.join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage).with_context(|| format!("removing {}", stage.display()))?;
    }
    fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
    for binary in BINARIES {
        copy(&release.join(binary), &stage.join(binary))?;
    }
    for license in LICENSES {
        copy(&root.join(license), &stage.join(license))?;
    }
    fs::write(stage.join("README.txt"), README.replace('\n', "\r\n"))
        .context("writing README.txt")?;
    copy_dir(&ui, &stage.join(UI_DIR))?;
    if let Some(cef) = &cef {
        stage_browser(cef, &release.join(HELPER_BUILT), &stage.join(BROWSER_DIR))?;
    }
    let zip = out.join(format!("{name}.zip"));
    write_zip(&zip, &name, &files_under(&stage)?)?;
    eprintln!("wrote {}", zip.display());
    Ok(true)
}

/// Builds the web UI for release with trunk into `dist` and returns it.
fn build_ui(root: &Path, dist: &Path) -> Result<PathBuf> {
    let status = Command::new("trunk")
        .args(["build", "--release", "--locked", "--dist"])
        .arg(dist)
        .current_dir(root.join("crates/jstream-ui"))
        .status()
        .context("running trunk (install it, or pass --ui-dir <built UI folder>)")?;
    if !status.success() {
        bail!("trunk build failed: {status}");
    }
    Ok(dist.to_owned())
}

/// A release `cargo build` in `root`, with the C runtime linked statically.
fn cargo_build(root: &Path) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "--release", "--locked"])
        .current_dir(root);
    if let Some(flags) = with_crt_static(std::env::var("RUSTFLAGS").ok()) {
        command.env("RUSTFLAGS", flags);
    }
    command
}

/// Builds the release binaries, with the hosts' browser sources on when `browser` is.
fn build_binaries(root: &Path, browser: bool) -> Result<()> {
    let mut command = cargo_build(root);
    command.args(PACKAGES.iter().flat_map(|package| ["-p", package]));
    if browser {
        command.args(["--features", HOST_FEATURES]);
    }
    let status = command.status().context("running cargo build")?;
    if !status.success() {
        bail!("cargo build failed: {status}");
    }
    Ok(())
}

/// Builds the browser helper's DLL with CEF, which downloads CEF's distribution and builds
/// its wrapper (CMake and Ninja on the path), and returns the distribution's folder.
fn build_helper(root: &Path) -> Result<PathBuf> {
    let output = cargo_build(root)
        .args(["-p", HELPER_PACKAGE, "--features", "cef"])
        .arg("--message-format=json-render-diagnostics")
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("running cargo build for the browser helper")?;
    if !output.status.success() {
        bail!(
            "cargo build of the browser helper failed: {} (it needs CMake and Ninja; \
             --no-browser builds a zip without browser sources)",
            output.status
        );
    }
    let messages = String::from_utf8_lossy(&output.stdout);
    cef_dir(&messages, |dir| dir.join(BOOTSTRAP).is_file())
        .context("the build named no CEF distribution folder holding bootstrap.exe")
}

/// CEF's distribution folder, from Cargo's JSON `messages`: the link search path of
/// `cef-dll-sys`'s build script that `is_cef` accepts (its other one is the wrapper's
/// CMake output).
fn cef_dir(messages: &str, is_cef: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "build-script-executed")
        .filter(|message| {
            message["package_id"]
                .as_str()
                .is_some_and(|id| id.contains("cef-dll-sys"))
        })
        .flat_map(|message| match &message["linked_paths"] {
            serde_json::Value::Array(paths) => paths.clone(),
            _ => Vec::new(),
        })
        .filter_map(|path| {
            let path = path.as_str()?;
            Some(PathBuf::from(path.strip_prefix("native=").unwrap_or(path)))
        })
        .find(|dir| is_cef(dir))
}

/// Whether the file `name`, at the top of CEF's distribution folder, ships: everything
/// but what only building needs (import libraries and [`CEF_BUILD_ONLY`]) and the
/// bootstrap exe, which ships renamed.
fn ships_from_cef(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !(lower.ends_with(".lib")
        || lower == BOOTSTRAP
        || CEF_BUILD_ONLY
            .iter()
            .any(|only| only.eq_ignore_ascii_case(name)))
}

/// What `CEF-VERSION.txt` says: the CEF distribution the runtime came from, as named in
/// its `archive.json` (CEF's and Chromium's versions), so the zip names the revisions
/// its LGPL parts were built from (ADR-0021).
fn cef_version_text(archive_json: &str) -> Result<String> {
    let archive: serde_json::Value =
        serde_json::from_str(archive_json).context("reading CEF's archive.json")?;
    let name = archive["name"]
        .as_str()
        .context("CEF's archive.json names no archive")?;
    let name = name.strip_suffix(".tar.bz2").unwrap_or(name);
    Ok(format!(
        "{name}\n\nThe Chromium Embedded Framework, from https://cef-builds.spotifycdn.com.\n\
         Its sources and Chromium's: https://bitbucket.org/chromiumembedded/cef.\n"
    ))
}

/// Lays the browser helper out in `to` as it ships: CEF's runtime files from `cef`, its
/// `locales`, its bootstrap exe renamed [`HELPER_EXE`] beside the helper's DLL `dll`
/// renamed [`HELPER_DLL`], and `CEF-VERSION.txt`.
fn stage_browser(cef: &Path, dll: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(cef).with_context(|| format!("reading {}", cef.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file() && name.to_str().is_some_and(ships_from_cef) {
            copy(&entry.path(), &to.join(&name))?;
        }
    }
    copy_dir(&cef.join("locales"), &to.join("locales"))?;
    copy(&cef.join(BOOTSTRAP), &to.join(HELPER_EXE))?;
    copy(dll, &to.join(HELPER_DLL))?;
    let archive = fs::read_to_string(cef.join("archive.json"))
        .with_context(|| format!("reading {}", cef.join("archive.json").display()))?;
    let text = cef_version_text(&archive)?;
    fs::write(to.join("CEF-VERSION.txt"), text.replace('\n', "\r\n"))
        .context("writing CEF-VERSION.txt")
}

/// The `RUSTFLAGS` to build with: `None` keeps `.cargo/config.toml`'s flags (no variable
/// set), else the variable's flags plus [`CRT_STATIC`], which they would otherwise drop.
fn with_crt_static(rustflags: Option<String>) -> Option<String> {
    let flags = rustflags?;
    if flags.split_whitespace().any(|flag| flag == CRT_STATIC) {
        return Some(flags);
    }
    Some(format!("{flags} {CRT_STATIC}").trim_start().to_owned())
}

/// Fails when the PE image at `path` imports a Visual C++ runtime DLL, so the zip never needs
/// the Visual C++ redistributable.
fn check_no_crt_imports(path: &Path) -> Result<()> {
    let image = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let dlls = imported_dlls(&image).with_context(|| format!("reading {}", path.display()))?;
    let crt: Vec<&String> = dlls.iter().filter(|dll| is_crt(dll)).collect();
    if !crt.is_empty() {
        bail!(
            "{} imports the Visual C++ runtime ({crt:?}): it must link it statically (crt-static)",
            path.display()
        );
    }
    Ok(())
}

/// Fails when the PE image at `path` imports CEF: the hosts only start the browser helper,
/// and CEF runs in it alone (ADR-0048).
fn check_no_cef_imports(path: &Path) -> Result<()> {
    let image = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let dlls = imported_dlls(&image).with_context(|| format!("reading {}", path.display()))?;
    if dlls.iter().any(|dll| dll == CEF_DLL) {
        bail!(
            "{} imports {CEF_DLL}: CEF must run only in the browser helper (ADR-0048)",
            path.display()
        );
    }
    Ok(())
}

/// Whether `dll` (lowercase) is a Visual C++ runtime DLL.
fn is_crt(dll: &str) -> bool {
    let versioned_msvcr = dll
        .strip_prefix("msvcr")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    versioned_msvcr || CRT_DLLS.iter().any(|prefix| dll.starts_with(prefix))
}

/// The DLLs a PE image imports, at load time or delay-loaded, lowercase.
fn imported_dlls(image: &[u8]) -> Result<Vec<String>> {
    let pe = Pe::parse(image)?;
    let mut dlls = pe.dll_names(Pe::IMPORT, 20, 12)?;
    dlls.extend(pe.dll_names(Pe::DELAY_IMPORT, 32, 4)?);
    Ok(dlls)
}

/// The parts of a PE image the import check reads.
struct Pe<'a> {
    image: &'a [u8],
    /// The offset of the first data directory entry.
    directories: usize,
    /// The data directory entries the header declares.
    directory_count: usize,
    /// The section headers: (virtual address, virtual size, raw offset, raw size).
    sections: Vec<(usize, usize, usize, usize)>,
}

impl<'a> Pe<'a> {
    /// The import directory's index in the data directories.
    const IMPORT: usize = 1;
    /// The delay-import directory's index.
    const DELAY_IMPORT: usize = 13;

    fn parse(image: &'a [u8]) -> Result<Self> {
        let pe = u32_at(image, 0x3C)?;
        if image.get(pe..pe + 4) != Some(b"PE\0\0".as_slice()) {
            bail!("not a PE image");
        }
        let section_count = u16_at(image, pe + 6)?;
        let optional = pe + 24;
        let optional_size = u16_at(image, pe + 20)?;
        let (directories, count_at) = match u16_at(image, optional)? {
            0x10B => (optional + 96, optional + 92),
            0x20B => (optional + 112, optional + 108),
            magic => bail!("unknown optional header magic {magic:#x}"),
        };
        let table = optional + optional_size;
        let sections = (0..section_count)
            .map(|n| {
                let at = table + n * 40;
                Ok((
                    u32_at(image, at + 12)?,
                    u32_at(image, at + 8)?,
                    u32_at(image, at + 20)?,
                    u32_at(image, at + 16)?,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            image,
            directories,
            directory_count: u32_at(image, count_at)?,
            sections,
        })
    }

    /// The file offset of `rva`.
    fn offset(&self, rva: usize) -> Result<usize> {
        self.sections
            .iter()
            .find(|(va, size, _, raw)| rva >= *va && rva < va + (*size).max(*raw))
            .map(|(va, _, raw_offset, _)| rva - va + raw_offset)
            .with_context(|| format!("RVA {rva:#x} is in no section"))
    }

    /// The DLL names in data directory `index`, a table of `stride`-byte descriptors
    /// ending in one whose name RVA (at `name_at`) is zero; lowercase.
    fn dll_names(&self, index: usize, stride: usize, name_at: usize) -> Result<Vec<String>> {
        if index >= self.directory_count {
            return Ok(Vec::new());
        }
        let rva = u32_at(self.image, self.directories + index * 8)?;
        if rva == 0 {
            return Ok(Vec::new());
        }
        let start = self.offset(rva)?;
        let mut names = Vec::new();
        for n in 0.. {
            let name = u32_at(self.image, start + n * stride + name_at)?;
            if name == 0 {
                break;
            }
            let at = self.offset(name)?;
            let rest = self.image.get(at..).context("a name past the end")?;
            let end = rest
                .iter()
                .position(|b| *b == 0)
                .context("an unterminated name")?;
            names.push(String::from_utf8_lossy(&rest[..end]).to_ascii_lowercase());
        }
        Ok(names)
    }
}

fn u16_at(image: &[u8], at: usize) -> Result<usize> {
    let bytes = image.get(at..at + 2).context("truncated PE image")?;
    Ok(usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
}

fn u32_at(image: &[u8], at: usize) -> Result<usize> {
    let bytes = image.get(at..at + 4).context("truncated PE image")?;
    let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    usize::try_from(value).context("an offset past usize")
}

/// The commit the zip is built from, for its name when `--revision` names none:
/// `git describe --always --dirty`. CI passes `--revision`, because a pull request's
/// checkout is GitHub's merge commit, which is in no branch's history.
fn describe(root: &Path) -> String {
    Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "dev".to_owned())
}

/// The architecture in the zip's name, in Windows' words.
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    Ok(())
}

/// Copies the folder `from` to `to`, which must not exist yet.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Every file under `dir`, as (path in the zip with `/` separators, file), sorted so the
/// zip's order doesn't depend on the file system.
fn files_under(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .with_context(|| format!("{:?} is not UTF-8", entry.path()))?;
            let path = format!("{prefix}{name}");
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), &format!("{path}/"), out)?;
            } else {
                out.push((path, entry.path()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files)?;
    files.sort();
    Ok(files)
}

/// The DOS date in every entry, 1980-01-01, so a zip depends only on its files.
const DOS_DATE: u16 = (1 << 5) | 1;
/// General purpose flag bit 11: names are UTF-8.
const UTF8_NAMES: u16 = 1 << 11;
/// Version 2.0: deflate.
const VERSION: u16 = 20;
const DEFLATE: u16 = 8;

/// Writes `files` (name in the zip, file) to a zip at `path`, each name under the folder
/// `top`, deflated. No zip64: it fails past 4 GiB or 65,535 files.
fn write_zip(path: &Path, top: &str, files: &[(String, PathBuf)]) -> Result<()> {
    let mut entries = Vec::new();
    for (name, file) in files {
        let data = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        entries.push((format!("{top}/{name}"), data));
    }
    let bytes = zip_bytes(&entries)?;
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// The bytes of a zip holding `entries` (name, contents), deflated, in the given order.
fn zip_bytes(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let too_big = || anyhow::anyhow!("the zip would need zip64 (over 4 GiB or 65,535 files)");
    let u32_of = |n: usize| u32::try_from(n).map_err(|_| too_big());
    let u16_of = |n: usize| u16::try_from(n).map_err(|_| too_big());
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(data)?;
        let deflated = encoder.finish()?;
        let offset = u32_of(out.len())?;
        // The fields both headers share, from the version needed on.
        let mut common = Vec::new();
        for field in [VERSION, UTF8_NAMES, DEFLATE, 0, DOS_DATE] {
            common.extend(field.to_le_bytes());
        }
        for field in [crc.sum(), u32_of(deflated.len())?, u32_of(data.len())?] {
            common.extend(field.to_le_bytes());
        }
        common.extend(u16_of(name.len())?.to_le_bytes());
        common.extend(0u16.to_le_bytes()); // extra field length

        out.extend(0x0403_4B50u32.to_le_bytes());
        out.extend(&common);
        out.extend(name.as_bytes());
        out.extend(&deflated);

        central.extend(0x0201_4B50u32.to_le_bytes());
        central.extend(VERSION.to_le_bytes()); // made by: MS-DOS, 2.0
        central.extend(&common);
        // Comment length, disk number, internal and external attributes.
        for field in [0u16, 0, 0, 0, 0] {
            central.extend(field.to_le_bytes());
        }
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let central_offset = u32_of(out.len())?;
    let count = u16_of(entries.len())?;
    out.extend(&central);
    out.extend(0x0605_4B50u32.to_le_bytes());
    for field in [0u16, 0, count, count] {
        out.extend(field.to_le_bytes());
    }
    out.extend(u32_of(central.len())?.to_le_bytes());
    out.extend(central_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // comment length
    u32_of(out.len())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        CRT_STATIC, Pe, cef_dir, cef_version_text, imported_dlls, is_crt, ships_from_cef,
        with_crt_static, zip_bytes,
    };
    use std::io::Read;
    use std::path::{Path, PathBuf};

    fn u16_at(bytes: &[u8], at: usize) -> usize {
        usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
    }

    fn u32_at(bytes: &[u8], at: usize) -> usize {
        let value = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        usize::try_from(value).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Reads a zip back through its central directory, checking each local header
    /// against it and each entry's CRC, and returns (name, contents).
    fn unzip(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let end = zip.len() - 22;
        assert_eq!(u32_at(zip, end), 0x0605_4B50, "end of central directory");
        let count = u16_at(zip, end + 10);
        assert_eq!(count, u16_at(zip, end + 8));
        let size = u32_at(zip, end + 12);
        let mut at = u32_at(zip, end + 16);
        assert_eq!(
            at + size,
            end,
            "the central directory ends where the end record starts"
        );
        let mut entries = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(zip, at), 0x0201_4B50);
            assert_eq!(u16_at(zip, at + 10), 8, "deflate");
            let crc = u32_at(zip, at + 16);
            let compressed = u32_at(zip, at + 20);
            let length = u32_at(zip, at + 24);
            let name_length = u16_at(zip, at + 28);
            let extra = u16_at(zip, at + 30) + u16_at(zip, at + 32);
            let local = u32_at(zip, at + 42);
            let name = String::from_utf8(zip[at + 46..at + 46 + name_length].to_vec())
                .unwrap_or_else(|e| panic!("{e}"));
            at += 46 + name_length + extra;

            assert_eq!(u32_at(zip, local), 0x0403_4B50);
            assert_eq!(u32_at(zip, local + 14), crc, "{name}: local CRC");
            assert_eq!(u32_at(zip, local + 18), compressed);
            assert_eq!(u32_at(zip, local + 22), length);
            assert_eq!(&zip[local + 30..local + 30 + name_length], name.as_bytes());
            let data_at = local + 30 + name_length + u16_at(zip, local + 28);
            let mut data = Vec::new();
            flate2::read::DeflateDecoder::new(&zip[data_at..data_at + compressed])
                .read_to_end(&mut data)
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(data.len(), length);
            let mut sum = flate2::Crc::new();
            sum.update(&data);
            assert_eq!(usize::try_from(sum.sum()).ok(), Some(crc), "{name}: CRC");
            entries.push((name, data));
        }
        assert_eq!(at, end);
        entries
    }

    #[test]
    fn a_zip_reads_back_as_written() {
        let entries = vec![
            ("jstream/README.txt".to_owned(), b"hello\r\n".to_vec()),
            ("jstream/empty".to_owned(), Vec::new()),
            (
                "jstream/ui/app_bg.wasm".to_owned(),
                (0..100_000u32)
                    .flat_map(|n| (n % 251).to_le_bytes())
                    .collect(),
            ),
        ];
        let zip = zip_bytes(&entries).unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(unzip(&zip), entries);
        assert_eq!(
            zip_bytes(&entries).unwrap_or_else(|e| panic!("{e:#}")),
            zip,
            "the same files make the same zip"
        );
    }

    #[test]
    fn an_empty_zip_is_just_the_end_record() {
        let zip = zip_bytes(&[]).unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(zip.len(), 22);
        assert!(unzip(&zip).is_empty());
    }

    #[test]
    fn rustflags_keep_crt_static() {
        assert_eq!(with_crt_static(None), None, "the config's flags apply");
        assert_eq!(
            with_crt_static(Some("-D warnings".to_owned())).as_deref(),
            Some("-D warnings -Ctarget-feature=+crt-static")
        );
        assert_eq!(
            with_crt_static(Some(String::new())).as_deref(),
            Some(CRT_STATIC)
        );
        assert_eq!(
            with_crt_static(Some(CRT_STATIC.to_owned())).as_deref(),
            Some(CRT_STATIC)
        );
    }

    #[test]
    fn crt_dlls_are_recognised() {
        for dll in [
            "vcruntime140.dll",
            "vcruntime140_1.dll",
            "msvcp140.dll",
            "vccorlib140.dll",
            "msvcr120.dll",
        ] {
            assert!(is_crt(dll), "{dll}");
        }
        for dll in [
            "kernel32.dll",
            "mfplat.dll",
            "d3d11.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "msvcrt.dll",
        ] {
            assert!(!is_crt(dll), "{dll}");
        }
    }

    /// A minimal PE32+ image: one section at RVA 0x1000 (file offset 0x200) holding an
    /// import table naming `imports` and a delay-import table naming `delayed`.
    fn pe(imports: &[&str], delayed: &[&str]) -> Vec<u8> {
        const SECTION_RVA: usize = 0x1000;
        const SECTION_RAW: usize = 0x200;
        let put16 = |image: &mut Vec<u8>, at: usize, value: usize| {
            let value = u16::try_from(value).unwrap_or_else(|e| panic!("{e}"));
            image[at..at + 2].copy_from_slice(&value.to_le_bytes());
        };
        let put32 = |image: &mut Vec<u8>, at: usize, value: usize| {
            let value = u32::try_from(value).unwrap_or_else(|e| panic!("{e}"));
            image[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        // The section: the two descriptor tables, then the names.
        let mut section = vec![0u8; (imports.len() + 1) * 20 + (delayed.len() + 1) * 32];
        let delay_table = (imports.len() + 1) * 20;
        for (n, name) in imports.iter().enumerate() {
            let rva = SECTION_RVA + section.len();
            put32(&mut section, n * 20 + 12, rva);
            section.extend(name.as_bytes());
            section.push(0);
        }
        for (n, name) in delayed.iter().enumerate() {
            let rva = SECTION_RVA + section.len();
            put32(&mut section, delay_table + n * 32 + 4, rva);
            section.extend(name.as_bytes());
            section.push(0);
        }
        let mut image = vec![0u8; SECTION_RAW];
        let pe = 0x80;
        put32(&mut image, 0x3C, pe);
        image[pe..pe + 4].copy_from_slice(b"PE\0\0");
        put16(&mut image, pe + 6, 1); // one section
        let optional = pe + 24;
        let optional_size = 112 + 16 * 8;
        put16(&mut image, pe + 20, optional_size);
        put16(&mut image, optional, 0x20B);
        put32(&mut image, optional + 108, 16); // data directories
        let directories = optional + 112;
        put32(&mut image, directories + Pe::IMPORT * 8, SECTION_RVA);
        if !delayed.is_empty() {
            put32(
                &mut image,
                directories + Pe::DELAY_IMPORT * 8,
                SECTION_RVA + delay_table,
            );
        }
        let header = optional + optional_size;
        put32(&mut image, header + 8, section.len());
        put32(&mut image, header + 12, SECTION_RVA);
        put32(&mut image, header + 16, section.len());
        put32(&mut image, header + 20, SECTION_RAW);
        image.extend(section);
        image
    }

    #[test]
    fn imports_and_delay_imports_are_read() {
        let image = pe(&["KERNEL32.dll", "VCRUNTIME140.dll"], &["mfplat.dll"]);
        assert_eq!(
            imported_dlls(&image).unwrap_or_else(|e| panic!("{e:#}")),
            ["kernel32.dll", "vcruntime140.dll", "mfplat.dll"]
        );
        let image = pe(&["KERNEL32.dll"], &[]);
        assert_eq!(
            imported_dlls(&image).unwrap_or_else(|e| panic!("{e:#}")),
            ["kernel32.dll"]
        );
    }

    #[test]
    fn a_broken_image_is_an_error() {
        assert!(imported_dlls(b"MZ").is_err());
        assert!(imported_dlls(&[0u8; 256]).is_err());
        let mut image = pe(&["KERNEL32.dll"], &[]);
        image.truncate(0x200 + 4);
        assert!(imported_dlls(&image).is_err(), "a truncated import table");
    }

    #[test]
    fn the_cef_folder_comes_from_cef_dll_sys_link_paths() {
        let messages = [
            r#"{"reason":"compiler-artifact","package_id":"registry+https://github.com/rust-lang/crates.io-index#cef-dll-sys@154.5.0+154.0.34"}"#,
            "not json",
            r#"{"reason":"build-script-executed","package_id":"path+file:///c/x#jstream-browser@0.0.0","linked_paths":["native=C:\\other"]}"#,
            r#"{"reason":"build-script-executed","package_id":"registry+https://github.com/rust-lang/crates.io-index#cef-dll-sys@154.5.0+154.0.34","linked_paths":["native=C:\\out\\build","native=C:\\out\\cef_windows_x86_64"]}"#,
        ]
        .join("\n");
        let is_cef = |dir: &Path| dir.to_string_lossy().ends_with("cef_windows_x86_64");
        assert_eq!(
            cef_dir(&messages, is_cef),
            Some(PathBuf::from("C:\\out\\cef_windows_x86_64"))
        );
        assert_eq!(cef_dir(&messages, |_| false), None);
        assert_eq!(cef_dir("", is_cef), None);
    }

    #[test]
    fn only_cef_runtime_files_ship() {
        for name in [
            "libcef.dll",
            "chrome_elf.dll",
            "icudtl.dat",
            "resources.pak",
            "v8_context_snapshot.bin",
            "vk_swiftshader_icd.json",
            "CREDITS.html",
        ] {
            assert!(ships_from_cef(name), "{name}");
        }
        for name in [
            "libcef.lib",
            "cef_sandbox.LIB",
            "archive.json",
            "CMakeLists.txt",
            "bootstrap.exe",
            "bootstrapc.exe",
        ] {
            assert!(!ships_from_cef(name), "{name}");
        }
    }

    #[test]
    fn the_cef_version_names_the_archive() {
        let text = cef_version_text(
            r#"{"type":"minimal","name":"cef_binary_154.0.34+g1+chromium-154.0.7795.1_windows64_minimal.tar.bz2","sha1":"ab"}"#,
        )
        .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(
            text.starts_with("cef_binary_154.0.34+g1+chromium-154.0.7795.1_windows64_minimal\n"),
            "{text}"
        );
        assert!(cef_version_text("{}").is_err());
        assert!(cef_version_text("not json").is_err());
    }
}
