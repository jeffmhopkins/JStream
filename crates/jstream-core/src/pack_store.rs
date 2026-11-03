//! Effect packs written over the protocol (ADR-0097): the checks a `pack.write` passes
//! before anything is written, the staging folder it loads from, the two renames that
//! swap it in, `pack.delete`'s folder, and the recovery at startup of a swap that
//! stopped between its renames. [`crate::effect_packs::Packs`] runs these with the
//! watcher held, so no poll sees a pack half written.
//!
//! Docs: crates/jstream-core/README.md#design

use std::collections::BTreeMap;
use std::io;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use jstream_effects::MANIFEST;
use jstream_protocol::{
    AssetRoot, EntityKind, MAX_PACK_FILES, MAX_PACK_IMAGE_BYTES, MAX_PACK_WRITE_BYTES, PackContent,
    PackFile, PackFileInfo, ProtocolError,
};

/// Where a write is put together and loaded before it is swapped in. The watcher reads
/// only the direct sub-folders of the packs folder that hold an `effect.toml`, so never
/// this one, and no pack ID names a dot-folder.
pub(crate) const STAGING: &str = ".staging";

/// Where the old version of a pack waits between a write's two renames.
pub(crate) const TRASH: &str = ".trash";

/// Where a deleted pack's folder goes before it is removed, so startup recovery never
/// takes a delete for a stopped write.
pub(crate) const DELETED: &str = ".deleted";

/// The file in the packs folder that keeps each written pack's last writer.
pub(crate) const WRITERS_FILE: &str = ".writers.json";

/// How long a failing rename is retried: a virus scanner or an editor holding a file
/// open on Windows fails it with a sharing violation for a moment.
const RENAME_FOR: Duration = Duration::from_secs(1);

/// How long to wait between two tries of a rename.
const RENAME_PAUSE: Duration = Duration::from_millis(50);

/// The most bytes of text a `pack.read` of a whole pack returns: past it, a file's
/// `text` is left out, and reading it by `path` returns it.
const MAX_READ_TEXT_BYTES: u64 = MAX_PACK_WRITE_BYTES as u64;

/// Checks `pack` is a pack ID that names a folder on every platform: 1 to
/// [`jstream_effects::MAX_ID_LEN`] lower-case letters, digits and `-`, starting with a
/// letter or digit, and not a Windows device name.
pub(crate) fn check_pack_id(pack: &str) -> Result<(), ProtocolError> {
    let max = jstream_effects::MAX_ID_LEN;
    let reason = if pack.is_empty() || pack.len() > max {
        Some(format!("a pack ID is 1 to {max} characters"))
    } else if !pack.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !pack
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        Some(
            "a pack ID is lower-case letters, digits and `-`, starting with a letter or digit"
                .into(),
        )
    } else {
        jstream_protocol::check_asset_path(AssetRoot::App, pack)
            .err()
            .map(|_| format!("`{pack}` is a Windows device name"))
    };
    match reason {
        Some(reason) => Err(ProtocolError::invalid_argument("pack", reason)),
        None => Ok(()),
    }
}

/// Whether a pack file is text (the manifest and WGSL) rather than an image.
fn is_text(path: &str) -> bool {
    path == MANIFEST || extension(path).is_some_and(|e| e == "wgsl")
}

/// `path`'s extension, lower-cased.
fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

/// Checks a pack file's path (ADR-0097 decision 2): the asset-path rules for every
/// component, no dot-component and no trailing dot or space, and `effect.toml` or a
/// `.wgsl`, `.png`, `.jpg` or `.jpeg` file.
fn check_file_path(path: &str) -> Result<(), String> {
    jstream_protocol::check_asset_path(AssetRoot::App, path)?;
    if path
        .chars()
        .any(|c| c.is_control() || "<>\"|?*".contains(c))
    {
        return Err(
            "a pack file path must not contain a control character or any of `<>\"|?*`".into(),
        );
    }
    for component in path.split('/') {
        if component.starts_with('.') {
            return Err("no component of a pack file path may start with `.`".into());
        }
        if component.ends_with(['.', ' ']) {
            return Err("no component of a pack file path may end in `.` or a space".into());
        }
        jstream_protocol::check_asset_path(AssetRoot::App, component)
            .map_err(|_| format!("`{component}` is a Windows device name"))?;
    }
    if path == MANIFEST {
        return Ok(());
    }
    match extension(path).as_deref() {
        Some("wgsl" | "png" | "jpg" | "jpeg") => Ok(()),
        _ => Err(format!(
            "a pack file is `{MANIFEST}` or a `.wgsl`, `.png`, `.jpg` or `.jpeg` file"
        )),
    }
}

/// The bytes `s` takes as a JSON string, quotes and escapes included.
fn json_len(s: &str) -> usize {
    serde_json::to_string(s).map_or(usize::MAX, |json| json.len())
}

/// Checks a `pack.write`'s files (ADR-0097 decisions 2 and 3) and decodes them, each
/// with its path. Their size as sent is checked first, before any is decoded.
pub(crate) fn decode(files: &[PackFile]) -> Result<Vec<(String, Vec<u8>)>, ProtocolError> {
    let sent = files.iter().fold(0usize, |sum, file| {
        let content = match &file.content {
            PackContent::Text(text) | PackContent::Base64(text) => text,
        };
        sum.saturating_add(json_len(&file.path))
            .saturating_add(json_len(content))
    });
    if sent > MAX_PACK_WRITE_BYTES {
        return Err(ProtocolError::TooLarge {
            size_bytes: sent as u64,
            limit_bytes: MAX_PACK_WRITE_BYTES as u64,
        });
    }
    if files.len() > MAX_PACK_FILES {
        return Err(ProtocolError::invalid_argument(
            "files",
            format!(
                "{} files is more than the {MAX_PACK_FILES} a pack write may hold",
                files.len()
            ),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut decoded = Vec::with_capacity(files.len());
    for file in files {
        let path = &file.path;
        let bad = |reason: String| {
            ProtocolError::invalid_argument("files", format!("`{path}`: {reason}"))
        };
        check_file_path(path).map_err(bad)?;
        // Without case, as Windows and macOS compare names: two that differ only in
        // case would be one file there.
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(bad("the write names this file twice".into()));
        }
        let bytes = match (&file.content, is_text(path)) {
            (PackContent::Text(text), true) => text.as_bytes().to_vec(),
            (PackContent::Base64(_), true) => {
                return Err(bad("a manifest or WGSL file is sent as `text`".into()));
            }
            (PackContent::Text(_), false) => {
                return Err(bad("an image is sent as `base64`".into()));
            }
            (PackContent::Base64(text), false) => {
                let bytes = STANDARD
                    .decode(text)
                    .map_err(|error| bad(format!("not standard base64: {error}")))?;
                if bytes.len() > MAX_PACK_IMAGE_BYTES {
                    return Err(bad(format!(
                        "{} bytes is larger than the {MAX_PACK_IMAGE_BYTES}-byte image limit",
                        bytes.len()
                    )));
                }
                bytes
            }
        };
        decoded.push((path.clone(), bytes));
    }
    if !decoded.iter().any(|(path, _)| path == MANIFEST) {
        return Err(ProtocolError::invalid_argument(
            "files",
            format!("a pack write holds the pack's `{MANIFEST}`"),
        ));
    }
    Ok(decoded)
}

/// Whether the file system takes the folder names `a` and `b` for one: without case on
/// Windows and macOS.
fn same_name(a: &str, b: &str) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// The sub-folders of `root` that may hold a pack, with their names: every one but a
/// dot-folder, sorted.
pub(crate) fn folders(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let path = e.path();
            (!name.starts_with('.') && path.is_dir()).then_some((name, path))
        })
        .collect();
    out.sort();
    out
}

/// The folders whose `effect.toml` names pack ID `pack`, loaded or not.
pub(crate) fn holders(root: &Path, pack: &str) -> Vec<PathBuf> {
    folders(root)
        .into_iter()
        .filter(|(_, dir)| jstream_effects::manifest_pack_id(dir).is_ok_and(|id| id == pack))
        .map(|(_, dir)| dir)
        .collect()
}

/// Removes `path`, a folder (never following a link) or a file, if it is there.
fn remove(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    }
}

/// Whether anything, a link included, is at `path`.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Renames `from` to `to`, retried for up to [`RENAME_FOR`].
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let start = Instant::now();
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if start.elapsed() >= RENAME_FOR => return Err(error),
            Err(_) => std::thread::sleep(RENAME_PAUSE),
        }
    }
}

/// An `internal` error for `what` failing on `path`.
fn internal(what: &str, path: &Path, error: &io::Error) -> ProtocolError {
    ProtocolError::Internal {
        reason: format!("{what} {}: {error}", path.display()),
    }
}

/// Recovers, before the first load, a write or delete that stopped part-way
/// (ADR-0097 decision 4): a `.trash/<pack>` with no `<pack>` is renamed back, one beside
/// its `<pack>` is removed, and `.staging` and `.deleted` are emptied. Each failure is
/// logged; the next write of that pack tries again.
pub(crate) fn recover(root: &Path) {
    if let Ok(entries) = std::fs::read_dir(root.join(TRASH)) {
        for entry in entries.filter_map(Result::ok) {
            let trashed = entry.path();
            let home = root.join(entry.file_name());
            let done = if exists(&home) {
                remove(&trashed)
            } else {
                tracing::warn!(pack = %home.display(), "a pack write stopped between its renames; putting the old version back");
                rename(&trashed, &home)
            };
            if let Err(error) = done {
                tracing::warn!(path = %trashed.display(), %error, "stopped pack write not recovered");
            }
        }
    }
    for leftover in [STAGING, DELETED] {
        if let Err(error) = remove(&root.join(leftover)) {
            tracing::warn!(path = %root.join(leftover).display(), %error, "leftover pack files not removed");
        }
    }
}

/// Writes `files` as pack `pack` in `root` (ADR-0097 decisions 2 and 4): checks which
/// folder it may land in, stages and loads it with `load`, and swaps it in with two
/// renames. Returns the pack's folder. The caller holds the watcher, so no poll runs
/// meanwhile. On an error the old version, if any, stays as it was, except when the
/// error says the pack is off air until the next start.
pub(crate) fn write(
    root: &Path,
    pack: &str,
    files: &[(String, Vec<u8>)],
    load: impl FnOnce(&Path) -> Result<jstream_effects::Pack, jstream_effects::PackError>,
) -> Result<PathBuf, ProtocolError> {
    let home = root.join(pack);
    let staging = root.join(STAGING).join(pack);
    let trash = root.join(TRASH).join(pack);
    // An earlier write's undo failed: its old version is the only copy, so put it back
    // before anything else is removed.
    if exists(&trash) && !exists(&home) {
        rename(&trash, &home).map_err(|e| internal("couldn't put back", &trash, &e))?;
        tracing::info!(pack, "recovered a pack an earlier write left off air");
    }
    let refuse = |reason: String| Err(ProtocolError::invalid_argument("pack", reason));
    for (name, dir) in folders(root) {
        let id = jstream_effects::manifest_pack_id(&dir);
        if same_name(&name, pack) {
            match id {
                Ok(id) if id == pack => {}
                Ok(id) => {
                    return refuse(format!(
                        "the folder `{name}` holds pack `{id}`; a write only replaces the pack it names"
                    ));
                }
                Err(error) => {
                    return refuse(format!(
                        "the folder `{name}` is there but holds no pack this write may replace: {error}"
                    ));
                }
            }
        } else if id.is_ok_and(|id| id == pack) {
            return refuse(format!(
                "the folder `{name}` already holds pack `{pack}`; delete it first, or write that one"
            ));
        }
    }
    if exists(&home) && !home.is_dir() {
        return refuse(format!("`{pack}` in the packs folder is not a folder"));
    }
    remove(&staging).map_err(|e| internal("couldn't remove the leftover", &staging, &e))?;
    remove(&trash).map_err(|e| internal("couldn't remove the leftover", &trash, &e))?;
    let staged = stage(&staging, files).and_then(|()| {
        let loaded = load(&staging).map_err(|error| {
            ProtocolError::invalid_argument("files", without_staging(&staging, &error.to_string()))
        })?;
        if loaded.id() == pack {
            Ok(())
        } else {
            Err(ProtocolError::invalid_argument(
                "files",
                format!("`{MANIFEST}` names pack `{}`, not `{pack}`", loaded.id()),
            ))
        }
    });
    if let Err(error) = staged {
        if let Err(e) = remove(&staging) {
            tracing::warn!(path = %staging.display(), error = %e, "staged pack not removed");
        }
        return Err(error);
    }
    if exists(&home) {
        if let Err(error) =
            std::fs::create_dir_all(root.join(TRASH)).and_then(|()| rename(&home, &trash))
        {
            let _ = remove(&staging);
            return Err(internal("couldn't move aside", &home, &error));
        }
        if let Err(error) = rename(&staging, &home) {
            let _ = remove(&staging);
            return match rename(&trash, &home) {
                Ok(()) => Err(internal(
                    "couldn't move the new version into",
                    &home,
                    &error,
                )),
                Err(undo) => {
                    tracing::error!(pack, %error, %undo, "pack write failed and wasn't undone; the pack is off air until the next start");
                    Err(ProtocolError::Internal {
                        reason: format!(
                            "couldn't move the new version into {} ({error}), nor the old one back ({undo}): pack `{pack}` is off air until the next start, which puts the old version back",
                            home.display()
                        ),
                    })
                }
            };
        }
        if let Err(error) = remove(&trash) {
            tracing::warn!(path = %trash.display(), %error, "old pack version not removed; the next write of it removes it");
        }
    } else if let Err(error) = rename(&staging, &home) {
        let _ = remove(&staging);
        return Err(internal("couldn't move the new pack into", &home, &error));
    }
    Ok(home)
}

/// Writes `files` into `staging`, a fresh folder.
fn stage(staging: &Path, files: &[(String, Vec<u8>)]) -> Result<(), ProtocolError> {
    for (path, bytes) in files {
        let to = staging.join(path);
        let written = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&to, bytes));
        written.map_err(|e| internal("couldn't write", &to, &e))?;
    }
    if files.is_empty() {
        std::fs::create_dir_all(staging).map_err(|e| internal("couldn't make", staging, &e))?;
    }
    Ok(())
}

/// A load error's text with the staging folder taken out of its paths, so it names
/// each file by its path in the pack.
fn without_staging(staging: &Path, message: &str) -> String {
    let prefix = format!("{}{MAIN_SEPARATOR}", staging.display());
    message
        .replace(&prefix, "")
        .replace(&staging.display().to_string(), "the pack")
}

/// Deletes pack `pack` from `root` (ADR-0097 decision 4): each folder whose
/// `effect.toml` names it goes to `.deleted/<pack>`, then is removed. `not_found` when
/// none does.
pub(crate) fn delete(root: &Path, pack: &str) -> Result<(), ProtocolError> {
    let held = holders(root, pack);
    if held.is_empty() {
        return Err(ProtocolError::NotFound {
            entity: EntityKind::Pack,
            id: pack.to_owned(),
        });
    }
    let deleted = root.join(DELETED).join(pack);
    for dir in held {
        remove(&deleted).map_err(|e| internal("couldn't remove the leftover", &deleted, &e))?;
        std::fs::create_dir_all(root.join(DELETED))
            .and_then(|()| rename(&dir, &deleted))
            .map_err(|e| internal("couldn't move aside", &dir, &e))?;
        if let Err(error) = remove(&deleted) {
            tracing::warn!(path = %deleted.display(), %error, "deleted pack's files not removed; the next start removes them");
        }
    }
    Ok(())
}

/// Each file of the pack in `dir`, for `pack.read` (ADR-0097 decision 1): the text of
/// its manifest and WGSL files, and of an image its size and digest. With `path`, only
/// that file, an image as base64; one over [`MAX_PACK_IMAGE_BYTES`] is `too_large`.
pub(crate) fn read(dir: &Path, path: Option<&str>) -> Result<Vec<PackFileInfo>, ProtocolError> {
    let mut files = Vec::new();
    list_files(dir, "", &mut files);
    if let Some(wanted) = path {
        let Some(found) = files.iter().find(|(rel, _)| rel == wanted) else {
            return Err(ProtocolError::invalid_argument(
                "path",
                format!("the pack has no file `{wanted}`"),
            ));
        };
        let bytes = std::fs::read(&found.1).map_err(|e| internal("couldn't read", &found.1, &e))?;
        let mut info = file_info(wanted, &bytes);
        if is_text(wanted) {
            info.text = Some(String::from_utf8_lossy(&bytes).into_owned());
        } else if bytes.len() > MAX_PACK_IMAGE_BYTES {
            return Err(ProtocolError::TooLarge {
                size_bytes: bytes.len() as u64,
                limit_bytes: MAX_PACK_IMAGE_BYTES as u64,
            });
        } else {
            info.base64 = Some(STANDARD.encode(&bytes));
        }
        return Ok(vec![info]);
    }
    let mut text_left = MAX_READ_TEXT_BYTES;
    let mut out = Vec::with_capacity(files.len());
    for (rel, file) in &files {
        let bytes = std::fs::read(file).map_err(|e| internal("couldn't read", file, &e))?;
        let mut info = file_info(rel, &bytes);
        if is_text(rel) && info.size <= text_left {
            text_left -= info.size;
            info.text = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        out.push(info);
    }
    Ok(out)
}

/// A file's size and digest, with no contents.
fn file_info(path: &str, bytes: &[u8]) -> PackFileInfo {
    PackFileInfo {
        path: path.to_owned(),
        size: bytes.len() as u64,
        digest: blake3::hash(bytes).to_hex().to_string(),
        text: None,
        base64: None,
    }
}

/// The regular files under `dir`, with their `/`-separated paths below `prefix`,
/// sorted. Links aren't followed, and dot-files are left out.
fn list_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        match entry.file_type() {
            Ok(t) if t.is_dir() => list_files(&entry.path(), &rel, out),
            Ok(t) if t.is_file() => out.push((rel, entry.path())),
            _ => {}
        }
    }
}

/// The last writer of each written pack, from [`WRITERS_FILE`]: none when it is missing
/// or can't be read.
pub(crate) fn read_writers(root: &Path) -> BTreeMap<String, String> {
    std::fs::read(root.join(WRITERS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Saves `writers` to [`WRITERS_FILE`], logging a failure: the names then last this run.
pub(crate) fn save_writers(root: &Path, writers: &BTreeMap<String, String>) {
    let path = root.join(WRITERS_FILE);
    let written = serde_json::to_vec_pretty(writers)
        .map_err(io::Error::other)
        .and_then(|bytes| crate::collection::write_atomically(&path, &bytes));
    if let Err(error) = written {
        tracing::warn!(path = %path.display(), %error, "pack writers not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::{check_file_path, check_pack_id, decode};
    use jstream_protocol::{PackContent, PackFile, ProtocolError};

    fn text(path: &str, text: &str) -> PackFile {
        PackFile {
            path: path.into(),
            content: PackContent::Text(text.into()),
        }
    }

    #[test]
    fn pack_ids_name_a_folder_on_every_platform() {
        for good in ["tint", "a", "0-glow", "abc-123"] {
            assert!(check_pack_id(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-x",
            "Tint",
            "a_b",
            ".staging",
            "a/b",
            "con",
            "com1",
            &"a".repeat(33),
        ] {
            assert!(check_pack_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn file_paths_follow_the_asset_path_rules_per_component() {
        for good in ["effect.toml", "mul.wgsl", "img/noise.PNG", "a/b/c.jpeg"] {
            assert_eq!(check_file_path(good), Ok(()), "{good}");
        }
        for bad in [
            "../x.wgsl",
            "/abs.wgsl",
            "C:/x.wgsl",
            "a\\b.wgsl",
            "a:b.wgsl",
            ".hidden/x.wgsl",
            "a/.x.wgsl",
            "nul.png",
            "con/x.wgsl",
            "x.wgsl.",
            "dir /x.wgsl",
            "a//b.wgsl",
            "x.exe",
            "sub/effect.toml",
            "quarantine.json",
            "x?.wgsl",
        ] {
            assert!(check_file_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_write_is_checked_before_it_is_decoded() {
        let manifest = text("effect.toml", "[pack]\n");
        assert!(decode(std::slice::from_ref(&manifest)).is_ok());
        let missing = decode(&[text("a.wgsl", "")]).unwrap_err();
        assert!(missing.to_string().contains("effect.toml"), "{missing}");
        let twice = decode(&[manifest.clone(), text("A.wgsl", ""), text("a.wgsl", "")]);
        assert!(twice.unwrap_err().to_string().contains("twice"));
        let image_as_text = decode(&[manifest.clone(), text("a.png", "x")]);
        assert!(image_as_text.unwrap_err().to_string().contains("base64"));
        let not_base64 = PackFile {
            path: "a.png".into(),
            content: PackContent::Base64("!!".into()),
        };
        assert!(decode(&[manifest.clone(), not_base64]).is_err());
        let many: Vec<PackFile> = (0..65).map(|i| text(&format!("{i}.wgsl"), "")).collect();
        assert!(decode(&many).unwrap_err().to_string().contains("65 files"));
        let big = text(
            "big.wgsl",
            &"x".repeat(jstream_protocol::MAX_PACK_WRITE_BYTES),
        );
        assert!(matches!(
            decode(&[manifest, big]),
            Err(ProtocolError::TooLarge { .. })
        ));
    }

    mod packs {
        use crate::effect_packs::Packs;
        use crate::effect_packs::test_pack::{self, BLUE, KIND, MANIFEST, MUL, Temp};
        use jstream_protocol::{PackContent, PackFile, ProtocolError};

        fn text(path: &str, text: &str) -> PackFile {
            PackFile {
                path: path.into(),
                content: PackContent::Text(text.into()),
            }
        }

        /// The `tint` pack's files, drawing with `shader`.
        fn tint(shader: &str) -> Vec<PackFile> {
            vec![text("effect.toml", MANIFEST), text("mul.wgsl", shader)]
        }

        fn open(temp: &Temp) -> Packs {
            let mut packs = Packs::new(temp.0.clone());
            let _ = packs.poll();
            packs
        }

        #[test]
        fn a_written_pack_loads_at_once_and_lists_its_writer() {
            let temp = Temp::new("write-new");
            let mut packs = open(&temp);
            let (info, update) = packs.write("tint", &tint(MUL), "agent").expect("written");
            assert_eq!(info.kinds, [KIND]);
            assert_eq!(info.folder, "tint");
            assert!(info.loaded);
            assert_eq!(info.written_by.as_deref(), Some("agent"));
            let update = update.expect("its kinds are new");
            assert!(update.effects.contains_key(KIND));
            assert_eq!(packs.list(), [info]);
            let files = packs.read("tint", None).expect("read");
            let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(paths, ["effect.toml", "mul.wgsl"]);
            assert_eq!(files[1].text.as_deref(), Some(MUL));
            assert!(!temp.0.join(super::super::STAGING).join("tint").exists());
            // The writer outlasts the engine.
            let again = open(&temp);
            assert_eq!(again.list()[0].written_by.as_deref(), Some("agent"));
        }

        #[test]
        fn a_write_that_fails_to_load_keeps_the_old_version() {
            let temp = Temp::new("write-fails");
            let mut packs = open(&temp);
            packs.write("tint", &tint(MUL), "agent").expect("written");
            let error = packs
                .write("tint", &tint("@fragment fn fs("), "agent")
                .expect_err("the shader doesn't parse");
            let message = error.to_string();
            assert_eq!(error.code(), "invalid_argument");
            assert!(message.contains("mul.wgsl"), "{message}");
            assert!(!message.contains(".staging"), "{message}");
            let files = packs.read("tint", Some("mul.wgsl")).expect("read");
            assert_eq!(files[0].text.as_deref(), Some(MUL));
            assert!(packs.poll().is_none(), "nothing changed on disk");
        }

        #[test]
        fn a_rewrite_swaps_the_pack_and_leaves_nothing_behind() {
            let temp = Temp::new("rewrite");
            let mut packs = open(&temp);
            packs.write("tint", &tint(MUL), "one").expect("written");
            let (info, update) = packs.write("tint", &tint(BLUE), "two").expect("rewritten");
            assert_eq!(info.written_by.as_deref(), Some("two"));
            let update = update.expect("the kind changed");
            assert_eq!(update.described.len(), 1);
            let shader = std::fs::read_to_string(temp.pack().join("mul.wgsl")).expect("read");
            assert_eq!(shader, BLUE);
            for leftover in [super::super::STAGING, super::super::TRASH] {
                assert!(!temp.0.join(leftover).join("tint").exists(), "{leftover}");
            }
        }

        #[test]
        fn a_write_only_replaces_the_pack_it_names() {
            let temp = Temp::new("write-refused");
            let mut packs = open(&temp);
            let refused = |packs: &mut Packs, pack: &str, files: &[PackFile]| {
                let error = packs.write(pack, files, "agent").expect_err("refused");
                assert_eq!(error.code(), "invalid_argument", "{error}");
                error.to_string()
            };
            // Its manifest names another pack.
            let message = refused(&mut packs, "glow", &tint(MUL));
            assert!(message.contains("not `glow`"), "{message}");
            assert!(!temp.0.join("glow").exists());
            // Not a pack ID.
            refused(&mut packs, "../tint", &tint(MUL));
            refused(&mut packs, "nul", &tint(MUL));
            // Another folder holds the ID.
            test_pack::write(&temp.0.join("mine"), MUL);
            let message = refused(&mut packs, "tint", &tint(BLUE));
            assert!(message.contains("`mine` already holds"), "{message}");
            std::fs::remove_dir_all(temp.0.join("mine")).expect("remove");
            // The folder is there, with no manifest.
            std::fs::create_dir_all(temp.pack()).expect("mkdir");
            std::fs::write(temp.pack().join("notes.txt"), "mine").expect("write");
            let message = refused(&mut packs, "tint", &tint(MUL));
            assert!(message.contains("holds no pack"), "{message}");
            assert!(temp.pack().join("notes.txt").exists());
        }

        #[test]
        fn a_pack_that_fails_to_load_can_be_fixed_by_a_write() {
            let temp = Temp::new("write-fix");
            test_pack::write(&temp.pack(), "@fragment fn fs(");
            let mut packs = open(&temp);
            assert!(!packs.list()[0].loaded);
            let (info, _) = packs.write("tint", &tint(MUL), "agent").expect("fixed");
            assert!(info.loaded);
        }

        #[test]
        fn a_delete_removes_the_folder_that_names_the_pack() {
            let temp = Temp::new("delete");
            test_pack::write(&temp.0.join("mine"), MUL);
            let mut packs = open(&temp);
            let update = packs
                .delete("tint")
                .expect("deleted")
                .expect("its kind went");
            assert_eq!(update.removed, [KIND]);
            assert!(!temp.0.join("mine").exists());
            assert!(packs.list().is_empty());
            let error = packs.delete("tint").expect_err("gone");
            assert!(matches!(error, ProtocolError::NotFound { .. }), "{error}");
            assert!(packs.read("tint", None).is_err());
        }

        #[test]
        fn startup_recovers_a_swap_that_stopped_between_its_renames() {
            let temp = Temp::new("recover");
            // The old `tint`, moved aside, with no new one in its place.
            test_pack::write(&temp.0.join(super::super::TRASH).join("tint"), MUL);
            // A `glow` whose swap finished but whose old version wasn't removed.
            std::fs::create_dir_all(temp.0.join("glow")).expect("mkdir");
            std::fs::create_dir_all(temp.0.join(super::super::TRASH).join("glow")).expect("mkdir");
            // Leftovers of a write and a delete.
            test_pack::write(&temp.0.join(super::super::STAGING).join("x"), MUL);
            test_pack::write(&temp.0.join(super::super::DELETED).join("y"), MUL);
            let packs = open(&temp);
            assert_eq!(packs.list()[0].kinds, [KIND]);
            assert!(temp.0.join("glow").exists());
            assert!(!temp.0.join(super::super::TRASH).join("glow").exists());
            assert!(!temp.0.join(super::super::STAGING).exists());
            assert!(!temp.0.join(super::super::DELETED).exists());
        }

        #[test]
        fn a_write_puts_back_an_old_version_an_undo_left_off_air() {
            let temp = Temp::new("write-recover");
            let mut packs = open(&temp);
            test_pack::write(&temp.0.join(super::super::TRASH).join("tint"), MUL);
            let (info, _) = packs.write("tint", &tint(BLUE), "agent").expect("written");
            assert!(info.loaded);
            assert!(!temp.0.join(super::super::TRASH).join("tint").exists());
            let shader = std::fs::read_to_string(temp.pack().join("mul.wgsl")).expect("read");
            assert_eq!(shader, BLUE);
        }

        #[test]
        fn an_image_is_read_as_base64_only_by_its_path() {
            let temp = Temp::new("read-image");
            let mut packs = open(&temp);
            let mut files = tint(MUL);
            files.push(PackFile {
                path: "img/a.png".into(),
                content: PackContent::Base64("AQID".into()),
            });
            packs.write("tint", &files, "agent").expect("written");
            let all = packs.read("tint", None).expect("read");
            let image = all.iter().find(|f| f.path == "img/a.png").expect("listed");
            assert_eq!(
                (image.size, image.text.as_ref(), image.base64.as_ref()),
                (3, None, None)
            );
            assert_eq!(image.digest, blake3::hash(&[1, 2, 3]).to_hex().to_string());
            let one = packs.read("tint", Some("img/a.png")).expect("read");
            assert_eq!(one[0].base64.as_deref(), Some("AQID"));
            let missing = packs.read("tint", Some("b.png")).expect_err("no such file");
            assert_eq!(missing.code(), "invalid_argument");
            // One a person put there, over the image limit.
            let big = vec![0u8; jstream_protocol::MAX_PACK_IMAGE_BYTES + 1];
            std::fs::write(temp.pack().join("big.png"), big).expect("write");
            let error = packs.read("tint", Some("big.png")).expect_err("too large");
            assert!(matches!(error, ProtocolError::TooLarge { .. }), "{error}");
        }
    }
}
