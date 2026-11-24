//! A scene's own folders (ADR-0219): folders under a scene that hold its keyframe scenes
//! and timelines, the owners `folder.create`, `folder.set_parent`, `scene.set_folder`
//! and `timeline.set` check, what blocks a folder's delete and its plan, a deleted
//! scene's folders and their undo, and the collection file.

use super::{apply_err, apply_ok, blocker, blockers, fixture, invalid_reason, plan};
use crate::Graph;
use jstream_protocol::{
    DeleteTarget, Edit, EntityKind, FolderId, ProtocolError, SceneId, TimelineId, TimelineInfo,
    TimelineLoop,
};

fn folder(id: &str, scene: Option<&str>, parent: Option<&str>) -> Edit {
    Edit::FolderCreate {
        folder_id: FolderId::new(id),
        name: id.to_uppercase(),
        parent: parent.map(FolderId::new),
        scene_id: scene.map(SceneId::new),
    }
}

fn timeline(id: &str, folder: Option<&str>) -> TimelineInfo {
    TimelineInfo {
        timeline_id: TimelineId::new(id),
        name: String::new(),
        duration_ms: 1000,
        looping: TimelineLoop::Once,
        cues: Vec::new(),
        labels: Vec::new(),
        lanes: Vec::new(),
        starts: Vec::new(),
        stops: Vec::new(),
        folder: folder.map(FolderId::new),
    }
}

fn set(scene: &str, id: &str, folder: Option<&str>) -> Edit {
    Edit::TimelineSet {
        scene_id: SceneId::new(scene),
        timeline: timeline(id, folder),
    }
}

fn file(scene: &str, folder: Option<&str>) -> Edit {
    Edit::SceneSetFolder {
        scene_id: SceneId::new(scene),
        folder_id: folder.map(FolderId::new),
    }
}

/// The fixture with keyframe `k` of `a`, a scene-list folder `list`, folder `looks`
/// under `a` with `intro` inside it, folder `other` under `b`, keyframe `k` and
/// timeline `t` (on `a`) in `intro`, and timeline `u` on `k` in `looks`.
fn filed() -> Graph {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("k"),
                variant_of: SceneId::new("a"),
                name: "K".into(),
            },
            folder("list", None, None),
            folder("looks", Some("a"), None),
            folder("intro", Some("a"), Some("looks")),
            folder("other", Some("b"), None),
            file("k", Some("intro")),
            set("a", "t", Some("intro")),
            // A keyframe scene's timeline files in its parent's folders.
            set("k", "u", Some("looks")),
        ],
    );
    graph
}

#[test]
fn a_scenes_folders_hold_its_keyframes_and_timelines() {
    let graph = filed();
    let state = graph.state();
    let looks = state.folder(&FolderId::new("looks")).expect("looks");
    assert_eq!(looks.scene(), Some(&SceneId::new("a")));
    let jstream_protocol::QueryResult::StateGet { folders, .. } = graph.state_get() else {
        panic!("state.get");
    };
    let intro = folders
        .iter()
        .find(|f| f.folder_id == FolderId::new("intro"))
        .expect("intro is reported");
    assert_eq!(intro.scene_id, Some(SceneId::new("a")));
    assert_eq!(intro.parent, Some(FolderId::new("looks")));
}

#[test]
fn each_edit_checks_the_folders_owner() {
    let mut graph = filed();
    let refused = |graph: &mut Graph, edit: Edit| invalid_reason(apply_err(graph, vec![edit])).0;
    // A list scene goes only in the list's folders, a keyframe only in its parent's.
    assert_eq!(
        refused(&mut graph, file("b", Some("looks"))),
        "edits.0.folder_id"
    );
    assert_eq!(
        refused(&mut graph, file("k", Some("list"))),
        "edits.0.folder_id"
    );
    assert_eq!(
        refused(&mut graph, file("k", Some("other"))),
        "edits.0.folder_id"
    );
    // A timeline only in its scene's family's.
    assert_eq!(
        refused(&mut graph, set("a", "t", Some("other"))),
        "edits.0.timeline.folder"
    );
    assert_eq!(
        refused(&mut graph, set("b", "v", Some("looks"))),
        "edits.0.timeline.folder"
    );
    assert_eq!(
        refused(&mut graph, set("a", "t", Some("list"))),
        "edits.0.timeline.folder"
    );
    // A folder sits only in a folder of the same owner.
    assert_eq!(
        refused(&mut graph, folder("x", Some("b"), Some("looks"))),
        "edits.0.parent"
    );
    assert_eq!(
        refused(&mut graph, folder("x", None, Some("looks"))),
        "edits.0.parent"
    );
    let reparent = Edit::FolderSetParent {
        folder_id: FolderId::new("other"),
        parent: Some(FolderId::new("looks")),
    };
    assert_eq!(refused(&mut graph, reparent), "edits.0.parent");
    // A keyframe scene has no folders of its own.
    assert_eq!(
        refused(&mut graph, folder("x", Some("k"), None)),
        "edits.0.scene_id"
    );
    assert!(matches!(
        apply_err(&mut graph, vec![folder("x", Some("gone"), None)]),
        ProtocolError::NotFound { .. }
    ));
    assert!(matches!(
        apply_err(&mut graph, vec![set("a", "t", Some("gone"))]),
        ProtocolError::NotFound { .. }
    ));
    // Moving within the owner's folders works, and takes them out again.
    apply_ok(
        &mut graph,
        vec![
            file("k", Some("looks")),
            set("a", "t", None),
            Edit::FolderSetParent {
                folder_id: FolderId::new("intro"),
                parent: None,
            },
            file("k", None),
        ],
    );
}

#[test]
fn a_folder_holding_a_timeline_is_not_deleted_and_its_plan_moves_it_up() {
    let mut graph = filed();
    let error = apply_err(
        &mut graph,
        vec![Edit::FolderDelete {
            folder_id: FolderId::new("intro"),
        }],
    );
    assert_eq!(
        blockers(error),
        vec![
            blocker(EntityKind::Scene, "k", "folder"),
            blocker(EntityKind::Timeline, "t", "folder"),
        ]
    );
    let edits = plan(
        &graph,
        DeleteTarget::Folder {
            folder_id: FolderId::new("intro"),
        },
    );
    assert_eq!(
        edits,
        vec![
            file("k", Some("looks")),
            set("a", "t", Some("looks")),
            Edit::FolderDelete {
                folder_id: FolderId::new("intro"),
            },
        ]
    );
    apply_ok(&mut graph, edits);
}

#[test]
fn a_scenes_folders_go_with_it_and_its_undo_puts_them_back() {
    let mut graph = filed();
    // `apply_ok` checks that the inverse brings back the same state, folders in the
    // same places among the others and the keyframe and timelines filed again.
    let edits = plan(
        &graph,
        DeleteTarget::Scene {
            scene_id: SceneId::new("a"),
        },
    );
    apply_ok(&mut graph, edits);
    let left: Vec<&str> = graph
        .state()
        .folders()
        .iter()
        .map(|f| f.id().as_str())
        .collect();
    assert_eq!(left, ["list", "other"]);
}

#[test]
fn the_collection_file_keeps_a_scenes_folders() {
    let mut graph = filed();
    // A scene folder before a list folder in folder order loads in its place.
    apply_ok(
        &mut graph,
        vec![Edit::FolderReorder {
            folder_id: FolderId::new("intro"),
            index: 0,
        }],
    );
    let json = graph.to_collection_json();
    let loaded = graph.clone().with_collection_json(&json).expect("loads");
    assert_eq!(loaded.state(), graph.state());
}
