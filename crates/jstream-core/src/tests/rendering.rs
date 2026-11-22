//! The 3D rendering switches (ADR-0170 decision 1): `studio.set_rendering` changes the
//! switches it names, `studio.get` reports them, and the host's file keeps them.

use super::{block_on, connected};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, CallResult, Query, QueryResult, RenderFeatures, RenderFeaturesChange,
};

fn rendering(control: &dyn Control) -> Option<RenderFeatures> {
    let response = block_on(control.call(Call::Query {
        query: Query::StudioGet,
    }))
    .expect("studio.get answers");
    match response.result {
        CallResult::Query {
            result: QueryResult::StudioGet { rendering, .. },
        } => rendering,
        other => panic!("studio.get answered {other:?}"),
    }
}

fn set(control: &dyn Control, change: RenderFeaturesChange) {
    block_on(control.call(Call::Action {
        action: Action::StudioSetRendering { change },
    }))
    .expect("studio.set_rendering applies");
}

#[test]
fn the_switches_are_set_one_at_a_time_and_kept_in_their_file() {
    let dir = std::env::temp_dir().join(format!("jstream-rendering-engine-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let file = dir.join("rendering.json");
    let _ = std::fs::remove_file(&file);

    let engine = Engine::new()
        .with_cpu_compositor()
        .with_rendering_file(&file);
    let control = connected(&engine);
    assert_eq!(rendering(&control), Some(RenderFeatures::ALL), "all on");
    set(
        &control,
        RenderFeaturesChange {
            shadows: Some(false),
            ..RenderFeaturesChange::default()
        },
    );
    set(
        &control,
        RenderFeaturesChange {
            floor_reflections: Some(false),
            ..RenderFeaturesChange::default()
        },
    );
    let off = RenderFeatures {
        shadows: false,
        floor_reflections: false,
        ..RenderFeatures::ALL
    };
    assert_eq!(rendering(&control), Some(off), "each change keeps the rest");
    drop(control);
    drop(engine);

    let engine = Engine::new()
        .with_cpu_compositor()
        .with_rendering_file(&file);
    let control = connected(&engine);
    assert_eq!(rendering(&control), Some(off), "the file kept them");
    drop(control);
    drop(engine);
    std::fs::remove_dir_all(&dir).expect("cleaned up");
}
