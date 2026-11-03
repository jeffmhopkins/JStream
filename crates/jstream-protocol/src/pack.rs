//! Effect packs written over the protocol (0.71,
//! [ADR-0097](../../../docs/adr/0097-agents-write-effect-packs.md)): the files a
//! `pack.write` carries, and what `pack.list` and `pack.read` answer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most files one `pack.write` carries.
pub const MAX_PACK_FILES: usize = 64;

/// The most bytes a `pack.write`'s `files` take as sent: each path and each file's text
/// or base64, as JSON strings, escapes included. With the request's envelope, the whole
/// message stays under the 4 MiB request limit (ADR-0074).
pub const MAX_PACK_WRITE_BYTES: usize = 3 << 20;

/// The most bytes one image of a pack takes, decoded: what `pack.write` accepts, and the
/// largest `pack.read` returns.
pub const MAX_PACK_IMAGE_BYTES: usize = 2 << 20;

/// One file of a `pack.write`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackFile {
    /// Its path in the pack, `/`-separated: `effect.toml`, or a `.wgsl`, `.png`, `.jpg`
    /// or `.jpeg` file, under the asset-path rules (no `..`, no absolute path, no
    /// component that starts with `.`, no `\` or `:`, no Windows reserved name).
    pub path: String,
    /// Its contents.
    #[serde(flatten)]
    pub content: PackContent,
}

/// A pack file's contents: text for `effect.toml` and `.wgsl` files, base64 for images.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackContent {
    /// UTF-8 text.
    Text(String),
    /// Bytes, as standard base64 with padding.
    Base64(String),
}

/// One pack in the packs folder, as `pack.list` and `pack.write` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackInfo {
    /// Its pack ID, from its `effect.toml`.
    pub id: String,
    /// The folder it is in, within the packs folder. A pack written over the protocol is
    /// in the folder named by its ID; one a person installed may be in another.
    pub folder: String,
    /// Its kinds, `<pack>/<id>`, effects then sources, while it is loaded. Empty for a
    /// pack that doesn't load.
    pub kinds: Vec<String>,
    /// Whether the engine loads it: `false` for a pack whose manifest or shaders fail
    /// to load, or whose ID another folder already holds.
    pub loaded: bool,
    /// The client name (from its `hello`) of the last `pack.write` of this pack ID,
    /// which says who claims to have written it, not who proved it. It stays when a
    /// person later edits or replaces the folder by hand. Omitted for a pack ID no
    /// client has written, and when the last writer gave no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
}

/// One file of a pack, as `pack.read` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackFileInfo {
    /// Its path in the pack, `/`-separated.
    pub path: String,
    /// Its size in bytes.
    pub size: u64,
    /// The BLAKE3 digest of its bytes, as lower-case hex.
    pub digest: String,
    /// The text of an `effect.toml` or `.wgsl` file. Omitted for an image, and in a
    /// whole-pack read for a file past its 3 MiB budget of text: read it by `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// An image's bytes as base64, only when `pack.read` named that image by `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}
