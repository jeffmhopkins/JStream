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

/// What sources are created with: where asset references point, and how much a
/// decoded asset may take.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// The folders behind asset roots.
    pub assets: AssetRoots,
    /// Size limits for decoded assets.
    pub limits: Limits,
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
    /// # Errors
    /// [`SourceError::NoAssetRoot`] when the root has no folder,
    /// [`SourceError::AssetRead`] when the file or its root can't be resolved, and
    /// [`SourceError::AssetEscapesRoot`] when it leaves its root.
    pub fn resolve(&self, root: AssetRoot, path: &str) -> Result<PathBuf, SourceError> {
        let read_err = |e: std::io::Error| SourceError::AssetRead {
            path: path.to_owned(),
            kind: e.kind(),
        };
        let base = match root {
            AssetRoot::Collection => self.collection.as_deref(),
            AssetRoot::App => self.app.as_deref(),
            AssetRoot::Absolute => return Path::new(path).canonicalize().map_err(read_err),
            // `AssetRoot` is non-exhaustive: a root this crate doesn't know has no folder.
            _ => None,
        };
        let base = base.ok_or(SourceError::NoAssetRoot { root })?;
        let base = base.canonicalize().map_err(read_err)?;
        let file = base.join(path).canonicalize().map_err(read_err)?;
        if file.starts_with(&base) {
            Ok(file)
        } else {
            Err(SourceError::AssetEscapesRoot {
                root,
                path: path.to_owned(),
            })
        }
    }

    /// Reads the file an asset reference names, up to `max_bytes`.
    ///
    /// # Errors
    /// Those of [`resolve`](Self::resolve), [`SourceError::AssetRead`] when reading
    /// fails, and [`SourceError::TooLarge`] when the file is longer than `max_bytes`.
    pub fn read(
        &self,
        root: AssetRoot,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SourceError> {
        let read_err = |e: std::io::Error| SourceError::AssetRead {
            path: path.to_owned(),
            kind: e.kind(),
        };
        let file = File::open(self.resolve(root, path)?).map_err(read_err)?;
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
