//! The activity log over the protocol (ADR-0083): `activity.list` through
//! `InProcessControl`, with the sentences the engine writes.

use super::{add, batch, block_on, code, connected, create, query, switch_call};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    ActivityEntry, Batch, Call, Edit, Finite32, Hello, PROTOCOL_VERSION, Query, QueryResult,
    SceneId, Settings, SourceId, SourceKind,
};

fn activity(
    control: &dyn Control,
    after_seq: Option<u64>,
    limit: Option<u32>,
) -> Vec<ActivityEntry> {
    match query(control, Query::ActivityList { after_seq, limit }).expect("activity.list") {
        QueryResult::ActivityList { now_ms, entries } => {
            assert!(entries.iter().all(|entry| entry.time_ms <= now_ms));
            entries
        }
        other => panic!("not an activity list: {other:?}"),
    }
}

fn mic() -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new("mic"),
        name: "Mic".into(),
        kind: SourceKind::Builtin {
            kind: "audio_input".into(),
        },
        settings: Settings::default(),
        duplicated_from: None,
    }
}

fn gain(db: f32) -> Edit {
    Edit::SourceSetAudio {
        source_id: SourceId::new("mic"),
        enabled: None,
        follow: None,
        gain_db: Finite32::new(db),
        muted: None,
    }
}

#[test]
fn every_call_is_listed_newest_first_with_its_sentence() {
    let engine = Engine::new();
    let agent = connected(&engine);
    block_on(agent.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "claude-desktop".into(),
    }))
    .expect("hello again");
    assert!(activity(&agent, None, None).is_empty());

    batch(&agent, Batch::single(create("cam"))).expect("scene");
    batch(&agent, Batch::single(mic())).expect("mic");
    batch(&agent, Batch::single(gain(-1.5))).expect("gain");
    batch(&agent, Batch::single(gain(-3.0))).expect("gain");
    block_on(agent.call(switch_call("cam"))).expect("switch");
    let error = block_on(agent.call(switch_call("gone"))).expect_err("no such scene");
    batch(
        &agent,
        Batch {
            edits: vec![create("two"), add("two", "two-mic", "mic")],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("two edits");

    let entries = activity(&agent, None, None);
    let summaries: Vec<&str> = entries.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(
        summaries,
        [
            "Created scene “TWO” and 1 more change",
            "Switched to “gone”",
            "Switched to “CAM”",
            "Mic gain −1.5 → −3.0 dB",
            "Mic gain 0.0 → −1.5 dB",
            "Created source “Mic”",
            "Created scene “CAM”",
        ]
    );
    let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [7, 6, 5, 4, 3, 2, 1]);
    assert!(entries.iter().all(|e| e.client_name == "claude-desktop"));
    assert_eq!(entries[1].error.as_deref(), code(&error));
    assert_eq!(entries[1].call, "scene.switch");
    assert_eq!(entries[0].call, "batch");
    assert!(entries[0].error.is_none());

    let newer: Vec<u64> = activity(&agent, Some(5), None)
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(newer, [7, 6]);
    assert_eq!(activity(&agent, None, Some(1)).len(), 1);
}

#[test]
fn a_deleted_object_keeps_its_name_and_the_delete_leads() {
    let engine = Engine::new();
    let agent = connected(&engine);
    batch(&agent, Batch::single(create("intro"))).expect("scene");
    let rename = Edit::SceneRename {
        scene_id: SceneId::new("intro"),
        name: "Opening".into(),
    };
    batch(&agent, Batch::single(rename)).expect("rename");
    batch(
        &agent,
        Batch {
            edits: vec![
                create("spare"),
                Edit::SceneDelete {
                    scene_id: SceneId::new("intro"),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("delete");
    let summaries: Vec<String> = activity(&agent, None, None)
        .into_iter()
        .map(|e| e.summary)
        .collect();
    assert_eq!(
        summaries,
        [
            "Deleted scene “Opening” and 1 more change",
            "Renamed scene “INTRO” to “Opening”",
            "Created scene “INTRO”",
        ]
    );
}

#[test]
fn queries_are_not_activity() {
    let engine = Engine::new();
    let agent = connected(&engine);
    query(&agent, Query::SceneList).expect("scene.list");
    let _ = block_on(agent.call(Call::Query {
        query: Query::StateGet,
    }));
    assert!(activity(&agent, None, None).is_empty());
}
