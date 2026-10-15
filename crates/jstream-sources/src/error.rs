//! Errors from creating, updating and running sources.

use jstream_protocol::AssetRoot;

/// Why a source could not be created, updated or asked for a frame.
///
/// No variant carries a host path: an asset is named by the root and path its setting
/// holds, which the client already knows (ADR-0025, docs/adr/0025-typed-settings-kind-descriptors.md).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// No kind with this ID is registered.
    #[error("unknown source kind `{kind_id}`")]
    UnknownKind {
        /// The kind asked for.
        kind_id: String,
    },
    /// The kind is described, so sources of it can be created in the collection, but
    /// nothing produces its frames yet.
    #[error("the `{kind_id}` source kind is described but not built yet")]
    NotBuilt {
        /// The kind asked for.
        kind_id: String,
    },
    /// The kind is audio-only: the engine records its sources through the platform's
    /// audio I/O ([`AudioTarget`](crate::AudioTarget)), and they make no frames.
    #[error("the `{kind_id}` source kind is audio-only and makes no frames")]
    AudioOnly {
        /// The kind asked for.
        kind_id: String,
    },
    /// A setting is missing or doesn't have its descriptor's type. The engine checks
    /// settings before a source exists, so this means a caller skipped that check.
    #[error("setting `{param}`: {reason}")]
    Setting {
        /// The parameter ID.
        param: String,
        /// What is wrong with it.
        reason: String,
    },
    /// An update changed a parameter that isn't `live`; the source has to be created
    /// again with the new settings ([`needs_restart`](crate::needs_restart)).
    #[error("setting `{param}` is not live; the source must be restarted")]
    NotLive {
        /// The parameter that changed.
        param: String,
    },
    /// The asset's root has no folder: an unsaved collection has no `collection`
    /// folder, and an engine may be run without an `app` one.
    #[error("no {root:?} asset folder is configured")]
    NoAssetRoot {
        /// The root the reference names.
        root: AssetRoot,
    },
    /// The asset path, once symlinks are resolved, leaves its root.
    #[error("asset `{path}` is outside its {root:?} folder")]
    AssetEscapesRoot {
        /// The root the reference names.
        root: AssetRoot,
        /// The path the reference holds.
        path: String,
    },
    /// The asset could not be read.
    #[error("asset `{path}` could not be read: {kind}")]
    AssetRead {
        /// The path the reference holds.
        path: String,
        /// What went wrong, without the host path.
        kind: std::io::ErrorKind,
    },
    /// The file's type is not one the image source decodes.
    #[error("asset `{path}` is not a PNG, JPEG, WebP or GIF image")]
    UnsupportedFormat {
        /// The path the reference holds.
        path: String,
    },
    /// The decoder rejected the file.
    #[error("asset `{path}` could not be decoded as {format}: {reason}")]
    Decode {
        /// The path the reference holds.
        path: String,
        /// The format it was decoded as.
        format: &'static str,
        /// The decoder's message.
        reason: String,
    },
    /// The image or its decoded frames are larger than [`Limits`](crate::Limits) allow.
    #[error("asset `{path}` is too large: {what}")]
    TooLarge {
        /// The path the reference holds.
        path: String,
        /// Which limit it passed.
        what: String,
    },
    /// The kind plays through a platform backend the engine wasn't given, such as media
    /// files on a platform without a media backend.
    #[error("this engine has no {backend} backend")]
    NoBackend {
        /// The backend, such as "media file".
        backend: &'static str,
    },
    /// A frame could not be built.
    #[error(transparent)]
    Media(#[from] jstream_media::MediaError),
}
