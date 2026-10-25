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
            },
            SourceSize {
                source_id: SourceId::new("spare"),
                size: size(8, 6),
            },
            SourceSize {
                source_id: SourceId::new("webcam"),
                size: None,
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
