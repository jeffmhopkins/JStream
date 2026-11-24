use super::*;
use jstream_protocol::{BusId, Edit, Finite32, MuteTo};

fn bound(trigger: Trigger, action: Action) -> Graph {
    let mut graph = Graph::new();
    graph
        .apply(&[Edit::BindingSet { trigger, action }], None)
        .expect("binds");
    graph
}

fn data(key: &str, on: DataOn, field: &str) -> Trigger {
    Trigger::Data {
        input: None,
        key: key.into(),
        field: field.into(),
        on,
    }
}

/// A toggle of the master's mute, which has no release; `_what` names it for the
/// reader.
fn switch(_what: &str) -> Action {
    Action::BusMute {
        bus_id: BusId::new("master"),
        to: MuteTo::Toggle,
    }
}

fn put(live: &LiveData, key: &str, json: &str) {
    live.put("Bitwig", key, json.as_bytes(), 0).expect("taken");
}

#[test]
fn a_selected_scene_fires_once_when_it_comes_and_not_from_where_it_starts() {
    let graph = bound(
        data(
            "grid",
            DataOn::Equals { value: "3".into() },
            "selectedScene",
        ),
        switch("scn_chorus"),
    );
    let live = LiveData::new();
    let mut runner = DataTriggers::default();
    let t0 = Instant::now();
    put(&live, "grid", r#"{"selectedScene": 3}"#);
    assert!(runner.tick(&graph, &live, t0).is_empty(), "where it starts");
    put(&live, "grid", r#"{"selectedScene": 1}"#);
    assert!(runner.tick(&graph, &live, t0 + MIN_GAP).is_empty());
    put(&live, "grid", r#"{"selectedScene": 3}"#);
    assert_eq!(
        runner.tick(&graph, &live, t0 + MIN_GAP * 2),
        vec![switch("scn_chorus")]
    );
    assert!(
        runner.tick(&graph, &live, t0 + MIN_GAP * 3).is_empty(),
        "still 3: nothing more"
    );
}

#[test]
fn a_launched_scene_on_any_track_fires_and_a_new_track_starts_where_it_is() {
    let graph = bound(
        data("track/*", DataOn::Equals { value: "2".into() }, "playing"),
        switch("scn_verse"),
    );
    let live = LiveData::new();
    let mut runner = DataTriggers::default();
    let t0 = Instant::now();
    put(&live, "track/1", r#"{"playing": -1}"#);
    assert!(runner.tick(&graph, &live, t0).is_empty());
    // A track that arrives already playing scene 2 is where it starts.
    put(&live, "track/2", r#"{"playing": 2}"#);
    assert!(runner.tick(&graph, &live, t0 + MIN_GAP).is_empty());
    put(&live, "track/2", r#"{"playing": -1}"#);
    assert!(runner.tick(&graph, &live, t0 + MIN_GAP * 2).is_empty());
    put(&live, "track/1", r#"{"playing": 2}"#);
    put(&live, "track/2", r#"{"playing": 2}"#);
    assert_eq!(
        runner.tick(&graph, &live, t0 + MIN_GAP * 3),
        vec![switch("scn_verse")],
        "two tracks launching scene 2 fire once"
    );
    assert!(matches("track/*", "track/12"));
    assert!(!matches("track/*", "track/1/clip"));
    assert!(!matches("track/*", "params"));
}

#[test]
fn a_mod_crossing_a_level_presses_and_releases_at_most_once_a_gap() {
    let level = Finite32::new(0.5).expect("finite");
    let graph = bound(
        data("params", DataOn::Rises { level }, "values.19"),
        Action::BusMute {
            bus_id: BusId::new("master"),
            to: MuteTo::PushToTalk,
        },
    );
    let live = LiveData::new();
    let mut runner = DataTriggers::default();
    let t0 = Instant::now();
    let values = |v: f32| {
        let mut all = vec![0.0f32; 32];
        all[19] = v;
        format!(r#"{{"values": {all:?}}}"#)
    };
    put(&live, "params", &values(0.1));
    assert!(runner.tick(&graph, &live, t0).is_empty());
    put(&live, "params", &values(0.7));
    assert_eq!(
        runner.tick(&graph, &live, t0 + MIN_GAP),
        vec![Action::BusMute {
            bus_id: BusId::new("master"),
            to: MuteTo::PushToTalk,
        }]
    );
    put(&live, "params", &values(0.2));
    assert_eq!(
        runner.tick(&graph, &live, t0 + MIN_GAP + Duration::from_millis(10)),
        vec![Action::BusMute {
            bus_id: BusId::new("master"),
            to: MuteTo::Muted,
        }],
        "falling back releases"
    );
    // Up again within the gap: no second press.
    put(&live, "params", &values(0.9));
    assert!(
        runner
            .tick(&graph, &live, t0 + MIN_GAP + Duration::from_millis(20))
            .is_empty()
    );
}

#[test]
fn changes_fires_on_each_new_value_and_equals_reads_numbers_texts_and_switches() {
    let graph = bound(
        data("transport", DataOn::Changes, "tempoText"),
        switch("scn_tempo"),
    );
    let live = LiveData::new();
    let mut runner = DataTriggers::default();
    let t0 = Instant::now();
    put(&live, "transport", r#"{"tempoText": "120.00"}"#);
    assert!(runner.tick(&graph, &live, t0).is_empty());
    put(&live, "transport", r#"{"tempoText": "121.00"}"#);
    assert_eq!(
        runner.tick(&graph, &live, t0 + MIN_GAP),
        vec![switch("scn_tempo")]
    );
    assert!(equals(&serde_json::json!(3), "3.0"));
    assert!(equals(&serde_json::json!("playing"), "playing"));
    assert!(equals(&serde_json::json!(true), "true"));
    assert!(!equals(&serde_json::json!(null), "null"));
}
