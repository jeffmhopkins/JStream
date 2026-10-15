//! The `Source` and `SourceKind` seams, and what a source is created with.

use crate::SourceError;
use jstream_media::{MediaTime, Rounding, Timebase, VideoFrame};
use jstream_protocol::{AssetRoot, KindDescriptor, Reference, Settings};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A running source: something that produces video frames on request.
///
/// Sources are created by their [`SourceKind`] from a complete, already validated
/// settings map. Creating one may block (an image is read and decoded then), so it never
/// happens on a real-time thread. Asking for a frame doesn't block or allocate for the
/// built-in kinds: frames are made when the source is created or updated and handed out
/// as shared [`Arc`]s, so the render thread can hold one past the next update.
pub trait Source: Send {
    /// The frame to show at `at`, the time since the source started on the media clock
    /// (ADR-0014, docs/adr/0014-media-clock.md). A time before the start shows the
    /// first frame.
    ///
    /// # Errors
    /// Whatever stops the source from producing a frame. The built-in kinds never fail
    /// here; their errors surface when they are created.
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError>;

    /// When the frame shown at `at` is next replaced, on the same clock as
    /// [`frame`](Self::frame). `None` means it never changes again (a still image, a
    /// colour, or an animation that has finished its loops).
    fn next_change(&self, at: MediaTime) -> Option<MediaTime>;

    /// Applies a new settings map. Only parameters marked `live` in the kind's
    /// descriptor may differ from the ones the source runs with; check
    /// [`needs_restart`](crate::needs_restart) first and create a new source otherwise.
    ///
    /// # Errors
    /// [`SourceError::NotLive`] when a non-live parameter changed, and
    /// [`SourceError::Setting`] when a value doesn't have its descriptor's type.
    fn update(&mut self, settings: &Settings) -> Result<(), SourceError>;
}

/// A source kind: its descriptor (`kind.describe`) and how to create its sources.
///
/// The descriptor is what the engine validates settings against (ADR-0025,
/// docs/adr/0025-typed-settings-kind-descriptors.md), so `create` may assume every
/// parameter is present and well typed, but still reports a bad value as an error
/// rather than panicking.
pub trait SourceKind: Send + Sync {
    /// The kind's descriptor. Its `kind_id` is the registry key.
    fn descriptor(&self) -> &KindDescriptor;

    /// Creates a source from a complete settings map.
    ///
    /// # Errors
    /// [`SourceError::NotBuilt`] for a kind that is only described so far, and the
    /// kind's own errors (an unreadable or undecodable asset, a bad setting).
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError>;
}

/// What sources are created with: where asset references point, how much a decoded
/// asset may take, and the featured chat message.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// The folders behind asset roots.
    pub assets: AssetRoots,
    /// Size limits for decoded assets.
    pub limits: Limits,
    /// The featured chat message `chat_message` sources draw. Clones share it.
    pub chat: crate::ChatFeature,
}

/// The folders behind the `collection` and `app` asset roots. `absolute` paths need
/// none.
#[derive(Clone, Debug, Default)]
pub struct AssetRoots {
    /// The collection file's folder; `None` while the collection is unsaved.
    pub collection: Option<PathBuf>,
    /// The app's asset folder.
    pub app: Option<PathBuf>,
}

impl AssetRoots {
    /// The host file an asset reference names. Under `collection` and `app`, the path
    /// is resolved through symlinks and must stay inside its root (ADR-0025).
    ///
    /// The answer can be out of date as soon as it returns, so it says only where the
    /// file was; [`read`](Self::read) checks the file it opens instead.
    ///
    /// # Errors
    /// [`SourceError::NoAssetRoot`] when the root has no folder,
    /// [`SourceError::AssetRead`] when the file or its root can't be resolved, and
    /// [`SourceError::AssetEscapesRoot`] when it leaves its root.
    pub fn resolve(&self, root: AssetRoot, path: &str) -> Result<PathBuf, SourceError> {
        let read_err = read_err(path);
        let Some(base) = self.base(root, path)? else {
            return Path::new(path).canonicalize().map_err(read_err);
        };
        let file = base.join(path).canonicalize().map_err(read_err)?;
        if file.starts_with(&base) {
            Ok(file)
        } else {
            Err(escapes(root, path))
        }
    }

    /// Reads the file an asset reference names, up to `max_bytes`.
    ///
    /// Under `collection` and `app`, the file read is checked to be inside its root as
    /// it is opened, so swapping a folder inside the root for a symlink while the file
    /// is being resolved can't make it read a file outside.
    ///
    /// # Errors
    /// Those of [`resolve`](Self::resolve) (with [`SourceError::AssetEscapesRoot`] also
    /// when the file changes while it is opened), [`SourceError::AssetRead`] when
    /// reading fails, and [`SourceError::TooLarge`] when the file is longer than
    /// `max_bytes`.
    pub fn read(
        &self,
        root: AssetRoot,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SourceError> {
        let read_err = read_err(path);
        let file = match self.base(root, path)? {
            Some(base) => open_inside(&base, path).map_err(|e| match e {
                Opened::Escapes => escapes(root, path),
                Opened::Io(e) => read_err(e),
            })?,
            None => File::open(path).map_err(&read_err)?,
        };
        let mut bytes = Vec::new();
        // One byte past the limit tells a file of exactly `max_bytes` from a longer one,
        // without trusting a length the file could change under us.
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(read_err)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(SourceError::TooLarge {
                path: path.to_owned(),
                what: format!("the file is over {max_bytes} bytes"),
            });
        }
        Ok(bytes)
    }

    /// The canonical folder behind `root`; `None` for `absolute`, which has none. A
    /// root that can't be resolved is reported against `path`, the asset being read.
    fn base(&self, root: AssetRoot, path: &str) -> Result<Option<PathBuf>, SourceError> {
        let base = match root {
            AssetRoot::Collection => self.collection.as_deref(),
            AssetRoot::App => self.app.as_deref(),
            AssetRoot::Absolute => return Ok(None),
            // `AssetRoot` is non-exhaustive: a root this crate doesn't know has no folder.
            _ => None,
        };
        let base = base.ok_or(SourceError::NoAssetRoot { root })?;
        // The root is the host's own configuration, so it is trusted as it resolves now.
        let base = base.canonicalize().map_err(read_err(path))?;
        Ok(Some(base))
    }
}

fn read_err(path: &str) -> impl Fn(std::io::Error) -> SourceError + '_ {
    move |e| SourceError::AssetRead {
        path: path.to_owned(),
        kind: e.kind(),
    }
}

fn escapes(root: AssetRoot, path: &str) -> SourceError {
    SourceError::AssetEscapesRoot {
        root,
        path: path.to_owned(),
    }
}

/// Why [`open_inside`] gave no file.
#[derive(Debug)]
enum Opened {
    /// The file it would open is outside the root, or changed while it was opened.
    Escapes,
    /// Resolving or opening failed.
    Io(std::io::Error),
}

impl From<std::io::Error> for Opened {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Opens `rel` under the canonical folder `base`, and makes sure the file opened is
/// inside `base` however the path's folders change meanwhile.
///
/// On Unix the file is opened first and then compared, by device and inode, with what
/// its path resolves to afterwards: a file swapped in from outside the root is a
/// different file.
#[cfg(unix)]
fn open_inside(base: &Path, rel: &str) -> Result<File, Opened> {
    let joined = base.join(rel);
    let file = File::open(&joined)?;
    check_opened(&file, base, &joined)?;
    Ok(file)
}

/// Whether `file`, opened through `joined`, is the file `joined` now resolves to inside
/// `base`.
#[cfg(unix)]
fn check_opened(file: &File, base: &Path, joined: &Path) -> Result<(), Opened> {
    use std::os::unix::fs::MetadataExt;
    let canonical = joined.canonicalize()?;
    if !canonical.starts_with(base) {
        return Err(Opened::Escapes);
    }
    let (opened, named) = (file.metadata()?, std::fs::metadata(&canonical)?);
    if (opened.dev(), opened.ino()) == (named.dev(), named.ino()) {
        Ok(())
    } else {
        Err(Opened::Escapes)
    }
}

/// Opens `rel` under the canonical folder `base`, and makes sure the file opened is
/// inside `base` however the path's folders change meanwhile.
///
/// Stable Rust has no file IDs on Windows, so the path is held still instead: its
/// resolved form is walked from `base` one component at a time (see [`walk_below`]).
/// A path that resolves outside `base` is refused before the walk.
#[cfg(windows)]
fn open_inside(base: &Path, rel: &str) -> Result<File, Opened> {
    let canonical = base.join(rel).canonicalize()?;
    let below = canonical.strip_prefix(base).map_err(|_| Opened::Escapes)?;
    walk_below(base, below)
}

/// Opens `below`, a path relative to the canonical folder `base` that was resolved
/// already, one component at a time, each opened as itself (not followed if it is a
/// reparse point) and without `FILE_SHARE_DELETE`, so it can't be renamed, deleted or
/// replaced while the walk holds it. A component that is a link by then (a
/// name-surrogate reparse point: symlink, junction, WSL link) was swapped after
/// resolving, and is refused. Other reparse points, such as OneDrive placeholders and
/// dedup files, are files `canonicalize` left in place, and are read.
#[cfg(windows)]
fn walk_below(base: &Path, below: &Path) -> Result<File, Opened> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    // Win32 values (winnt.h, winbase.h).
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let parts: Vec<_> = below.components().collect();
    let mut at = base.to_path_buf();
    // Handles on the folders walked so far, held until the file is open.
    let mut held = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        at.push(part);
        let last = i + 1 == parts.len();
        // A folder opens only with backup semantics; the file itself without, so a
        // folder at the end fails to open as it does elsewhere.
        let flags =
            FILE_FLAG_OPEN_REPARSE_POINT | if last { 0 } else { FILE_FLAG_BACKUP_SEMANTICS };
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(flags)
            .open(&at)?;
        // std reads the reparse tag, and counts only name-surrogate tags as links.
        if handle.metadata()?.file_type().is_symlink() {
            return Err(Opened::Escapes);
        }
        if last {
            return Ok(handle);
        }
        held.push(handle);
    }
    // `rel` resolved to `base` itself, a folder.
    Err(Opened::Io(std::io::ErrorKind::IsADirectory.into()))
}

/// How large a decoded asset may be. A small file can decode to a huge image (a
/// "decompression bomb"), so the limits apply to the decoded size, not only the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The largest file read, in bytes.
    pub max_file_bytes: u64,
    /// The largest width or height, in pixels.
    pub max_side: u32,
    /// The most bytes decoding one image may hold: the frames kept plus the decoder's
    /// working buffers.
    pub max_decoded_bytes: u64,
}

impl Default for Limits {
    /// 256 MiB files, 16384 px sides (the largest a source's `width` and `height` may
    /// be) and 1 GiB of decoded memory.
    fn default() -> Self {
        Self {
            max_file_bytes: 256 << 20,
            max_side: 16384,
            max_decoded_bytes: 1 << 30,
        }
    }
}

/// Whether going from settings `old` to `new` needs the source to be created again:
/// some parameter that isn't `live` in `descriptor` changed.
pub fn needs_restart(descriptor: &KindDescriptor, old: &Settings, new: &Settings) -> bool {
    changed_non_live(descriptor, old, new).is_some()
}

/// The first parameter, in descriptor order, that isn't `live` and differs between
/// `old` and `new`.
pub(crate) fn changed_non_live<'d>(
    descriptor: &'d KindDescriptor,
    old: &Settings,
    new: &Settings,
) -> Option<&'d str> {
    descriptor
        .params
        .iter()
        .find(|p| !p.live && old.get(&p.id) != new.get(&p.id))
        .map(|p| p.id.as_str())
}

/// `at` in whole milliseconds, with times before the start as zero.
pub(crate) fn millis(at: MediaTime) -> u64 {
    at.rescale(Timebase::MILLIS, Rounding::Down)
        .map_or(0, |t| u64::try_from(t.ticks()).unwrap_or(0))
}

/// A millisecond time on the media clock.
pub(crate) fn at_millis(ms: u64) -> MediaTime {
    MediaTime::new(i64::try_from(ms).unwrap_or(i64::MAX), Timebase::MILLIS)
}

/// The value of parameter `id`, or a [`SourceError::Setting`] naming it.
pub(crate) fn setting<'s>(
    settings: &'s Settings,
    id: &str,
) -> Result<&'s serde_json::Value, SourceError> {
    settings.get(id).ok_or_else(|| SourceError::Setting {
        param: id.to_owned(),
        reason: "missing".into(),
    })
}

/// The asset reference parameter `id` holds; `None` when it is `null`.
pub(crate) fn asset(
    settings: &Settings,
    id: &str,
) -> Result<Option<(AssetRoot, String)>, SourceError> {
    let value = setting(settings, id)?;
    if value.is_null() {
        return Ok(None);
    }
    match Reference::from_value(value) {
        Some(Reference::Asset { root, path }) => Ok(Some((root, path))),
        _ => Err(SourceError::Setting {
            param: id.to_owned(),
            reason: "must be an asset reference or null".into(),
        }),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Opened, check_opened};
    use std::fs::{self, File};

    // The race itself can't be timed in a test, so this checks its two outcomes: the
    // file opened before a swap is compared with what the path names after it.
    #[test]
    fn a_file_swapped_in_from_outside_the_root_is_refused() {
        /// Removes the folder however the test ends, a failed assertion included.
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let temp = TempDir(
            std::env::temp_dir().join(format!("jstream-sources-opened-{}", std::process::id())),
        );
        let dir = &temp.0;
        let _ = fs::remove_dir_all(dir);
        let root = dir.join("root");
        fs::create_dir_all(&root).expect("a temp folder");
        fs::write(root.join("a.png"), b"inside").expect("a file");
        fs::write(dir.join("outside.png"), b"outside").expect("a file");
        let base = root.canonicalize().expect("the root");
        let joined = base.join("a.png");

        // Opened through a path that pointed outside, checked once it points back in.
        let outside = File::open(dir.join("outside.png")).expect("open");
        assert!(matches!(
            check_opened(&outside, &base, &joined),
            Err(Opened::Escapes)
        ));
        // The file the path names, opened and checked, is accepted.
        let inside = File::open(&joined).expect("open");
        assert!(check_opened(&inside, &base, &joined).is_ok());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{AssetRoots, Opened, open_inside, walk_below};
    use crate::SourceError;
    use jstream_protocol::AssetRoot;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Removes the folder however the test ends, a failed assertion included.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Checks that `rel`, which goes through a link inside `root` to `outside.png`
    /// outside it, is refused by `resolve`, `read` and `open_inside` (at the resolve
    /// step), and by the walk on its own, as if the link had been swapped in after the
    /// path resolved, while the target is reachable through the link.
    fn refused(roots: &AssetRoots, base: &Path, rel: &str) {
        assert_eq!(
            fs::read(base.join(rel)).expect("the link leads to the file outside"),
            b"outside"
        );
        assert!(matches!(
            roots.resolve(AssetRoot::Collection, rel),
            Err(SourceError::AssetEscapesRoot { .. })
        ));
        assert!(matches!(
            roots.read(AssetRoot::Collection, rel, 1024),
            Err(SourceError::AssetEscapesRoot { .. })
        ));
        assert!(matches!(open_inside(base, rel), Err(Opened::Escapes)));
        // The walk's own link check, which `open_inside` reaches only when a link
        // appears between resolving and walking.
        assert!(matches!(
            walk_below(base, Path::new(rel)),
            Err(Opened::Escapes)
        ));
    }

    #[test]
    fn links_inside_the_root_to_outside_are_refused() {
        let temp = TempDir(
            std::env::temp_dir().join(format!("jstream-sources-links-{}", std::process::id())),
        );
        let dir = &temp.0;
        let _ = fs::remove_dir_all(dir);
        let root = dir.join("root");
        let outside = dir.join("outside");
        fs::create_dir_all(&root).expect("a temp folder");
        fs::create_dir_all(&outside).expect("a temp folder");
        fs::write(root.join("a.png"), b"inside").expect("a file");
        fs::write(outside.join("outside.png"), b"outside").expect("a file");
        let roots = AssetRoots {
            collection: Some(root.clone()),
            app: None,
        };
        let base = root.canonicalize().expect("the root");

        // A file inside the root reads.
        assert_eq!(
            roots
                .read(AssetRoot::Collection, "a.png", 1024)
                .expect("inside"),
            b"inside"
        );
        // The walk opens a plain file inside the root.
        assert!(walk_below(&base, Path::new("a.png")).is_ok());

        // A junction needs no privilege, so it is always tested.
        let made = Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(root.join("junction"))
            .arg(&outside)
            .output()
            .expect("cmd runs");
        assert!(made.status.success(), "mklink /J: {made:?}");
        refused(&roots, &base, "junction/outside.png");

        // Symbolic links need a privilege (or Developer Mode) the runner may not have.
        match std::os::windows::fs::symlink_dir(&outside, root.join("dirlink")) {
            Ok(()) => refused(&roots, &base, "dirlink/outside.png"),
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "note: directory symlink not tested, can't create one: {e}"
                );
            }
        }
        match std::os::windows::fs::symlink_file(
            outside.join("outside.png"),
            root.join("filelink.png"),
        ) {
            Ok(()) => refused(&roots, &base, "filelink.png"),
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "note: file symlink not tested, can't create one: {e}"
                );
            }
        }
    }
}
