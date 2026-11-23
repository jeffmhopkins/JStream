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

/// The probes' resolution cap (ADR-0170 decision 3) is one of the face sizes: another is
/// refused and changes nothing.
#[test]
fn a_probe_resolution_cap_is_one_of_the_face_sizes() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    set(
        &control,
        RenderFeaturesChange {
            max_probe_resolution: Some(256),
            ..RenderFeaturesChange::default()
        },
    );
    let capped = RenderFeatures {
        max_probe_resolution: 256,
        ..RenderFeatures::ALL
    };
    assert_eq!(rendering(&control), Some(capped));
    let refused = block_on(control.call(Call::Action {
        action: Action::StudioSetRendering {
            change: RenderFeaturesChange {
                max_probe_resolution: Some(300),
                reflection_probes: Some(false),
                ..RenderFeaturesChange::default()
            },
        },
    }));
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("max_probe_resolution")),
        "{refused:?}"
    );
    assert_eq!(rendering(&control), Some(capped), "nothing changed");
}

/// The steps a screen-space reflection's ray takes (ADR-0170 decision 4) are one of
/// the counts: another is refused and changes nothing.
#[test]
fn the_reflection_steps_are_one_of_the_counts() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    set(
        &control,
        RenderFeaturesChange {
            screen_space_steps: Some(64),
            screen_space_reflections: Some(false),
            ..RenderFeaturesChange::default()
        },
    );
    let set_to = RenderFeatures {
        screen_space_steps: 64,
        screen_space_reflections: false,
        ..RenderFeatures::ALL
    };
    assert_eq!(rendering(&control), Some(set_to));
    let refused = block_on(control.call(Call::Action {
        action: Action::StudioSetRendering {
            change: RenderFeaturesChange {
                screen_space_steps: Some(20),
                screen_space_reflections: Some(true),
                ..RenderFeaturesChange::default()
            },
        },
    }));
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("screen_space_steps")),
        "{refused:?}"
    );
    assert_eq!(rendering(&control), Some(set_to), "nothing changed");
}
