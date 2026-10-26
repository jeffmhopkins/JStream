//! The mock's folders, labels and organisation metadata (ADR-0024 §3 and §4,
//! docs/adr/0024-scene-structure.md): the `folder.*` and `label.*` edits,
//! `scene.set_folder`, `scene.set_meta` and `source.set_meta`, with the checks
//! `jstream-graph` makes, in the same order and with the same errors and inverses. The
//! value checks are the protocol's own ([`check_icon`], [`check_color`],
//! [`SceneMeta::problem`]), so the mock rejects exactly what the engine rejects.

use super::{
    Collection, already_exists, check_id, check_index, check_name, invalid, not_found, wire_index,
};
use jstream_protocol::{
    Blocker, Edit, EntityKind, FolderId, FolderInfo, LabelId, LabelInfo, ProtocolError, SceneId,
    SceneMeta, SourceId, SourceMeta, check_color, check_icon,
};
use std::collections::BTreeSet;

impl Collection {
    /// Applies an organisation edit and returns the edits that undo it. `None` for any
    /// other edit.
    pub(super) fn apply_organise(
        &mut self,
        edit: &Edit,
        at: usize,
    ) -> Option<Result<Vec<Edit>, ProtocolError>> {
        Some(match edit {
            Edit::FolderCreate {
                folder_id,
                name,
                parent,
            } => self.create_folder(folder_id, name, parent.as_ref(), at),
            Edit::FolderRename { folder_id, name } => self.rename_folder(folder_id, name, at),
            Edit::FolderSetParent { folder_id, parent } => {
                self.set_parent(folder_id, parent.as_ref(), at)
            }
            Edit::FolderReorder { folder_id, index } => reorder(
                &mut self.folders,
                |f| &f.folder_id == folder_id,
                *index,
                at,
                "folders",
                || not_found(EntityKind::Folder, folder_id.as_str()),
            )
            .map(|from| {
                vec![Edit::FolderReorder {
                    folder_id: folder_id.clone(),
                    index: from,
                }]
            }),
            Edit::FolderDelete { folder_id } => self.delete_folder(folder_id, at),
            Edit::SceneSetFolder {
                scene_id,
                folder_id,
            } => self.set_folder(scene_id, folder_id.as_ref()),
            Edit::SceneSetMeta { scene_id, meta } => self.set_scene_meta(scene_id, meta, at),
            Edit::LabelCreate {
                label_id,
                name,
                color,
                icon,
            } => self.create_label(label_id, name, color, icon.as_deref(), at),
            Edit::LabelUpdate {
                label_id,
                name,
                color,
                icon,
            } => self.update_label(label_id, name, color, icon.as_deref(), at),
            Edit::LabelReorder { label_id, index } => reorder(
                &mut self.labels,
                |l| &l.label_id == label_id,
                *index,
                at,
                "labels",
                || not_found(EntityKind::Label, label_id.as_str()),
            )
            .map(|from| {
                vec![Edit::LabelReorder {
                    label_id: label_id.clone(),
                    index: from,
                }]
            }),
            Edit::LabelDelete { label_id } => self.delete_label(label_id, at),
            Edit::SourceSetMeta { source_id, meta } => self.set_source_meta(source_id, meta, at),
            _ => return None,
        })
    }

    fn folder_index(&self, id: &FolderId) -> Result<usize, ProtocolError> {
        self.folders
            .iter()
            .position(|f| &f.folder_id == id)
            .ok_or_else(|| not_found(EntityKind::Folder, id.as_str()))
    }

    fn label_index(&self, id: &LabelId) -> Result<usize, ProtocolError> {
        self.labels
            .iter()
            .position(|l| &l.label_id == id)
            .ok_or_else(|| not_found(EntityKind::Label, id.as_str()))
    }

    /// Everything that stops deleting folder `id`: the scenes filed in it (display
    /// order), then the folders inside it (folder order).
    fn folder_blockers(&self, id: &FolderId) -> Vec<Blocker> {
        let scenes = self
            .scenes
            .iter()
            .filter(|s| s.folder.as_ref() == Some(id))
            .map(|s| blocker(EntityKind::Scene, s.id.as_str(), "folder"));
        let folders = self
            .folders
            .iter()
            .filter(|f| f.parent.as_ref() == Some(id))
            .map(|f| blocker(EntityKind::Folder, f.folder_id.as_str(), "parent"));
        scenes.chain(folders).collect()
    }

    /// Everything that stops deleting label `id`: the sources carrying it, by ID, then
    /// the scenes carrying it, in display order.
    fn label_blockers(&self, id: &LabelId) -> Vec<Blocker> {
        let sources = self
            .sources
            .iter()
            .filter(|(_, s)| s.meta.labels.contains(id))
            .map(|(source_id, _)| blocker(EntityKind::Source, source_id.as_str(), "labels"));
        let scenes = self
            .scenes
            .iter()
            .filter(|s| s.meta.labels.contains(id))
            .map(|s| blocker(EntityKind::Scene, s.id.as_str(), "labels"));
        sources.chain(scenes).collect()
    }

    /// `labels` ordered by ID, as the engine stores them: `not_found` for a label that
    /// doesn't exist, `invalid_argument` for one listed twice.
    fn checked_labels(&self, labels: &[LabelId], at: usize) -> Result<Vec<LabelId>, ProtocolError> {
        let mut set = BTreeSet::new();
        for label in labels {
            self.label_index(label)?;
            if !set.insert(label.clone()) {
                return Err(invalid(
                    at,
                    "labels",
                    format!("label `{label}` is listed more than once"),
                ));
            }
        }
        Ok(set.into_iter().collect())
    }

    fn create_folder(
        &mut self,
        id: &FolderId,
        name: &str,
        parent: Option<&FolderId>,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        check_id(id.as_str(), at, "folder_id")?;
        check_name(name, at)?;
        if self.folders.iter().any(|f| &f.folder_id == id) {
            return Err(already_exists(EntityKind::Folder, id.as_str()));
        }
        if let Some(parent) = parent {
            self.folder_index(parent)?;
        }
        self.folders.push(FolderInfo {
            folder_id: id.clone(),
            name: name.to_owned(),
            parent: parent.cloned(),
        });
        Ok(vec![Edit::FolderDelete {
            folder_id: id.clone(),
        }])
    }

    fn rename_folder(
        &mut self,
        id: &FolderId,
        name: &str,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let fi = self.folder_index(id)?;
        check_name(name, at)?;
        let old = std::mem::replace(&mut self.folders[fi].name, name.to_owned());
        Ok(vec![Edit::FolderRename {
            folder_id: id.clone(),
            name: old,
        }])
    }

    fn set_parent(
        &mut self,
        id: &FolderId,
        parent: Option<&FolderId>,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let fi = self.folder_index(id)?;
        if let Some(parent) = parent {
            self.folder_index(parent)?;
            // Walk up from the new parent: reaching the folder means it would sit inside
            // itself. Parents always exist and never loop, so the walk ends.
            let mut path = vec![parent.clone()];
            let mut current = Some(parent.clone());
            while let Some(folder) = current {
                if &folder == id {
                    let chain: Vec<String> = path.iter().rev().map(|f| format!("`{f}`")).collect();
                    return Err(invalid(
                        at,
                        "parent",
                        format!(
                            "folder `{id}` would sit inside itself: {} -> `{id}`",
                            chain.join(" -> ")
                        ),
                    ));
                }
                current = self
                    .folders
                    .iter()
                    .find(|f| f.folder_id == folder)
                    .and_then(|f| f.parent.clone());
                if let Some(next) = &current {
                    path.push(next.clone());
                }
            }
        }
        let old = std::mem::replace(&mut self.folders[fi].parent, parent.cloned());
        Ok(vec![Edit::FolderSetParent {
            folder_id: id.clone(),
            parent: old,
        }])
    }

    fn delete_folder(&mut self, id: &FolderId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
        let fi = self.folder_index(id)?;
        let blockers = self.folder_blockers(id);
        if !blockers.is_empty() {
            return Err(blocked(
                at,
                "folder_id",
                &format!("folder `{id}`"),
                blockers,
            ));
        }
        let folder = self.folders.remove(fi);
        let mut inverse = vec![Edit::FolderCreate {
            folder_id: folder.folder_id.clone(),
            name: folder.name,
            parent: folder.parent,
        }];
        // Re-created folders are appended, so only a folder that wasn't last moves back.
        if fi != self.folders.len() {
            inverse.push(Edit::FolderReorder {
                folder_id: folder.folder_id,
                index: wire_index(fi)?,
            });
        }
        Ok(inverse)
    }

    fn set_folder(
        &mut self,
        scene_id: &SceneId,
        folder_id: Option<&FolderId>,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let si = self.existing_scene(scene_id)?;
        if let Some(folder_id) = folder_id {
            self.folder_index(folder_id)?;
        }
        let old = std::mem::replace(&mut self.scenes[si].folder, folder_id.cloned());
        Ok(vec![Edit::SceneSetFolder {
            scene_id: scene_id.clone(),
            folder_id: old,
        }])
    }

    fn set_scene_meta(
        &mut self,
        scene_id: &SceneId,
        meta: &SceneMeta,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let si = self.existing_scene(scene_id)?;
        if let Some((name, reason)) = meta.problem() {
            return Err(invalid(at, name, reason));
        }
        let new = SceneMeta {
            labels: self.checked_labels(&meta.labels, at)?,
            ..meta.clone()
        };
        let old = std::mem::replace(&mut self.scenes[si].meta, new);
        Ok(vec![Edit::SceneSetMeta {
            scene_id: scene_id.clone(),
            meta: old,
        }])
    }

    fn create_label(
        &mut self,
        id: &LabelId,
        name: &str,
        color: &str,
        icon: Option<&str>,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        check_id(id.as_str(), at, "label_id")?;
        if self.labels.iter().any(|l| &l.label_id == id) {
            return Err(already_exists(EntityKind::Label, id.as_str()));
        }
        check_label(name, color, icon, at)?;
        self.labels.push(LabelInfo {
            label_id: id.clone(),
            name: name.to_owned(),
            color: color.to_owned(),
            icon: icon.map(str::to_owned),
        });
        Ok(vec![Edit::LabelDelete {
            label_id: id.clone(),
        }])
    }

    fn update_label(
        &mut self,
        id: &LabelId,
        name: &str,
        color: &str,
        icon: Option<&str>,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let li = self.label_index(id)?;
        check_label(name, color, icon, at)?;
        let label = &mut self.labels[li];
        Ok(vec![Edit::LabelUpdate {
            label_id: id.clone(),
            name: std::mem::replace(&mut label.name, name.to_owned()),
            color: std::mem::replace(&mut label.color, color.to_owned()),
            icon: std::mem::replace(&mut label.icon, icon.map(str::to_owned)),
        }])
    }

    fn delete_label(&mut self, id: &LabelId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
        let li = self.label_index(id)?;
        let blockers = self.label_blockers(id);
        if !blockers.is_empty() {
            return Err(blocked(at, "label_id", &format!("label `{id}`"), blockers));
        }
        let label = self.labels.remove(li);
        let mut inverse = vec![Edit::LabelCreate {
            label_id: label.label_id.clone(),
            name: label.name,
            color: label.color,
            icon: label.icon,
        }];
        if li != self.labels.len() {
            inverse.push(Edit::LabelReorder {
                label_id: label.label_id,
                index: wire_index(li)?,
            });
        }
        Ok(inverse)
    }

    fn set_source_meta(
        &mut self,
        source_id: &SourceId,
        meta: &SourceMeta,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        if !self.sources.contains_key(source_id) {
            return Err(not_found(EntityKind::Source, source_id.as_str()));
        }
        let labels = self.checked_labels(&meta.labels, at)?;
        let source = self
            .sources
            .get_mut(source_id)
            .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
        // Stored ordered by ID, as the engine reports them.
        let new = SourceMeta {
            labels,
            favourite: meta.favourite,
        };
        let old = std::mem::replace(&mut source.meta, new);
        Ok(vec![Edit::SourceSetMeta {
            source_id: source_id.clone(),
            meta: old,
        }])
    }
}

/// Checks a label's name, colour and icon, in the graph's order.
fn check_label(
    name: &str,
    color: &str,
    icon: Option<&str>,
    at: usize,
) -> Result<(), ProtocolError> {
    check_name(name, at)?;
    check_color(color).map_err(|reason| invalid(at, "color", reason))?;
    if let Some(icon) = icon {
        check_icon(icon).map_err(|reason| invalid(at, "icon", reason))?;
    }
    Ok(())
}

/// Moves the entry matching `is` to `index` in `list` and returns its old index, or
/// `missing()` if no entry matches.
fn reorder<T>(
    list: &mut Vec<T>,
    is: impl Fn(&T) -> bool,
    index: u32,
    at: usize,
    what: &str,
    missing: impl FnOnce() -> ProtocolError,
) -> Result<u32, ProtocolError> {
    let from = list.iter().position(is).ok_or_else(missing)?;
    let to = check_index(index, list.len(), at, what)?;
    let entry = list.remove(from);
    list.insert(to, entry);
    wire_index(from)
}

fn blocker(entity: EntityKind, id: &str, field: &str) -> Blocker {
    Blocker {
        entity,
        id: id.to_owned(),
        field: field.into(),
    }
}

/// The `invalid_argument` that refuses deleting `target` while `blockers` refer to it,
/// as the graph words it.
fn blocked(at: usize, field: &str, target: &str, blockers: Vec<Blocker>) -> ProtocolError {
    let list: Vec<String> = blockers
        .iter()
        .map(|b| format!("{} `{}` ({})", b.entity, b.id, b.field))
        .collect();
    ProtocolError::InvalidArgument {
        field: format!("edits.{at}.{field}"),
        reason: format!(
            "{target} is still referred to by {}; remove those references first, or send \
             the batch `delete.plan` returns",
            list.join(", ")
        ),
        blockers,
    }
}
