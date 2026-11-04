//! Shared values (ADR-0102 decision 7): `values.set`, `values.list` and `values.changed`
//! through `InProcessControl`. Not collection state, so no `rev`, no undo step and no
//! activity-log entry.

use super::{block_on, code, connected, drain, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{Action, Call, Event, Query, QueryResult, SharedValue};
use serde_json::{Value, json};

fn set(
    control: &dyn Control,
    name: &str,
    value: Option<Value>,
) -> Result<(), Option<&'static str>> {
    let action = Action::ValuesSet {
        name: name.into(),
        value,
    };
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|error| code(&error))
}

fn list(control: &dyn Control, prefix: Option<&str>) -> Vec<SharedValue> {
    match query(
        control,
        Query::ValuesList {
            prefix: prefix.map(Into::into),
        },
    ) {
        Ok(QueryResult::ValuesList { values }) => values,
        other => panic!("not a values list: {other:?}"),
    }
}

fn changes(events: Vec<Event>) -> Vec<(String, Option<Value>)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            Event::ValuesChanged { name, value, .. } => Some((name, value)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_set_is_listed_and_announced_once() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let rev = engine.rev();

    set(&control, "status.text", Some(json!("On air"))).expect("set");
    set(&control, "counter.a", Some(json!({ "n": 2, "note": null }))).expect("set");
    // The same value again changes nothing and sends nothing.
    set(&control, "status.text", Some(json!("On air"))).expect("set again");
    assert_eq!(
        changes(drain(&mut events)),
        [
            ("status.text".to_owned(), Some(json!("On air"))),
            (
                "counter.a".to_owned(),
                Some(json!({ "n": 2, "note": null }))
            ),
        ]
    );
    assert_eq!(
        list(&control, None),
        [
            SharedValue {
                name: "counter.a".into(),
                value: json!({ "n": 2, "note": null }),
            },
            SharedValue {
                name: "status.text".into(),
                value: json!("On air"),
            },
        ]
    );
    assert_eq!(list(&control, Some("status")).len(), 1);
    // Not collection state: the revision doesn't move.
    assert_eq!(engine.rev(), rev);

    // A removal is announced with no value; removing it again sends nothing.
    set(&control, "status.text", None).expect("remove");
    set(&control, "status.text", None).expect("remove again");
    assert_eq!(
        changes(drain(&mut events)),
        [("status.text".to_owned(), None)]
    );
    assert_eq!(list(&control, Some("status")), []);
    // Nor an audited change: the activity log stays empty.
    match query(
        &control,
        Query::ActivityList {
            after_seq: None,
            limit: None,
        },
    ) {
        Ok(QueryResult::ActivityList { entries, .. }) => assert_eq!(entries, []),
        other => panic!("not an activity list: {other:?}"),
    }
}

#[test]
fn a_refused_set_changes_nothing() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    assert_eq!(
        set(&control, "Status", Some(json!(1))),
        Err(Some("invalid_argument"))
    );
    let big = json!("x".repeat(jstream_protocol::MAX_VALUE_BYTES));
    assert_eq!(
        set(&control, "status", Some(big)),
        Err(Some("invalid_argument"))
    );
    assert_eq!(changes(drain(&mut events)), []);
    assert_eq!(list(&control, None), []);
}
