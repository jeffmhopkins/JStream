//! What lands on the Edit view's tracks from outside it: a scene dragged from the Scenes
//! panel, and media files dropped from the desktop (the desktop app's own drop) or a
//! file manager (a browser's drop with a URI list). Each drop is one undo step: a scene
//! is one `show.set`; files are one batch that makes their scenes or sources
//! ([`super::super::drop`]) and then sets the show.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::super::ShowPanel;
use super::super::drop::{DropIds, DropTarget, DroppedFile, plan_drop, source_for};
use super::super::edits;
use super::super::model::{ClipKind, TrackKind, TrackView};
use crate::store::Collection;
use jstream_control::protocol::Edit;

/// How long a scene dragged onto a track lasts there: 30 s. A scene has no length of
/// its own, and on V1 it ends in a pausing bookmark, so its length matters only for
/// what is timed inside it.
pub const DEFAULT_SCENE_MS: u32 = 30_000;

/// The length of the media `file` plays, once the engine reports one. Source sizes don't
/// carry a duration yet, so it is unknown for now, and a dropped video or audio clip
/// takes the show's image length until it is.
pub fn media_length_ms(collection: &Collection, file: &DroppedFile) -> Option<u32> {
    let _playing = source_for(collection, file);
    None
}

/// Places scene `scene` (called `name`) on `track` of show `show_id` at `at_ms`, or after
/// program's last clip when it was let go on the drop slot (`slot`). On V1 it goes in
/// by the clip it lands on and ends in a pausing bookmark; on an overlay track, where
/// it fits; a keyframe scene goes on K as a keyframe marker. Anything else is refused
/// in the problem bar.
pub fn place_scene(
    panel: &ShowPanel,
    show_id: &str,
    track: &TrackView,
    (scene, name): (&str, &str),
    at_ms: u32,
    slot: bool,
) {
    let keyframe = panel
        .collection(|c| c.is_keyframe(&scene.into()))
        .unwrap_or(false);
    let tag = track.kind.tag();
    match (track.kind, keyframe) {
        (TrackKind::Keyframes, true) => panel.set_show(show_id, |s| {
            edits::add_keyframe(s, at_ms, Some(scene.into()));
        }),
        (TrackKind::Keyframes, false) => {
            panel.refuse(format!(
                "{name} is a scene: drop it on a V track. K takes keyframes."
            ));
        }
        (_, true) => panel.refuse(format!("{name} is a keyframe: drop it on K.")),
        (TrackKind::Program, false) => panel.set_show(show_id, |s| {
            let at = if slot { u32::MAX } else { at_ms };
            edits::insert_program_clip(s, scene.into(), name, at, DEFAULT_SCENE_MS, true);
        }),
        (TrackKind::Overlay(_), false) => {
            let mut fits = true;
            panel.set_show(show_id, |s| {
                fits = edits::add_overlay_clip(s, &track.id, scene.into(), at_ms, DEFAULT_SCENE_MS)
                    .is_some();
            });
            if !fits {
                panel.refuse(format!(
                    "{name} doesn't fit on {tag} there: make room, or try another spot."
                ));
            }
        }
        (TrackKind::Sequences(_) | TrackKind::Audio(_), false) => {
            panel.refuse(format!("A scene goes on a V track, not {tag}."));
        }
    }
}

/// Places the files at `paths` (on the engine's machine) on `track` of show `show_id`
/// from `at_ms` on, one after another, as one batch: the "Show media" folder, sources
/// and scenes each needs, then the show. `folder` is the collection's folder, so a file
/// under it is kept relative. A file that can't go there is refused in the problem bar
/// and the rest still go.
pub fn place_files(
    panel: &ShowPanel,
    show_id: &str,
    track: &TrackView,
    paths: &[String],
    at_ms: u32,
    slot: bool,
    folder: Option<&str>,
) {
    let Some(mut show) = panel.info(show_id) else {
        return;
    };
    // The same file twice in one drop goes in once: each copy would be planned against
    // the same collection and make its own source and scene.
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<String> = paths.iter().filter(|p| seen.insert(*p)).cloned().collect();
    let before = show.clone();
    let fresh = |prefix: &str| panel.fresh_id(prefix).unwrap_or_default();
    let folder_id = fresh("folder");
    let ids: Vec<DropIds> = paths
        .iter()
        .map(|_| DropIds {
            folder: folder_id.clone().into(),
            source: fresh("source").into(),
            scene: fresh("scene").into(),
            item: fresh("item").into(),
            clip: String::new(),
        })
        .collect();
    let mut at = if slot && track.kind == TrackKind::Program {
        u32::MAX
    } else {
        at_ms
    };
    let mut batch: Vec<Edit> = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    panel.collection(|collection| {
        for (path, ids) in paths.iter().zip(&ids) {
            let (root, relative) = crate::browse::place_asset(folder, path);
            let mut file = DroppedFile::new(root, relative);
            file.length_ms = media_length_ms(collection, &file);
            let target = DropTarget {
                track: track.kind,
                at_ms: at.min(show.length_ms()),
                image_ms: show.image_ms,
            };
            let plan = match plan_drop(collection, &file, target, ids) {
                Ok(plan) => plan,
                Err(refusal) => {
                    refusals.push(refusal.to_string());
                    continue;
                }
            };
            let len = plan.clip.len_ms;
            let placed = match (track.kind, &plan.clip.kind) {
                (TrackKind::Program, ClipKind::Scene(scene)) => {
                    edits::insert_program_clip(
                        &mut show,
                        scene.as_str().into(),
                        &plan.clip.label,
                        at,
                        len,
                        plan.pause_at_end,
                    );
                    true
                }
                (TrackKind::Overlay(_), ClipKind::Scene(scene)) => {
                    edits::add_overlay_clip(&mut show, &track.id, scene.as_str().into(), at, len)
                        .is_some()
                }
                (TrackKind::Audio(_), _) => edits::add_audio_clip(
                    &mut show,
                    &track.id,
                    plan.source.clone(),
                    at,
                    len,
                    plan.clip.fades.unwrap_or_default(),
                )
                .is_some(),
                _ => false,
            };
            if !placed {
                refusals.push(format!("{} doesn't fit there.", file.name()));
                continue;
            }
            for edit in plan.edits {
                let again = matches!(&edit, Edit::FolderCreate { .. }) && batch.contains(&edit);
                if !again {
                    batch.push(edit);
                }
            }
            if at != u32::MAX {
                at = at.saturating_add(len);
            }
        }
    });
    if let Some(first) = refusals.into_iter().next() {
        panel.refuse(first);
    }
    if show == before {
        return;
    }
    batch.push(Edit::ShowSet { show });
    panel.apply(batch);
}
