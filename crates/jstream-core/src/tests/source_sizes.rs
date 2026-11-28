//! Source sizes as runtime state (B12, ADR-0022 §4): `source.sizes` reports each
//! source's current size, and `source.size_changed` announces every change, from a live
//! source's frames, a stored frame, and a deletion.

use super::{add, batch, connected, create, drain, edits, query, webcam};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Edit, Event, ItemId, PixelSize, Query, QueryResult, Settings, SourceId, SourceKind, SourceSize,
};
use serde_json::json;

fn color(id: &str, width: u32, height: u32) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: Settings(
            [
                ("color".to_owned(), json!("#ff8000ff")),
                ("width".to_owned(), json!(width)),
                ("height".to_owned(), json!(height)),
            ]
            .into_iter()
            .collect(),
        ),
        duplicated_from: None,
    }
}

fn size(width: u32, height: u32) -> Option<PixelSize> {
    Some(PixelSize { width, height })
}

fn changed(id: &str, size: Option<PixelSize>) -> Event {
    Event::SourceSizeChanged {
        source_id: SourceId::new(id),
        size,
        depth: None,
        duration_ms: None,
        objects: Vec::new(),
    }
}

/// Only the size events, in order.
fn size_events(events: Vec<Event>) -> Vec<Event> {
    events
        .into_iter()
        .filter(|event| matches!(event, Event::SourceSizeChanged { .. }))
        .collect()
}

fn sizes(control: &dyn Control) -> Vec<SourceSize> {
    match query(control, Query::SourceSizes).expect("source.sizes") {
        QueryResult::SourceSizes { sources } => sources,
        other => panic!("not source.sizes: {other:?}"),
    }
}

#[test]
fn sizes_follow_live_sources_stored_frames_and_deletes() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    batch(
        &control,
        edits(vec![
            create("a"),
            color("fill", 4, 2),
            // Not placed anywhere: a source the program doesn't show has a size too.
            color("spare", 8, 6),
            webcam(),
            add("a", "i1", "fill"),
            add("a", "i2", "webcam"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");

    // The webcam is a capture, whose frames reach only the render thread: no size yet.
    assert_eq!(
        sizes(&control),
        vec![
            SourceSize {
                source_id: SourceId::new("fill"),
                size: size(4, 2),
                depth: None,
                duration_ms: None,
                objects: Vec::new(),
            },
            SourceSize {
                source_id: SourceId::new("spare"),
                size: size(8, 6),
                depth: None,
                duration_ms: None,
                objects: Vec::new(),
            },
            SourceSize {
                source_id: SourceId::new("webcam"),
                size: None,
                depth: None,
                duration_ms: None,
                objects: Vec::new(),
            },
        ]
    );
    let mut announced = size_events(drain(&mut events));
    announced.sort_by_key(|event| format!("{event:?}"));
    assert_eq!(
        announced,
        vec![changed("fill", size(4, 2)), changed("spare", size(8, 6))]
    );

    // A rebuild at a new size is announced once; re-planning the same frame sends nothing.
    batch(
        &control,
        edits(vec![Edit::SourceSetSettings {
            source_id: SourceId::new("fill"),
            patch: Settings([("width".to_owned(), json!(16))].into_iter().collect()),
        }]),
    )
    .expect("resized");
    assert!(engine.settle_sources(), "rebuilt in time");
    engine.replan();
    assert_eq!(
        size_events(drain(&mut events)),
        vec![changed("fill", size(16, 2))]
    );

    // A stored frame gives the webcam its size, and wins over a live source's.
    engine
        .set_source_frame(&SourceId::new("webcam"), 2, 2, &[0; 16])
        .expect("frame");
    engine
        .set_source_frame(&SourceId::new("fill"), 3, 1, &[0; 12])
        .expect("frame");
    assert_eq!(
        size_events(drain(&mut events)),
        vec![changed("webcam", size(2, 2)), changed("fill", size(3, 1))]
    );

    // A deleted source loses its size, after the `state.changed` that deleted it.
    batch(
        &control,
        edits(vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            Edit::SourceDelete {
                source_id: SourceId::new("fill"),
            },
        ]),
    )
    .expect("deleted");
    let after = drain(&mut events);
    assert!(
        matches!(
            after.as_slice(),
            [Event::StateChanged { .. }, Event::SourceSizeChanged { .. }]
        ),
        "{after:?}"
    );
    assert_eq!(size_events(after), vec![changed("fill", None)]);
    assert_eq!(
        sizes(&control)
            .into_iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        vec![SourceId::new("spare"), SourceId::new("webcam")]
    );
}

/// A 3D model's size is its bounds at 512 px for the larger of width and height, and it
/// reports its depth in the same pixels (ADR-0107 decision 8); a flat source has none.
#[test]
fn a_model_reports_its_depth_beside_its_size() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let cube = Edit::SourceCreate {
        source_id: SourceId::new("cube"),
        name: "cube".into(),
        kind: SourceKind::Builtin {
            kind: "model".into(),
        },
        settings: Settings([("shape".to_owned(), json!("cube"))].into_iter().collect()),
        duplicated_from: None,
    };
    batch(&control, edits(vec![cube, color("fill", 4, 2)])).expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let mut reported = sizes(&control);
    reported.sort_by(|a, b| a.source_id.as_str().cmp(b.source_id.as_str()));
    assert_eq!(
        reported,
        vec![
            SourceSize {
                source_id: SourceId::new("cube"),
                size: size(512, 512),
                depth: Some(512),
                duration_ms: None,
                objects: Vec::new(),
            },
            SourceSize {
                source_id: SourceId::new("fill"),
                size: size(4, 2),
                depth: None,
                duration_ms: None,
                objects: Vec::new(),
            },
        ]
    );
    assert!(
        size_events(drain(&mut events)).contains(&Event::SourceSizeChanged {
            source_id: SourceId::new("cube"),
            size: size(512, 512),
            depth: Some(512),
            duration_ms: None,
            objects: Vec::new(),
        }),
        "the model's first size carries its depth"
    );
}

/// Protocol 0.198: a model's objects are listed as fractions of its own box, which the
/// size and depth beside them scale, so the canvas can place each one.
#[test]
fn a_models_objects_are_fractions_of_its_box() {
    use jstream_media::model::{Bounds, Object};
    let whole = Bounds {
        min: [-2.0, 0.0, -1.0],
        max: [2.0, 4.0, 1.0],
    };
    let objects: std::sync::Arc<[Object]> = vec![
        Object {
            name: "hull".into(),
            bounds: Bounds {
                min: [-2.0, 1.0, -1.0],
                max: [0.0, 4.0, 1.0],
            },
        },
        Object {
            name: "flat".into(),
            bounds: Bounds {
                min: [2.0, 2.0, 0.0],
                max: [2.0, 2.0, 0.0],
            },
        },
    ]
    .into();
    let listed = crate::engine::model_objects(&Some((whole, objects)));
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, "hull");
    assert_eq!(listed[0].min, [0.0, 0.25, 0.0]);
    assert_eq!(listed[0].max, [0.5, 1.0, 1.0]);
    assert_eq!(listed[1].min, [1.0, 0.5, 0.5]);
    assert!(crate::engine::model_objects(&None).is_empty());
}
