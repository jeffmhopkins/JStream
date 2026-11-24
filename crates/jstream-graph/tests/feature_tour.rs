//! The feature-tour demo project (`examples/feature-tour/feature-tour.json`) loads with the
//! engine's real kind descriptors, is saved exactly as committed (so it is in the
//! canonical form the engine writes), and every show in it names only things the
//! project holds: a show keeps a clip whose target is gone and does nothing with it, so
//! a typo would load silently and leave a hole in the demo.
//!
//! Regenerate the committed file in canonical form after editing it with
//! `FEATURE_TOUR_WRITE=1 cargo test -p jstream-graph --test feature_tour`.

use jstream_graph::Graph;
use jstream_protocol::KindDescriptor;
use serde_json::Value;
use std::collections::BTreeSet;

const TOUR: &str = "../../examples/feature-tour/feature-tour.json";

fn kinds() -> Vec<KindDescriptor> {
    let snap = include_str!("../../jstream-core/schema/kinds.snap");
    let start = snap
        .find('[')
        .expect("the kinds snapshot holds a JSON list");
    serde_json::from_str(&snap[start..]).expect("the kinds snapshot parses as descriptors")
}

fn load(json: &str) -> Graph {
    Graph::new()
        .with_kinds(kinds())
        .with_collection_json(json)
        .unwrap_or_else(|e| panic!("the feature tour loads: {e}"))
}

fn ids<'a>(list: &'a Value, key: &str) -> BTreeSet<&'a str> {
    list.as_array()
        .map(|a| a.iter().filter_map(|v| v[key].as_str()).collect())
        .unwrap_or_default()
}

#[test]
fn the_feature_tour_loads_and_is_saved_as_committed() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(TOUR);
    let committed = std::fs::read_to_string(&path).expect("the feature tour is committed");
    let saved = load(&committed).to_collection_json();
    if std::env::var_os("FEATURE_TOUR_WRITE").is_some() {
        std::fs::write(&path, &saved).expect("the feature tour is writable");
        return;
    }
    assert!(
        saved == committed,
        "the feature tour is not in canonical form; regenerate it with \
         FEATURE_TOUR_WRITE=1 cargo test -p jstream-graph --test feature_tour"
    );
    // Loading what was saved changes nothing more.
    assert_eq!(load(&saved).to_collection_json(), saved);
}

#[test]
fn every_show_names_only_what_the_tour_holds() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(TOUR);
    let json = std::fs::read_to_string(path).expect("the feature tour is committed");
    let graph = load(&json);
    let file: Value = serde_json::from_str(&json).expect("parses");

    let scenes = &file["scenes"];
    let all_scenes = ids(scenes, "id");
    let keyframes: BTreeSet<&str> = scenes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s.get("variant_of").is_some())
        .filter_map(|s| s["id"].as_str())
        .collect();
    let items: BTreeSet<&str> = scenes
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["items"].as_array().into_iter().flatten())
        .filter_map(|i| i["id"].as_str())
        .collect();
    let sources = ids(&file["sources"], "id");
    let presets = ids(&file["transitions"]["presets"], "id");
    let channels = ids(&file["channels"], "channel_id");
    let all_timelines: Vec<&jstream_protocol::TimelineInfo> = graph
        .state()
        .scenes()
        .iter()
        .flat_map(|s| s.timelines().values())
        .collect();
    let timelines: BTreeSet<String> = all_timelines
        .iter()
        .map(|t| t.timeline_id.to_string())
        .collect();

    assert!(graph.shows().len() >= 2, "the tour holds two shows");
    for show in graph.shows() {
        let id = show.show_id.as_str();
        assert_eq!(show.problem(), None, "show {id}");
        let preset = |p: &Option<jstream_protocol::TransitionPresetId>, what: &str| {
            if let Some(p) = p {
                assert!(presets.contains(p.as_str()), "show {id}: {what} preset {p}");
            }
        };
        preset(&show.preset_id, "default");
        for clip in &show.program {
            let scene = clip.scene_id.as_str();
            assert!(
                all_scenes.contains(scene) && !keyframes.contains(scene),
                "show {id}: clip {}",
                clip.clip_id
            );
            preset(&clip.preset_id, &clip.clip_id);
        }
        for track in &show.overlays {
            for clip in &track.clips {
                assert!(
                    all_scenes.contains(clip.scene_id.as_str()),
                    "show {id}: overlay {}",
                    clip.clip_id
                );
            }
        }
        for marker in &show.keyframes {
            if let Some(k) = &marker.keyframe_id {
                assert!(
                    keyframes.contains(k.as_str()),
                    "show {id}: keyframe {}",
                    marker.marker_id
                );
            }
            preset(&marker.preset_id, &marker.marker_id);
        }
        for clip in show.audio.iter().flat_map(|t| &t.clips) {
            assert!(
                sources.contains(clip.source_id.as_str()),
                "show {id}: audio {}",
                clip.clip_id
            );
        }
        for bookmark in &show.bookmarks {
            preset(&bookmark.preset_id, &bookmark.bookmark_id);
        }
        for cue in show.sequences.iter().flat_map(|t| &t.cues) {
            let action = cue
                .action()
                .unwrap_or_else(|| panic!("show {id}: cue {} parses", cue.cue_id));
            assert_eq!(action.cue_problem(), None, "show {id}: cue {}", cue.cue_id);
            let named = serde_json::to_value(&action).expect("serializes");
            if let Some(t) = named["timeline_id"].as_str() {
                assert!(
                    timelines.contains(t),
                    "show {id}: cue {} plays {t}",
                    cue.cue_id
                );
            }
            if let Some(i) = named["item_id"].as_str() {
                assert!(
                    items.contains(i),
                    "show {id}: cue {} names item {i}",
                    cue.cue_id
                );
            }
            if let Some(c) = named["channel_id"].as_str() {
                assert!(
                    channels.contains(c),
                    "show {id}: cue {} names channel {c}",
                    cue.cue_id
                );
            }
        }
    }
    // Every sequence cue and every lane names something the tour holds too.
    for timeline in &all_timelines {
        for cue in &timeline.cues {
            let action: Value = cue.action.clone();
            if let Some(i) = action["item_id"].as_str() {
                assert!(
                    items.contains(i),
                    "{}: cue names item {i}",
                    timeline.timeline_id
                );
            }
        }
        for lane in &timeline.lanes {
            assert!(
                channels.contains(lane.target.channel.as_str()),
                "{}",
                timeline.timeline_id
            );
        }
    }
}
