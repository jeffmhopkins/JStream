//! Effect packs written over the protocol (ADR-0097): `pack.write`, `pack.list`,
//! `pack.read` and `pack.delete` through `InProcessControl`, the kinds they change, and
//! the activity log's record of each write and delete.

use super::{block_on, connected, query};
use crate::Engine;
use crate::effect_packs::test_pack::{BLUE, KIND, MANIFEST, MUL, Temp};
use jstream_control::Control;
use jstream_protocol::{
    Action, ActionResult, Batch, Call, CallResult, Edit, FilterChain, FilterId, FilterOwner,
    HistoryCall, HistoryOp, PackContent, PackFile, Query, QueryResult, Settings, SourceId,
    SourceKind,
};

fn write(control: &dyn Control, shader: &str) -> Result<CallResult, jstream_control::ControlError> {
    let files = [("effect.toml", MANIFEST), ("mul.wgsl", shader)]
        .into_iter()
        .map(|(path, text)| PackFile {
            path: path.into(),
            content: PackContent::Text(text.into()),
        })
        .collect();
    let call = Call::Action {
        action: Action::PackWrite {
            pack: "tint".into(),
            files,
        },
    };
    block_on(control.call(call)).map(|answer| answer.result)
}

fn kinds(control: &dyn Control) -> Vec<String> {
    let Ok(QueryResult::KindList { kinds }) = query(control, Query::KindList) else {
        panic!("kind.list");
    };
    kinds.into_iter().map(|k| k.kind_id).collect()
}

#[test]
fn an_agent_writes_reads_and_deletes_a_pack() {
    let temp = Temp::new("protocol");
    let engine = Engine::new();
    engine.watch_effect_packs(&temp.0);
    let control = connected(&engine);
    assert!(!kinds(&control).iter().any(|k| k == KIND));

    let Ok(CallResult::Action {
        result: Some(ActionResult::PackWrite { pack }),
    }) = write(&control, MUL)
    else {
        panic!("a pack.write result");
    };
    assert_eq!(
        (pack.kinds.as_slice(), pack.written_by.as_deref()),
        ([KIND.to_owned()].as_slice(), Some("test"))
    );
    // Loaded at once: no poll needed.
    assert!(kinds(&control).iter().any(|k| k == KIND));
    let Ok(QueryResult::PackList { packs }) = query(&control, Query::PackList) else {
        panic!("pack.list");
    };
    assert_eq!(packs, [pack]);

    let refused = write(&control, "@fragment fn fs(").expect_err("doesn't compile");
    assert!(refused.to_string().contains("mul.wgsl"), "{refused}");
    write(&control, BLUE).expect("rewritten");
    let Ok(QueryResult::PackRead { files }) = query(
        &control,
        Query::PackRead {
            pack: "tint".into(),
            path: Some("mul.wgsl".into()),
        },
    ) else {
        panic!("pack.read");
    };
    assert_eq!(files[0].text.as_deref(), Some(BLUE));

    let delete = Call::Action {
        action: Action::PackDelete {
            pack: "tint".into(),
        },
    };
    block_on(control.call(delete)).expect("pack.delete");
    assert!(!kinds(&control).iter().any(|k| k == KIND));

    let Ok(QueryResult::ActivityList { entries, .. }) = query(
        &control,
        Query::ActivityList {
            after_seq: None,
            limit: None,
        },
    ) else {
        panic!("activity.list");
    };
    let summaries: Vec<_> = entries.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(
        summaries,
        [
            "Deleted effect pack “tint”",
            "Wrote effect pack “tint” (2 files)",
            "Wrote effect pack “tint” (2 files)",
            "Wrote effect pack “tint” (2 files)",
        ]
    );
    assert_eq!(entries[2].error.as_deref(), Some("invalid_argument"));
}

/// The `tint` pack at version 2: its `tint` setting renamed `colour`.
const RENAMED: &str = r##"
[pack]
id = "tint"
name = "Tint"

[[effect]]
id = "mul"
name = "Multiply"
version = 2
shader = "mul.wgsl"
renames = { tint = "colour" }

[[effect.param]]
id = "colour"
label = "Colour"
type = "color"
default = "#00ff00"
"##;

/// [`MUL`] reading the renamed setting.
const MUL_RENAMED: &str = "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n    let c = src(in.uv);\n    return vec4<f32>(c.rgb * p.colour.rgb, c.a);\n}\n";

fn write_renamed(control: &dyn Control) {
    let files = [("effect.toml", RENAMED), ("mul.wgsl", MUL_RENAMED)]
        .into_iter()
        .map(|(path, text)| PackFile {
            path: path.into(),
            content: PackContent::Text(text.into()),
        })
        .collect();
    let call = Call::Action {
        action: Action::PackWrite {
            pack: "tint".into(),
            files,
        },
    };
    block_on(control.call(call)).expect("the renamed pack is written");
}

/// A colour source `bg` with a `tint` filter of [`KIND`] set to red.
fn tinted(control: &dyn Control) {
    super::batch(
        control,
        Batch {
            edits: vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("bg"),
                    name: "bg".into(),
                    kind: SourceKind::Builtin {
                        kind: "color".into(),
                    },
                    settings: Settings::default(),
                    duplicated_from: None,
                },
                Edit::FilterAdd {
                    owner: FilterOwner::Source {
                        source_id: SourceId::new("bg"),
                    },
                    chain: FilterChain::Video,
                    filter_id: FilterId::new("tint"),
                    kind: KIND.into(),
                    settings: [("tint", serde_json::json!("#ff0000"))]
                        .into_iter()
                        .collect(),
                    enabled: None,
                    index: None,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("a pack filter");
}

/// The `tint` filter on `bg` as the collection file at `path` holds it.
fn saved_filter(path: &std::path::Path) -> serde_json::Value {
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    saved["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .find(|s| s["id"] == "bg")
        .expect("bg")["filters"]["video"][0]
        .clone()
}

/// A pack reload that renames a setting moves each live filter's value to the new ID
/// (#1353, ADR-0025 › Versioning a kind), saves it, and an undo from before it no longer
/// writes the old ID.
#[test]
fn a_reload_moves_live_filters_through_the_renames() {
    let temp = Temp::new("renames-live");
    let path = temp.0.join("scenes.json");
    let engine = Engine::new()
        .with_collection_file(&path)
        .expect("collection file");
    engine.watch_effect_packs(temp.0.join("effects"));
    let control = connected(&engine);
    write(&control, MUL).expect("written");
    tinted(&control);
    assert_eq!(saved_filter(&path)["kind_version"], 1);
    let before = engine.rev();

    write_renamed(&control);
    assert_eq!(engine.rev(), before.next().expect("rev"), "one revision");
    let filter = saved_filter(&path);
    assert_eq!(filter["kind_version"], 2);
    assert_eq!(
        filter["settings"],
        serde_json::json!({ "colour": "#ff0000" })
    );

    // The batch that added the filter can't come back over the upgrade.
    let undo = block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo))));
    assert_eq!(
        undo.err().as_ref().and_then(super::code),
        Some("undo_conflict")
    );
    assert_eq!(
        saved_filter(&path)["settings"],
        serde_json::json!({ "colour": "#ff0000" })
    );
}

/// A filter loaded from the collection file before its pack keeps its settings as the
/// file has them, and they follow the renames from the recorded version once the pack
/// loads (#1353).
#[test]
fn a_loaded_filter_follows_the_renames_once_its_pack_loads() {
    let temp = Temp::new("renames-load");
    let path = temp.0.join("scenes.json");
    {
        let engine = Engine::new()
            .with_collection_file(&path)
            .expect("collection file");
        engine.watch_effect_packs(temp.0.join("effects"));
        let control = connected(&engine);
        write(&control, MUL).expect("written");
        tinted(&control);
    }
    // The pack moves on while the collection is closed.
    let pack = temp.0.join("effects").join("tint");
    std::fs::write(pack.join("effect.toml"), RENAMED).expect("manifest");
    std::fs::write(pack.join("mul.wgsl"), MUL_RENAMED).expect("shader");

    let engine = Engine::new()
        .with_collection_file(&path)
        .expect("the collection loads with its pack missing");
    let loaded = engine.rev();
    engine.watch_effect_packs(temp.0.join("effects"));
    assert_eq!(engine.rev(), loaded.next().expect("rev"), "one revision");
    let filter = saved_filter(&path);
    assert_eq!(filter["kind_version"], 2);
    assert_eq!(
        filter["settings"],
        serde_json::json!({ "colour": "#ff0000" })
    );
}

/// A `wave` pack with one shader source, `wave/w`, at `version`, whose one float
/// setting is called `setting`; `renames` is its manifest's `renames` line, if any.
fn wave_pack(dir: &std::path::Path, version: u32, setting: &str, renames: &str) {
    std::fs::create_dir_all(dir).expect("pack dir");
    let manifest = format!(
        "[pack]\nid = \"wave\"\nname = \"Wave\"\n\n[[source]]\nid = \"w\"\nname = \"Wave\"\n\
         version = {version}\nwidth = 16\nheight = 16\n{renames}\n\n[[source.param]]\n\
         id = \"{setting}\"\nlabel = \"Level\"\ntype = \"float\"\ndefault = 1.0\n\n\
         [[source.pass]]\nid = \"w\"\nshader = \"w.wgsl\"\ninputs = []\n"
    );
    let shader = format!(
        "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {{\n    return vec4<f32>(vec3<f32>(p.{setting}), 1.0);\n}}\n"
    );
    std::fs::write(dir.join("effect.toml"), manifest).expect("manifest");
    std::fs::write(dir.join("w.wgsl"), shader).expect("shader");
}

/// The source `s` as the collection file at `path` holds it.
fn saved_source(path: &std::path::Path) -> serde_json::Value {
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    saved["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .find(|s| s["id"] == "s")
        .expect("s")
        .clone()
}

/// A collection holding a shader source loads while the source's pack isn't loaded
/// yet, keeping the source as the file has it; once the pack loads, its settings follow
/// the renames from the version the file recorded, and a later reload's renames move
/// them again (#1412).
#[test]
fn a_shader_source_loads_before_its_pack_and_follows_its_renames() {
    let temp = Temp::new("renames-source");
    let path = temp.0.join("scenes.json");
    let effects = temp.0.join("effects");
    let pack = effects.join("wave");
    wave_pack(&pack, 1, "level", "");
    {
        let engine = Engine::new()
            .with_collection_file(&path)
            .expect("collection file");
        engine.watch_effect_packs(&effects);
        let control = connected(&engine);
        super::batch(
            &control,
            Batch {
                edits: vec![Edit::SourceCreate {
                    source_id: SourceId::new("s"),
                    name: "s".into(),
                    kind: SourceKind::Builtin {
                        kind: "wave/w".into(),
                    },
                    settings: [("level", serde_json::json!(0.25))].into_iter().collect(),
                    duplicated_from: None,
                }],
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: None,
            },
        )
        .expect("a shader source");
    }
    assert_eq!(saved_source(&path)["kind_version"], 1);
    wave_pack(&pack, 2, "gain", "renames = { level = \"gain\" }");

    // The daemon and the desktop app open the collection before they watch the packs.
    let engine = Engine::new()
        .with_collection_file(&path)
        .expect("the collection loads with its pack missing");
    let loaded = engine.rev();
    engine.watch_effect_packs(&effects);
    assert_eq!(engine.rev(), loaded.next().expect("rev"), "one revision");
    let source = saved_source(&path);
    assert_eq!(source["kind_version"], 2);
    assert_eq!(source["settings"]["gain"], 0.25);
    assert!(source["settings"].get("level").is_none(), "{source}");

    // A reload that renames it again moves it again.
    let control = connected(&engine);
    let files = [
        (
            "effect.toml",
            std::fs::read_to_string(pack.join("effect.toml")).expect("read"),
        ),
        (
            "w.wgsl",
            std::fs::read_to_string(pack.join("w.wgsl")).expect("read"),
        ),
    ]
    .into_iter()
    .map(|(path, text)| PackFile {
        path: path.into(),
        content: PackContent::Text(
            text.replace("version = 2", "version = 3")
                .replace(
                    "renames = { level = \"gain\" }",
                    "renames = { level = \"amount\", gain = \"amount\" }",
                )
                .replace("id = \"gain\"", "id = \"amount\"")
                .replace("p.gain", "p.amount"),
        ),
    })
    .collect();
    block_on(control.call(Call::Action {
        action: Action::PackWrite {
            pack: "wave".into(),
            files,
        },
    }))
    .expect("the pack is rewritten");
    let source = saved_source(&path);
    assert_eq!(source["kind_version"], 3);
    assert_eq!(source["settings"]["amount"], 0.25);
    assert!(source["settings"].get("gain").is_none(), "{source}");
}
