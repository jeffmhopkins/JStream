//! The gesture table (ADR-0032): every client's open live-value gestures, which gesture
//! holds each parameter, and the values of committed gestures that the render thread
//! has not caught up with yet.
//!
//! It holds no lock and runs no thread: the engine keeps it in its state, under the
//! collection lock, and turns what it returns into events and the render parameter
//! channel ([`LiveParams`]). Live values are runtime state: nothing here changes `rev`,
//! is undone or is saved.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0032-live-edits-and-undo.md, docs/ARCHITECTURE.md#threading

use crate::snapshot::LiveParams;
use jstream_graph::State;
use jstream_protocol::{
    BusId, ClientId, Edit, Event, FilterId, FilterOwner, GestureId, GestureInfo, ItemId,
    LiveOutcome, MAX_GESTURES_PER_CLIENT, MAX_LIVE_VALUES, ProtocolError, SceneId, Settings,
    SourceId, is_live_adjustable, live_problem,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The shortest time between two `live.changed` events of one gesture: at most 30 a
/// second.
pub(crate) const CHANGED_INTERVAL: Duration = Duration::from_micros(33_334);

/// One parameter a live value can override: a field of an item, as the item itself has
/// it (`None`) or as a keyframe scene whose tree holds the item overrides it (#846). The two
/// are different parameters, so gestures on both can be open at once.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Param {
    /// An item's placement: `item.set_transform` and `item.set_geometry` both set it.
    Placement(Option<SceneId>, ItemId),
    /// An item's crop.
    Crop(Option<SceneId>, ItemId),
    /// One setting of a source, which its kind marks `live` (ADR-0091).
    Setting(SourceId, String),
    /// A scene's camera, or a keyframe scene's override of it (ADR-0094).
    Camera(SceneId),
    /// One setting of a filter, which its kind marks `live` (ADR-0213).
    FilterSetting(FilterOwner, FilterId, String),
    /// An item's opacity, as the item has it or as a keyframe scene overrides it
    /// (ADR-0213).
    Opacity(Option<SceneId>, ItemId),
    /// A source's fader (ADR-0213).
    SourceGain(SourceId),
    /// A bus's fader (ADR-0213).
    BusGain(BusId),
    /// The gain of a source's send into a bus (ADR-0213).
    SendGain(SourceId, BusId),
}

impl Param {
    /// The parameter `value` sets, if it is live-adjustable. `is_keyframe` says whether
    /// the scene a value names is a keyframe scene; any other `scene_id` names the
    /// item's own placement, which the collection has checked.
    fn of(value: &Edit, is_keyframe: impl Fn(&SceneId) -> bool) -> Option<Param> {
        let keyframe = |scene_id: &Option<SceneId>| scene_id.clone().filter(|id| is_keyframe(id));
        match value {
            Edit::ItemSetTransform {
                item_id, scene_id, ..
            }
            | Edit::ItemSetGeometry {
                item_id, scene_id, ..
            } => Some(Param::Placement(keyframe(scene_id), item_id.clone())),
            Edit::ItemSetCrop {
                item_id, scene_id, ..
            } => Some(Param::Crop(keyframe(scene_id), item_id.clone())),
            Edit::ItemSetOpacity {
                item_id, scene_id, ..
            } => Some(Param::Opacity(keyframe(scene_id), item_id.clone())),
            Edit::SceneSetCamera { scene_id, .. } => Some(Param::Camera(scene_id.clone())),
            Edit::SourceSetAudio { source_id, .. } => Some(Param::SourceGain(source_id.clone())),
            Edit::BusSetFader { bus_id, .. } => Some(Param::BusGain(bus_id.clone())),
            Edit::SendSet {
                source_id, bus_id, ..
            } => Some(Param::SendGain(source_id.clone(), bus_id.clone())),
            _ => None,
        }
    }

    /// Whether the engine draws and hears it through the collection with the value
    /// applied (ADR-0091, ADR-0213) rather than the render parameter channel: every
    /// parameter but an item's placement and crop and a scene's camera.
    fn is_drawn(&self) -> bool {
        !matches!(
            self,
            Param::Placement(..) | Param::Crop(..) | Param::Camera(_)
        )
    }

    /// The item it belongs to, if it is an item's.
    fn item(&self) -> Option<&ItemId> {
        match self {
            Param::Placement(_, item_id) | Param::Crop(_, item_id) | Param::Opacity(_, item_id) => {
                Some(item_id)
            }
            _ => None,
        }
    }

    /// The keyframe scene it belongs to, if it is a keyframe's.
    fn keyframe(&self) -> Option<&SceneId> {
        match self {
            Param::Placement(keyframe, _)
            | Param::Crop(keyframe, _)
            | Param::Opacity(keyframe, _) => keyframe.as_ref(),
            _ => None,
        }
    }

    /// Whether what it sets is still in `state`: its item (in the keyframe scene it
    /// names, which must still be one), source, filter, scene, bus or send.
    fn exists(&self, state: &State, is_keyframe: &impl Fn(&SceneId) -> bool) -> bool {
        match self {
            Param::Placement(..) | Param::Crop(..) | Param::Opacity(..) => {
                self.item().is_some_and(|item| state.item(item).is_some())
                    && self.keyframe().is_none_or(is_keyframe)
            }
            Param::Setting(source_id, _) | Param::SourceGain(source_id) => {
                state.source(source_id).is_some()
            }
            Param::Camera(scene_id) => state.scene(scene_id).is_some(),
            Param::FilterSetting(owner, filter_id, _) => state.filter(owner, filter_id).is_some(),
            Param::BusGain(bus_id) => state.bus(bus_id).is_some(),
            Param::SendGain(source_id, bus_id) => state.send(source_id, bus_id).is_some(),
        }
    }
}

/// `value` as one value per parameter: a source's or a filter's settings patch split
/// into one setting each (ADR-0091, ADR-0213), any other value as it is.
fn split(value: Edit) -> Vec<Edit> {
    let one = |id: &str, v: &serde_json::Value| {
        let mut one = Settings::new();
        one.insert(id, v.clone());
        one
    };
    match value {
        Edit::SourceSetSettings { source_id, patch } => patch
            .iter()
            .map(|(id, v)| Edit::SourceSetSettings {
                source_id: source_id.clone(),
                patch: one(id, v),
            })
            .collect(),
        Edit::FilterSetSettings {
            owner,
            filter_id,
            patch,
        } => patch
            .iter()
            .map(|(id, v)| Edit::FilterSetSettings {
                owner: owner.clone(),
                filter_id: filter_id.clone(),
                patch: one(id, v),
            })
            .collect(),
        other => vec![other],
    }
}

/// The parameter a value from [`split`] sets, if it is live-adjustable.
fn param_of(value: &Edit, is_keyframe: impl Fn(&SceneId) -> bool) -> Option<Param> {
    match value {
        Edit::SourceSetSettings { source_id, patch } => patch
            .iter()
            .next()
            .map(|(id, _)| Param::Setting(source_id.clone(), id.to_owned())),
        Edit::FilterSetSettings {
            owner,
            filter_id,
            patch,
        } => patch
            .iter()
            .next()
            .map(|(id, _)| Param::FilterSetting(owner.clone(), filter_id.clone(), id.to_owned())),
        other => Param::of(other, is_keyframe),
    }
}

/// A gesture is named by its client and the ID the client chose.
type Key = (ClientId, GestureId);

/// One open gesture.
#[derive(Debug, Default)]
struct Gesture {
    /// The newest value of each parameter it holds.
    values: BTreeMap<Param, Edit>,
    /// The parameters whose value changed since its last `live.changed`.
    unsent: BTreeSet<Param>,
    /// When its last `live.changed` went out.
    sent_at: Option<Instant>,
}

/// A gesture that ended, for the audit log.
#[derive(Debug)]
pub(crate) struct Ended {
    pub(crate) client_id: ClientId,
    pub(crate) gesture_id: GestureId,
    pub(crate) outcome: LiveOutcome,
    /// The names of the edits it held values for, one per parameter.
    pub(crate) edits: Vec<&'static str>,
}

/// Every open gesture, and the parameter channel's contents.
#[derive(Debug, Default)]
pub(crate) struct Gestures {
    open: BTreeMap<Key, Gesture>,
    /// Which gesture holds each parameter. A parameter is held by at most one.
    holders: BTreeMap<Param, Key>,
    /// Committed gestures' values, with the render generation of the snapshot that
    /// carries the commit, oldest first. They stay on the parameter channel until the
    /// render thread has adopted that generation, so no frame shows the value from
    /// before the gesture in between.
    releasing: Vec<(u64, BTreeMap<Param, Edit>)>,
}

impl Gestures {
    /// Checks the shape of a `live.set` before its values are validated against the
    /// collection: at most [`MAX_LIVE_VALUES`] values, at least one, each a
    /// live-adjustable setter carrying only live fields ([`live_problem`]: a fader's
    /// gain alone), and room for the gesture among the client's open ones.
    pub(crate) fn check_set(
        &self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        values: &[Edit],
    ) -> Result<(), ProtocolError> {
        if values.is_empty() {
            return Err(invalid("values", "must name at least one value"));
        }
        if values.len() > MAX_LIVE_VALUES {
            return Err(invalid("values", "must name at most 256 values"));
        }
        if let Some(at) = values.iter().position(|value| !is_live_adjustable(value)) {
            return Err(invalid(
                &format!("values.{at}.type"),
                &format!(
                    "`{}` is not live-adjustable; send it in a batch",
                    values[at].name()
                ),
            ));
        }
        if let Some((at, (field, reason))) = values
            .iter()
            .enumerate()
            .find_map(|(at, value)| live_problem(value).map(|problem| (at, problem)))
        {
            return Err(invalid(&format!("values.{at}.{field}"), reason));
        }
        let key = (client_id.clone(), gesture_id.clone());
        if !self.open.contains_key(&key) && self.count(client_id) >= MAX_GESTURES_PER_CLIENT {
            return Err(invalid(
                "gesture_id",
                "the client already holds 16 open gestures; commit or cancel one first",
            ));
        }
        Ok(())
    }

    /// Opens the gesture if it is new and sets `values`, which [`check_set`](Self::check_set)
    /// and the collection have accepted. Each takes its parameter over from any other
    /// gesture that held it. `is_keyframe` says which scenes are keyframe scenes, whose
    /// values are parameters of their own.
    pub(crate) fn set(
        &mut self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        values: Vec<Edit>,
        is_keyframe: impl Fn(&SceneId) -> bool,
    ) {
        let key = (client_id.clone(), gesture_id.clone());
        self.open.entry(key.clone()).or_default();
        for value in values.into_iter().flat_map(split) {
            let Some(param) = param_of(&value, &is_keyframe) else {
                continue;
            };
            if let Some(holder) = self.holders.insert(param.clone(), key.clone())
                && holder != key
                && let Some(other) = self.open.get_mut(&holder)
            {
                other.values.remove(&param);
                other.unsent.remove(&param);
            }
            if let Some(gesture) = self.open.get_mut(&key) {
                gesture.values.insert(param.clone(), value);
                gesture.unsent.insert(param);
            }
        }
    }

    /// Whether `client_id` has `gesture_id` open.
    pub(crate) fn is_open(&self, client_id: &ClientId, gesture_id: &GestureId) -> bool {
        self.open
            .contains_key(&(client_id.clone(), gesture_id.clone()))
    }

    /// The `live.changed` event a gesture owes before it ends: its values not sent yet,
    /// if any.
    pub(crate) fn flush(&mut self, client_id: &ClientId, gesture_id: &GestureId) -> Option<Event> {
        let key = (client_id.clone(), gesture_id.clone());
        let gesture = self.open.get_mut(&key)?;
        changed(&key, gesture, Instant::now())
    }

    /// The `live.changed` events due at `now`: one for each gesture with unsent values
    /// whose last event is at least [`CHANGED_INTERVAL`] old.
    pub(crate) fn due(&mut self, now: Instant) -> Vec<Event> {
        self.open
            .iter_mut()
            .filter(|(_, gesture)| {
                gesture
                    .sent_at
                    .is_none_or(|sent| now.saturating_duration_since(sent) >= CHANGED_INTERVAL)
            })
            .filter_map(|(key, gesture)| changed(key, gesture, now))
            .collect()
    }

    /// When the next `live.changed` is due, if a gesture has unsent values.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.open
            .values()
            .filter(|gesture| !gesture.unsent.is_empty())
            .map(|gesture| {
                gesture
                    .sent_at
                    .map_or_else(Instant::now, |sent| sent + CHANGED_INTERVAL)
            })
            .min()
    }

    /// Ends a gesture without a commit: its parameters show their collection values
    /// again. `None` if the client has no such gesture open.
    pub(crate) fn cancel(
        &mut self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        outcome: LiveOutcome,
    ) -> Option<Ended> {
        let key = (client_id.clone(), gesture_id.clone());
        let gesture = self.remove(&key)?;
        Some(ended(key, &gesture, outcome))
    }

    /// Ends a gesture whose committing batch applied. Its values stay on the parameter
    /// channel until the render thread adopts `generation`, the render generation of
    /// the snapshot that carries the commit.
    pub(crate) fn commit(&mut self, client_id: &ClientId, gesture_id: &GestureId, generation: u64) {
        let key = (client_id.clone(), gesture_id.clone());
        if let Some(mut gesture) = self.remove(&key) {
            // A drawn value's committed value is in the collection the snapshot is
            // built from, so only the channel's values wait for the render thread.
            gesture.values.retain(|param, _| !param.is_drawn());
            if !gesture.values.is_empty() {
                self.releasing.push((generation, gesture.values));
            }
        }
    }

    /// The IDs of `client_id`'s open gestures, in order.
    pub(crate) fn of_client(&self, client_id: &ClientId) -> Vec<GestureId> {
        self.open
            .keys()
            .filter(|(owner, _)| owner == client_id)
            .map(|(_, gesture_id)| gesture_id.clone())
            .collect()
    }

    /// Lets go of the committed values whose generation the render thread has caught up
    /// with. Returns whether any went.
    pub(crate) fn release(&mut self, caught_up: impl Fn(u64) -> bool) -> bool {
        let before = self.releasing.len();
        self.releasing
            .retain(|(generation, _)| !caught_up(*generation));
        self.releasing.len() != before
    }

    /// Whether committed values wait for the render thread.
    pub(crate) fn releasing(&self) -> bool {
        !self.releasing.is_empty()
    }

    /// Drops every live value that has nothing left to draw it in `state`: a batch
    /// deleted its item, or the keyframe scene it is addressed to (`is_keyframe` says
    /// which scenes still are), its source, filter, bus or send, or the scene whose
    /// camera it sets. Returns whether any went.
    pub(crate) fn drop_missing(
        &mut self,
        state: &State,
        is_keyframe: impl Fn(&SceneId) -> bool,
    ) -> bool {
        let here = |param: &Param| param.exists(state, &is_keyframe);
        let gone: Vec<Param> = self
            .holders
            .keys()
            .filter(|param| !here(param))
            .cloned()
            .collect();
        for param in &gone {
            if let Some(key) = self.holders.remove(param)
                && let Some(gesture) = self.open.get_mut(&key)
            {
                gesture.values.remove(param);
                gesture.unsent.remove(param);
            }
        }
        let mut released = false;
        for (_, values) in &mut self.releasing {
            let before = values.len();
            values.retain(|param, _| here(param));
            released |= values.len() != before;
        }
        self.releasing.retain(|(_, values)| !values.is_empty());
        !gone.is_empty() || released
    }

    /// What the parameter channel holds: the values committed gestures still keep,
    /// oldest first, then every open gesture's, which win.
    pub(crate) fn params(&self) -> LiveParams {
        let committed = self.releasing.iter().flat_map(|(_, values)| values.iter());
        let open = self.holders.iter().filter_map(|(param, key)| {
            self.open
                .get(key)
                .and_then(|gesture| gesture.values.get_key_value(param))
        });
        LiveParams::from_values(
            committed
                .chain(open)
                .map(|(param, value)| (param.keyframe(), value)),
        )
    }

    /// The open gestures' values the engine draws the collection with applied: sources'
    /// and filters' settings, one setting each, items' opacity and the gains (ADR-0091,
    /// ADR-0213). Committed gestures' values are in the collection already, so none
    /// wait for the render thread.
    pub(crate) fn drawn(&self) -> Vec<Edit> {
        self.holders
            .iter()
            .filter(|(param, _)| param.is_drawn())
            .filter_map(|(param, key)| self.open.get(key)?.values.get(param).cloned())
            .collect()
    }

    /// The open gestures, for `live.list`, ordered by client and gesture ID.
    pub(crate) fn list(&self) -> Vec<GestureInfo> {
        self.open
            .iter()
            .map(|((client_id, gesture_id), gesture)| GestureInfo {
                gesture_id: gesture_id.clone(),
                client_id: client_id.clone(),
                values: gesture.values.values().cloned().collect(),
            })
            .collect()
    }

    fn count(&self, client_id: &ClientId) -> usize {
        self.open
            .keys()
            .filter(|(owner, _)| owner == client_id)
            .count()
    }

    fn remove(&mut self, key: &Key) -> Option<Gesture> {
        let gesture = self.open.remove(key)?;
        for param in gesture.values.keys() {
            if self.holders.get(param) == Some(key) {
                self.holders.remove(param);
            }
        }
        Some(gesture)
    }
}

/// The `live.changed` for `gesture`'s unsent values, marking them sent at `now`.
fn changed(key: &Key, gesture: &mut Gesture, now: Instant) -> Option<Event> {
    if gesture.unsent.is_empty() {
        return None;
    }
    let unsent = std::mem::take(&mut gesture.unsent);
    gesture.sent_at = Some(now);
    Some(Event::LiveChanged {
        gesture_id: key.1.clone(),
        client_id: key.0.clone(),
        values: unsent
            .iter()
            .filter_map(|param| gesture.values.get(param).cloned())
            .collect(),
    })
}

fn ended(key: Key, gesture: &Gesture, outcome: LiveOutcome) -> Ended {
    Ended {
        client_id: key.0,
        gesture_id: key.1,
        outcome,
        edits: gesture.values.values().map(Edit::name).collect(),
    }
}

fn invalid(field: &str, reason: &str) -> ProtocolError {
    ProtocolError::invalid_argument(field, reason)
}
