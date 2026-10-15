//! Hot reload: a watcher over a folder of pack folders, polled by its owner.
//!
//! Each [`PackWatcher::poll`] lists the pack folders (sub-folders holding an
//! `effect.toml`) and stamps every file in each one by modification time and size. A
//! folder whose stamp changed is reloaded, and the poll reports which kinds appeared,
//! changed or disappeared, for the engine's `kind.changed` events (ADR-0025). A reload
//! that fails keeps the last good version of the pack live and reports the error once,
//! so a half-saved shader never takes an effect off the air.
//!
//! Polling, not OS notifications: it needs no dependency and behaves the same on every
//! platform and file system, and a pack is a handful of small files. The cost is the
//! poll interval's latency, and an edit that keeps a file's size within the file
//! system's timestamp resolution (two seconds on FAT) is seen only on the next change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::PackError;
use crate::pack::{Effect, MANIFEST, Pack};

/// A poll interval that feels immediate to someone editing a shader.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The most files stamped in one pack folder; files past it are not watched.
const MAX_STAMPED_FILES: usize = 512;

/// How deep into a pack folder files are stamped.
const MAX_DEPTH: usize = 4;

/// What happened to one kind in a poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindChange {
    /// A new kind: its pack loaded, or gained the effect.
    Added(String),
    /// The kind's descriptor or WGSL changed.
    Changed(String),
    /// The kind is gone: its pack folder was removed, or the effect left the manifest.
    Removed(String),
}

/// The outcome of one poll.
#[derive(Debug, Default)]
pub struct Changes {
    /// Kind changes, grouped by pack folder.
    pub kinds: Vec<KindChange>,
    /// Packs that failed to load or reload since the last poll. A pack that fails a
    /// reload keeps its last good version.
    pub errors: Vec<PackError>,
}

/// A stamp of every file in a pack folder: path, modification time and size.
type Stamp = Vec<(PathBuf, Option<SystemTime>, u64)>;

#[derive(Debug)]
struct Folder {
    stamp: Stamp,
    pack: Option<Pack>,
}

/// Watches `root`, a folder of pack folders, for packs to load, reload and drop.
#[derive(Debug)]
pub struct PackWatcher {
    root: PathBuf,
    prelude: String,
    folders: BTreeMap<PathBuf, Folder>,
}

impl PackWatcher {
    /// A watcher of the pack folders in `root`, validating shaders against the host's
    /// `prelude`. Nothing is loaded until the first [`Self::poll`].
    pub fn new(root: impl Into<PathBuf>, prelude: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            prelude: prelude.into(),
            folders: BTreeMap::new(),
        }
    }

    /// The folder of pack folders.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Loads, reloads and drops packs to match the folders on disk. A missing `root`
    /// holds no packs.
    pub fn poll(&mut self) -> Changes {
        let mut changes = Changes::default();
        let present = pack_folders(&self.root);
        let gone: Vec<PathBuf> = self
            .folders
            .keys()
            .filter(|dir| !present.contains(dir))
            .cloned()
            .collect();
        for dir in gone {
            if let Some(Folder {
                pack: Some(pack), ..
            }) = self.folders.remove(&dir)
            {
                tracing::info!(pack = pack.id(), "effect pack removed");
                diff(Some(&pack), None, &mut changes.kinds);
            }
        }
        for dir in present {
            let stamp = stamp(&dir);
            if self.folders.get(&dir).is_some_and(|f| f.stamp == stamp) {
                continue;
            }
            let loaded = Pack::load(&dir, &self.prelude).and_then(|pack| {
                let taken = self.folders.iter().find(|(other, f)| {
                    **other != dir && f.pack.as_ref().is_some_and(|p| p.id() == pack.id())
                });
                match taken {
                    Some((first, _)) => Err(PackError::DuplicatePack {
                        path: dir.join(MANIFEST),
                        id: pack.id().to_owned(),
                        first: first.clone(),
                    }),
                    None => Ok(pack),
                }
            });
            let folder = self.folders.entry(dir).or_insert(Folder {
                stamp: Vec::new(),
                pack: None,
            });
            folder.stamp = stamp;
            match loaded {
                Ok(pack) => {
                    tracing::info!(
                        pack = pack.id(),
                        effects = pack.effects().len(),
                        "effect pack loaded"
                    );
                    diff(folder.pack.as_ref(), Some(&pack), &mut changes.kinds);
                    folder.pack = Some(pack);
                }
                Err(error) => {
                    tracing::warn!(%error, "effect pack not loaded");
                    changes.errors.push(error);
                }
            }
        }
        changes
    }

    /// The packs loaded now, by folder.
    pub fn packs(&self) -> impl Iterator<Item = &Pack> {
        self.folders.values().filter_map(|f| f.pack.as_ref())
    }

    /// The effect whose kind ID is `kind_id`, from whichever pack has it.
    pub fn effect(&self, kind_id: &str) -> Option<&Effect> {
        self.packs().find_map(|p| p.effect(kind_id))
    }
}

/// The sub-folders of `root` that hold a manifest, sorted.
fn pack_folders(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join(MANIFEST).is_file())
        .collect();
    dirs.sort();
    dirs
}

/// Every file under `dir`, to [`MAX_DEPTH`] and [`MAX_STAMPED_FILES`], sorted.
fn stamp(dir: &Path) -> Stamp {
    let mut out = Vec::new();
    let mut pending = vec![(dir.to_owned(), 0)];
    while let Some((at, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((entry.path(), depth + 1));
                }
            } else if out.len() < MAX_STAMPED_FILES {
                out.push((entry.path(), meta.modified().ok(), meta.len()));
            }
        }
    }
    out.sort();
    out
}

/// The kind changes from `old` to `new`.
fn diff(old: Option<&Pack>, new: Option<&Pack>, out: &mut Vec<KindChange>) {
    let kinds = |pack: Option<&Pack>| -> BTreeMap<String, Effect> {
        pack.map(|p| {
            p.effects()
                .iter()
                .map(|e| (e.kind_id().to_owned(), e.clone()))
                .collect()
        })
        .unwrap_or_default()
    };
    let (old, new) = (kinds(old), kinds(new));
    for (id, effect) in &new {
        match old.get(id) {
            None => out.push(KindChange::Added(id.clone())),
            Some(before)
                if before.descriptor() != effect.descriptor() || before.wgsl() != effect.wgsl() =>
            {
                out.push(KindChange::Changed(id.clone()));
            }
            Some(_) => {}
        }
    }
    for id in old.keys().filter(|id| !new.contains_key(*id)) {
        out.push(KindChange::Removed(id.clone()));
    }
}
