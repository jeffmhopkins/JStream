//! The CLAP audio plugin scan on the wire (0.95, ADR-0035): where the engine looks for
//! `.clap` files, the effects it found, and the files it couldn't scan, for
//! `plugin.clap_scan` and `plugin.clap_rescan`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `plugin.clap_scan` reports: whether this engine hosts CLAP plugins, the folders
/// it searches, and what its last scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ClapScanInfo {
    /// Whether this engine hosts CLAP plugins at all. `false` on a platform with no
    /// plugin host yet, and when the host couldn't be set up; `reason` then says why.
    pub available: bool,
    /// Why CLAP plugins are unavailable, when `available` is `false` and the engine knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether a scan is running now. The lists are the last finished scan's until it
    /// ends.
    #[serde(default)]
    pub scanning: bool,
    /// Whether a scan has finished since the engine started. Until one has, `found` and
    /// `failed` are empty.
    #[serde(default)]
    pub scanned: bool,
    /// The folders searched, in CLAP's order, each with whether it exists. The scan looks
    /// in the ones that do, and below them.
    #[serde(default)]
    pub folders: Vec<ClapFolder>,
    /// The audio effects the last scan found, ordered by kind ID. Each is the filter kind
    /// `kind_id` in `kind.list`.
    #[serde(default)]
    pub found: Vec<ClapFound>,
    /// The plugin files the last scan couldn't read, in the order it met them.
    #[serde(default)]
    pub failed: Vec<ClapFailed>,
}

impl ClapScanInfo {
    /// An engine that hosts no CLAP plugins, with why when it knows.
    pub fn unavailable(reason: Option<String>) -> Self {
        Self {
            reason,
            ..Self::default()
        }
    }

    /// An engine that hosts CLAP plugins: the folders it searches and its last scan.
    pub fn available(
        folders: Vec<ClapFolder>,
        scanning: bool,
        scanned: bool,
        found: Vec<ClapFound>,
        failed: Vec<ClapFailed>,
    ) -> Self {
        Self {
            available: true,
            reason: None,
            scanning,
            scanned,
            folders,
            found,
            failed,
        }
    }
}

/// A folder the CLAP scan searches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ClapFolder {
    /// The folder's path, as the engine's platform writes it.
    pub path: String,
    /// Whether it exists. One that doesn't is where plugins could be installed.
    pub exists: bool,
}

impl ClapFolder {
    /// A searched folder.
    pub fn new(path: impl Into<String>, exists: bool) -> Self {
        Self {
            path: path.into(),
            exists,
        }
    }
}

/// An audio effect the CLAP scan found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ClapFound {
    /// Its filter kind, `clap/<plugin id>`.
    pub kind_id: String,
    /// The plugin's name, as its file gives it: the plugin's own text, to show as data.
    pub name: String,
    /// The plugin file it is in.
    pub file: String,
}

impl ClapFound {
    /// A found effect.
    pub fn new(
        kind_id: impl Into<String>,
        name: impl Into<String>,
        file: impl Into<String>,
    ) -> Self {
        Self {
            kind_id: kind_id.into(),
            name: name.into(),
            file: file.into(),
        }
    }
}

/// A plugin file the CLAP scan couldn't read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ClapFailed {
    /// The file.
    pub file: String,
    /// Why: the host's error, such as a crash, a timeout or a plugin host that can't
    /// start.
    pub reason: String,
}

impl ClapFailed {
    /// A file that failed.
    pub fn new(file: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            reason: reason.into(),
        }
    }
}
