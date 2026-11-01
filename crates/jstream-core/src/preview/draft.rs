//! A preview stream of a source the collection doesn't hold (`preview.subscribe`'s
//! `source`): the Properties dialog's picture of the display, window or webcam being
//! picked, before the source exists or its new settings are saved.
//!
//! The draft is checked and planned once, at the subscription, on a collection of its
//! own: the engine's canvas and kinds, the one source, and one scene whose one item
//! covers the canvas, so the picture is the device fitted into the canvas as a new item
//! filling it would show it. The plan's one capture is the draft's: a display or window
//! is captured for the stream alone, and a webcam or capture card shares the open the
//! program or a scene already has (`runtime/devices.rs`). Nothing here touches the
//! engine's collection, its sources or its plan, so a draft is never shown on program,
//! heard, or listed.

use std::sync::Arc;

use jstream_graph::Graph;
use jstream_protocol::{
    Canvas, Edit, ItemGeometry, ItemId, KindDescriptor, PreviewDraft, ProtocolError, SceneId,
    SourceId, SourceKind,
};
use jstream_sources::{Context, Registry};

use crate::filter_assets::FilterAssets;
use crate::runtime::RenderSnapshot;
use crate::snapshot::{self, CaptureKey, Look};

/// The draft's source, scene and item IDs in its own collection.
const SOURCE: &str = "preview-draft";
const SCENE: &str = "preview-draft-scene";
const ITEM: &str = "preview-draft-item";

/// Plans `draft` on `canvas` with the engine's `kinds`, as the snapshot its stream draws.
/// The engine reads the kinds and the canvas under its lock and calls this after.
///
/// # Errors
/// `invalid_argument` naming `source.kind` for a kind the engine doesn't have or that
/// captures nothing, and `source.settings.<param>` (or `source.settings`) for settings
/// the kind refuses.
pub(crate) fn plan(
    kinds: Vec<KindDescriptor>,
    canvas: Canvas,
    draft: &PreviewDraft,
) -> Result<Arc<RenderSnapshot>, ProtocolError> {
    let not_capture = || {
        ProtocolError::invalid_argument(
            "source.kind",
            format!(
                "`{}` is not a capture kind: only display, window and webcam sources \
                 can be previewed before they are added",
                draft.kind
            ),
        )
    };
    if !kinds.iter().any(|kind| kind.kind_id == draft.kind) {
        return Err(ProtocolError::invalid_argument(
            "source.kind",
            format!("this engine has no source kind `{}`", draft.kind),
        ));
    }
    let mut own = Graph::new().with_kinds(kinds);
    let source_id = SourceId::new(SOURCE);
    let scene_id = SceneId::new(SCENE);
    let geometry = ItemGeometry::covering_canvas(canvas.width, canvas.height).map_err(|err| {
        ProtocolError::Internal {
            reason: format!("the canvas has no size: {err}"),
        }
    })?;
    let edits = [
        Edit::SourceCreate {
            source_id: source_id.clone(),
            name: "Preview".into(),
            kind: SourceKind::Builtin {
                kind: draft.kind.clone(),
            },
            settings: draft.settings.clone(),
            duplicated_from: None,
        },
        Edit::CanvasSetSize {
            width: canvas.width,
            height: canvas.height,
        },
        Edit::SceneCreate {
            scene_id: scene_id.clone(),
            name: "Preview".into(),
            role: None,
        },
        Edit::ItemAdd {
            scene_id: scene_id.clone(),
            item_id: ItemId::new(ITEM),
            source_id: source_id.clone(),
            transform: None,
            geometry: Some(geometry),
            opacity: None,
            blend: None,
        },
    ];
    own.apply(&edits, None).map_err(draft_error)?;
    let settings = own
        .state()
        .source(&source_id)
        .map(|source| source.settings().clone())
        .ok_or_else(|| ProtocolError::Internal {
            reason: "the draft source was not created".into(),
        })?;
    // A capture source's instance only says what it captures; creating one opens nothing.
    let instance = Registry::builtin()
        .create(&draft.kind, &settings, &Context::default())
        .map_err(|_| not_capture())?;
    let spec = instance.capture().cloned().ok_or_else(not_capture)?;
    let key = (spec != jstream_sources::CaptureSpec::Window(None)).then(|| {
        CaptureKey::for_source(spec, source_id.clone()).with_cursor(instance.show_cursor())
    });
    let planning = snapshot::plan_scene(
        &own,
        &scene_id,
        &mut |id| {
            if *id == source_id {
                key.clone().map(Look::Capture)
            } else {
                None
            }
        },
        &FilterAssets::default(),
    );
    Ok(RenderSnapshot::following(None, planning.shot))
}

/// The draft collection's error for `preview.subscribe`: the source's own edit is the
/// first, so its fields are the draft's (`edits.0.settings.fps` is `source.settings.fps`).
fn draft_error(error: ProtocolError) -> ProtocolError {
    match error {
        ProtocolError::InvalidArgument {
            field,
            reason,
            blockers,
        } => {
            let field = match field.strip_prefix("edits.0.") {
                Some(rest) if rest.starts_with("kind") => "source.kind".to_owned(),
                Some(rest) => format!("source.{rest}"),
                None => "source".to_owned(),
            };
            ProtocolError::InvalidArgument {
                field,
                reason,
                blockers,
            }
        }
        ProtocolError::NotFound { .. } => ProtocolError::invalid_argument(
            "source.kind",
            "this source kind can't be previewed before it is added",
        ),
        other => other,
    }
}
