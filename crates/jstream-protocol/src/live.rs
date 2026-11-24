//! Live values (ADR-0032): what a gesture carries on the wire besides its actions.
//!
//! A gesture overrides parameters of the collection at runtime with `live.set` and ends
//! with a committing batch (`ends_gesture`), a `live.cancel`, or its client's session
//! ending. These are the types its query and events share, and its limits.

use crate::command::Edit;
use crate::id::{ClientId, GestureId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many gestures one client may hold open. Opening one more is `invalid_argument`.
pub const MAX_GESTURES_PER_CLIENT: usize = 16;

/// How many values one `live.set` may carry. More is `invalid_argument`.
pub const MAX_LIVE_VALUES: usize = 256;

/// How a gesture ended, in `live.ended`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LiveOutcome {
    /// A batch with `ends_gesture` applied; its `rev` is in the event.
    Committed,
    /// Its client sent `live.cancel`.
    Cancelled,
    /// Its client's session ended.
    Expired,
}

impl LiveOutcome {
    /// The outcome as it is written on the wire (`committed`).
    pub fn name(self) -> &'static str {
        match self {
            LiveOutcome::Committed => "committed",
            LiveOutcome::Cancelled => "cancelled",
            LiveOutcome::Expired => "expired",
        }
    }
}

/// One open gesture, in a `live.list` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GestureInfo {
    /// The gesture, as its client named it.
    pub gesture_id: GestureId,
    /// The client that opened it.
    pub client_id: ClientId,
    /// The value of each parameter it holds now, as setter edits. A parameter another
    /// gesture took over is not among them.
    pub values: Vec<Edit>,
}

/// Whether `live.set` accepts `edit` as a value: an item's placement
/// (`item.set_transform`, `item.set_geometry`) and its crop (`item.set_crop`), which the
/// real-time threads take from a parameter channel, and since 0.67 a source's settings
/// (`source.set_settings`, ADR-0091), of which the engine accepts only the parameters
/// its kind marks `live`, and since 0.70 a scene's camera (`scene.set_camera`,
/// ADR-0094). Since 0.180 (ADR-0213) also a filter's settings (`filter.set_settings`),
/// again only the parameters its kind marks `live`, an item's opacity
/// (`item.set_opacity`), and the gains: a source's fader (`source.set_audio`), a bus's
/// (`bus.set_fader`) and a send's (`send.set`), of which [`live_problem`] says which
/// fields a value may carry.
pub fn is_live_adjustable(edit: &Edit) -> bool {
    matches!(
        edit,
        Edit::ItemSetTransform { .. }
            | Edit::ItemSetGeometry { .. }
            | Edit::ItemSetCrop { .. }
            | Edit::ItemSetOpacity { .. }
            | Edit::SourceSetSettings { .. }
            | Edit::FilterSetSettings { .. }
            | Edit::SceneSetCamera { .. }
            | Edit::SourceSetAudio { .. }
            | Edit::BusSetFader { .. }
            | Edit::SendSet { .. }
    )
}

/// Why `live.set` refuses `edit`'s fields though its type is live-adjustable, as the
/// field and the reason, or `None` when it takes them (ADR-0213): a `source.set_audio`
/// or `bus.set_fader` value sets the gain only, so it must carry `gain_db` and nothing
/// else (turning audio on, when it is heard and muting are edits, sent in a batch).
pub fn live_problem(edit: &Edit) -> Option<(&'static str, &'static str)> {
    const ONLY_GAIN: &str = "is not live-adjustable; only `gain_db` is, so send it in a batch";
    match edit {
        Edit::SourceSetAudio {
            enabled,
            follow,
            gain_db,
            muted,
            ..
        } => {
            if enabled.is_some() {
                Some(("enabled", ONLY_GAIN))
            } else if follow.is_some() {
                Some(("follow", ONLY_GAIN))
            } else if muted.is_some() {
                Some(("muted", ONLY_GAIN))
            } else if gain_db.is_none() {
                Some(("gain_db", "must be given: a live value sets the gain"))
            } else {
                None
            }
        }
        Edit::BusSetFader { gain_db, muted, .. } => {
            if muted.is_some() {
                Some(("muted", ONLY_GAIN))
            } else if gain_db.is_none() {
                Some(("gain_db", "must be given: a live value sets the gain"))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_live_adjustable, live_problem};
    use crate::{BusId, Edit, Finite32, SourceId};

    fn gain(enabled: Option<bool>, gain_db: Option<f32>, muted: Option<bool>) -> Edit {
        Edit::SourceSetAudio {
            source_id: SourceId::new("mic"),
            enabled,
            follow: None,
            gain_db: gain_db.and_then(Finite32::new),
            muted,
        }
    }

    #[test]
    fn gains_sends_opacity_and_filter_settings_are_live_adjustable() {
        let edits: Vec<Edit> = serde_json::from_value(serde_json::json!([
            {"type": "filter.set_settings", "owner": {"type": "master"}, "filter_id": "f",
             "patch": {"size": 4}},
            {"type": "item.set_opacity", "item_id": "i", "opacity": 0.5},
            {"type": "send.set", "source_id": "mic", "bus_id": "b", "gain_db": -3.0},
            {"type": "bus.set_fader", "bus_id": "master", "gain_db": -3.0},
            {"type": "source.set_audio", "source_id": "mic", "gain_db": -3.0},
        ]))
        .unwrap();
        for edit in &edits {
            assert!(is_live_adjustable(edit), "{}", edit.name());
            assert_eq!(live_problem(edit), None, "{}", edit.name());
        }
    }

    #[test]
    fn a_live_fader_sets_its_gain_and_nothing_else() {
        assert_eq!(live_problem(&gain(None, Some(-6.0), None)), None);
        assert_eq!(
            live_problem(&gain(Some(true), Some(-6.0), None)).map(|p| p.0),
            Some("enabled")
        );
        assert_eq!(
            live_problem(&gain(None, Some(-6.0), Some(true))).map(|p| p.0),
            Some("muted")
        );
        assert_eq!(
            live_problem(&gain(None, None, None)).map(|p| p.0),
            Some("gain_db")
        );
        let bus = |gain_db: Option<f32>, muted: Option<bool>| Edit::BusSetFader {
            bus_id: BusId::master(),
            gain_db: gain_db.and_then(Finite32::new),
            muted,
        };
        assert_eq!(live_problem(&bus(Some(0.0), None)), None);
        assert_eq!(
            live_problem(&bus(Some(0.0), Some(false))).map(|p| p.0),
            Some("muted")
        );
        assert_eq!(live_problem(&bus(None, None)).map(|p| p.0), Some("gain_db"));
    }

    #[test]
    fn structural_edits_stay_out_of_live_values() {
        let edit: Edit = serde_json::from_value(serde_json::json!(
            {"type": "send.remove", "source_id": "mic", "bus_id": "b"}
        ))
        .unwrap();
        assert!(!is_live_adjustable(&edit));
    }
}
