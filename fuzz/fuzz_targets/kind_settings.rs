//! Kind settings validation: arbitrary settings for every built-in kind.
//!
//! The first byte picks a kind; the rest is a JSON object of settings. The graph checks
//! them on `source.create` (ADR-0025) and must not panic. Settings it accepts must then
//! be accepted by `source.set_settings` too, undo exactly, and create a source that
//! reports bad values as errors rather than panicking: `Source::create` may assume the
//! settings are well typed, and the graph's check is what guarantees it.

#![no_main]

use jstream_graph::Graph;
use jstream_media::{MediaTime, Timebase};
use jstream_protocol::{Edit, Settings, SourceId, SourceKind};
use jstream_sources::{Context, Registry};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(rest) else {
        return;
    };
    let registry = Registry::builtin();
    let kinds = registry.descriptors();
    let kind = &kinds[usize::from(pick) % kinds.len()];

    // Each parameter's own check, alone.
    for (id, value) in &object {
        if let Some(param) = kind.param(id) {
            let _ = param.check(value);
        }
    }
    let _ = kind.settings_schema();

    let settings: Settings = object.into_iter().collect();
    let mut graph = Graph::new().with_kinds(kinds.iter().cloned());
    let source_id = SourceId::new("src");
    let created = graph.apply(
        &[Edit::SourceCreate {
            source_id: source_id.clone(),
            name: "fuzz".into(),
            kind: SourceKind::Builtin {
                kind: kind.kind_id.clone(),
            },
            settings: settings.clone(),
            duplicated_from: None,
        }],
        None,
    );
    if created.is_err() {
        return;
    }

    // The same values as a patch, then its undo.
    let before = graph.clone();
    let patched = graph
        .apply(
            &[Edit::SourceSetSettings {
                source_id: source_id.clone(),
                patch: settings,
            }],
            None,
        )
        .expect("settings accepted on create are accepted as a patch");
    graph
        .apply(&patched.inverse.edits, None)
        .expect("the patch's inverse applies");
    assert_eq!(graph.state(), before.state(), "undoing the patch");

    // The stored map is complete and checked: create the source from it. An asset
    // reference would read a file, so those are left to the kinds' own tests.
    let stored = graph
        .state()
        .source(&source_id)
        .expect("the source exists")
        .settings()
        .clone();
    if stored.references().next().is_some() {
        return;
    }
    if let Ok(mut source) = registry.create(&kind.kind_id, &stored, &Context::default()) {
        let _ = source.frame(MediaTime::new(0, Timebase::MILLIS));
        let _ = source.update(&stored);
    }
});
