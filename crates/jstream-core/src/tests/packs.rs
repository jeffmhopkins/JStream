//! Effect packs written over the protocol (ADR-0097): `pack.write`, `pack.list`,
//! `pack.read` and `pack.delete` through `InProcessControl`, the kinds they change, and
//! the activity log's record of each write and delete.

use super::{block_on, connected, query};
use crate::Engine;
use crate::effect_packs::test_pack::{BLUE, KIND, MANIFEST, MUL, Temp};
use jstream_control::Control;
use jstream_protocol::{
    Action, ActionResult, Call, CallResult, PackContent, PackFile, Query, QueryResult,
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
