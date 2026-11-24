//! What is arriving now, for mapping it: each JTunnel sender's live data with its Mods
//! (ADR-0171, ADR-0181) and each MIDI input's latest control (ADR-0178). The Map
//! control dialog lists these with their values, and Learn takes whichever moves first.
//!
//! [`read`] asks the engine once (`data.list`, each sender's `params` with `data.get`,
//! and `midi.last_controls`); [`live_sources`] reads about seven times a second for as
//! long as the calling component lives. Nothing here is stored: a mapping is the reader
//! ([`MotionKind`]) [`Moved::reader`] or [`mod_reader`] builds.
//!
//! Docs: docs/UI-FEATURES.md#automation, docs/adr/0171-live-data-from-a-daw-drives-scenes.md

use crate::app::SessionContext;
use crate::session::Session;
use crate::store::{Collection, query};
use jstream_control::protocol::{
    Finite32, MidiControlInfo, MotionCurve, MotionKind, MotionMidiControl, Query, QueryResult,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::BTreeMap;
use std::time::Duration;

/// How often an open dialog reads what is arriving.
const POLL: Duration = Duration::from_millis(150);

/// The key a JTunnel sender streams its modulation params under (ADR-0181).
pub const MODS_KEY: &str = "params";
/// How many Mods a sender has.
pub const MOD_COUNT: usize = 32;
/// How far a Mod must travel before Learn takes it: past a DAW's automation noise.
pub const MOD_MOVED: f32 = 0.02;

/// One JTunnel sender's live data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sender {
    /// Its name on the accepted list, as a reader's `input` names it.
    pub input: String,
    /// Its keys with a current message.
    pub keys: Vec<String>,
    /// Its Mods' values, 0 to 1, Mod 1 first; empty when it sends no `params`.
    pub mods: Vec<f32>,
    /// Its Mods' names, as the sender reports them; a slot it doesn't name is empty.
    pub mod_names: Vec<String>,
}

/// Everything arriving now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveSources {
    /// The senders with live data, by name.
    pub senders: Vec<Sender>,
    /// Each MIDI input's latest control change.
    pub controls: Vec<MidiControlInfo>,
    /// Whether the engine has been read yet: before the first read nothing is known, which
    /// is not the same as nothing arriving.
    pub loaded: bool,
}

/// A sender's Mods from its `params` message: `values` as numbers kept within 0 to 1, and
/// `names`. Anything else in the message, and anything past [`MOD_COUNT`], is left out,
/// since a sender's message is untrusted.
pub fn mods_of(message: &serde_json::Value) -> (Vec<f32>, Vec<String>) {
    let list = |name: &str| {
        message
            .get(name)
            .and_then(serde_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice)
    };
    let values = list("values")
        .iter()
        .take(MOD_COUNT)
        .map(|v| {
            let v = v.as_f64().unwrap_or(0.0) as f32;
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
        .collect();
    let names = list("names")
        .iter()
        .take(MOD_COUNT)
        .map(|v| {
            v.as_str()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(40)
                .collect()
        })
        .collect();
    (values, names)
}

/// The reader of Mod `slot` (from 0) of `sender`, or of any sender: key `params`, field
/// `values.<slot>`, over 0 to 1.
pub fn mod_reader(sender: Option<&str>, slot: usize) -> MotionKind {
    MotionKind::Data {
        input: sender.filter(|s| !s.is_empty()).map(str::to_owned),
        key: MODS_KEY.into(),
        field: format!("values.{slot}"),
        range: [Finite32::ZERO, Finite32::ONE],
        states: BTreeMap::new(),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
        color_from: None,
        palette: BTreeMap::new(),
    }
}

/// The Mod a reader reads, as its sender and its slot from 0; `None` for any other
/// reader.
pub fn mod_of(kind: &MotionKind) -> Option<(Option<&str>, usize)> {
    let MotionKind::Data {
        input, key, field, ..
    } = kind
    else {
        return None;
    };
    if key != MODS_KEY {
        return None;
    }
    let slot: usize = field.strip_prefix("values.")?.parse().ok()?;
    (slot < MOD_COUNT).then_some((input.as_deref(), slot))
}

/// What a reader reads, in a few words: "Mod 20 · Bitwig", "CC 21 · Pads", "Note 36",
/// "transport · playing", or a channel's name.
pub fn reader_name(collection: Option<&Collection>, kind: &MotionKind) -> String {
    let from = |input: &Option<String>| input.as_ref().map_or(String::new(), |i| format!(" · {i}"));
    match kind {
        MotionKind::Midi {
            control,
            controller,
            input,
            ..
        } => {
            let what = match (control, controller) {
                (MotionMidiControl::Cc, Some(n)) => format!("CC {n}"),
                (MotionMidiControl::Note, Some(n)) => format!("Note {n}"),
                (MotionMidiControl::PitchBend, _) => "Pitch bend".into(),
                (MotionMidiControl::Pressure, _) => "Pressure".into(),
                _ => "MIDI".into(),
            };
            format!("{what}{}", from(input))
        }
        MotionKind::Data {
            input, key, field, ..
        } => match mod_of(kind) {
            Some((_, slot)) => format!("Mod {}{}", slot + 1, from(input)),
            None if field.is_empty() => format!("{key}{}", from(input)),
            None => format!("{key} · {field}{}", from(input)),
        },
        MotionKind::Channel { channel_id, .. } => {
            collection.map_or_else(|| channel_id.clone(), |c| c.channel_name(channel_id))
        }
        MotionKind::Audio { .. } => collection.map_or_else(
            || "Audio".into(),
            |c| crate::panels::audio_read::audio_name(c, kind),
        ),
        _ => "A motion".into(),
    }
}

/// What reads Mod `slot` (from 0) of `sender` in `collection`, by name: the channels
/// with it as an input, the settings bound to it, and the items and cameras whose
/// motion follows it. A reader of any sender counts for every sender.
pub fn mod_drives(collection: &Collection, sender: &str, slot: usize) -> Vec<String> {
    let reads = |kind: &MotionKind| {
        mod_of(kind).is_some_and(|(input, n)| n == slot && input.is_none_or(|i| i == sender))
    };
    let mut names = Vec::new();
    for channel in &collection.channels {
        if channel.inputs.iter().any(reads) {
            names.push(collection.channel_name(&channel.channel_id));
        }
    }
    for binding in &collection.value_bindings {
        if reads(&binding.input) {
            names.push(crate::panels::channels::target_name(
                collection,
                &binding.target,
            ));
        }
    }
    for scene in &collection.scenes {
        if scene.camera_motion.iter().any(|m| reads(&m.kind)) {
            names.push(format!("{} · camera", scene.name));
        }
        for item in &scene.items {
            if item.motion.iter().any(|m| reads(&m.kind)) {
                names.push(collection.source_name(&item.source_id));
            }
        }
    }
    names.dedup();
    names
}

/// `names` as one short line: the first, and how many more.
pub fn drives_line(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

/// What Learn caught moving.
#[derive(Clone, Debug, PartialEq)]
pub enum Moved {
    /// A MIDI control change.
    Midi(MidiControlInfo),
    /// A sender's Mod, by its slot from 0.
    Mod {
        /// The sender.
        sender: String,
        /// The slot, from 0.
        slot: usize,
    },
}

impl Moved {
    /// The reader of what moved, over 0 to 1.
    pub fn reader(&self) -> MotionKind {
        match self {
            Moved::Midi(control) => MotionKind::Midi {
                control: MotionMidiControl::Cc,
                controller: Some(control.controller),
                channel: Some(control.channel),
                input: crate::jtunnel::bound_input(&control.input),
                smoothing_ms: 50,
                curve: MotionCurve::Linear,
                from: Finite32::ZERO,
                to: Finite32::ONE,
            },
            // Mods come only from JTunnel, and one sender is live: no name (ADR-0218).
            Moved::Mod { slot, .. } => mod_reader(None, *slot),
        }
    }
}

impl Moved {
    /// Whether this and `other` are the same source: a MIDI control by its input,
    /// channel and controller whatever its value, or a Mod by its sender and slot.
    pub fn same(&self, other: &Moved) -> bool {
        match (self, other) {
            (Moved::Midi(a), Moved::Midi(b)) => {
                a.input == b.input && a.channel == b.channel && a.controller == b.controller
            }
            (a @ Moved::Mod { .. }, b @ Moved::Mod { .. }) => a == b,
            _ => false,
        }
    }
}

/// Everything that moved between `baseline` and `now`, with how far it travelled as a
/// share of its whole range, farthest first: each MIDI input whose count of control
/// changes grew (or that is new), by its latest control, and each Mod that went at
/// least [`MOD_MOVED`]. A sender that wasn't there at the baseline has moved nothing:
/// its first message only says where its Mods rest.
pub fn travelled(baseline: &LiveSources, now: &LiveSources) -> Vec<(Moved, f32)> {
    let mut all = Vec::new();
    for control in &now.controls {
        let before = baseline.controls.iter().find(|b| b.input == control.input);
        if before.is_some_and(|b| control.count <= b.count) {
            continue;
        }
        let same =
            before.filter(|b| b.channel == control.channel && b.controller == control.controller);
        let travel = same.map_or(1.0, |b| f32::from(control.value.abs_diff(b.value)) / 127.0);
        // A control that sent a change has moved, however little.
        all.push((Moved::Midi(control.clone()), travel.max(1.0 / 127.0)));
    }
    for sender in &now.senders {
        let Some(before) = baseline.senders.iter().find(|s| s.input == sender.input) else {
            continue;
        };
        for (slot, value) in sender.mods.iter().enumerate() {
            let Some(was) = before.mods.get(slot) else {
                continue;
            };
            let travel = (value - was).abs();
            if travel >= MOD_MOVED {
                all.push((
                    Moved::Mod {
                        sender: sender.input.clone(),
                        slot,
                    },
                    travel,
                ));
            }
        }
    }
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    all
}

/// What moved between `baseline` and `now`: a MIDI control first, else the Mod that
/// travelled farthest.
pub fn moved(baseline: &LiveSources, now: &LiveSources) -> Option<Moved> {
    let all = travelled(baseline, now);
    all.iter()
        .find(|(m, _)| matches!(m, Moved::Midi(_)))
        .or(all.first())
        .map(|(m, _)| m.clone())
}

/// How many reads Learn spends finding what is moving already, before it listens.
pub const SETTLE_READS: u8 = 2;

/// Learn, listening (UI-FEATURES VC-03). Its first [`SETTLE_READS`] reads find what is
/// moving already, such as a DAW's automation while it plays, and those sources are
/// left out; what moves after that is a candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct Listening {
    /// What was arriving when it last took stock.
    baseline: LiveSources,
    /// The reads still spent settling.
    settling: u8,
    /// What was moving already.
    noisy: Vec<Moved>,
}

impl Listening {
    /// Starts listening from what is arriving `now`.
    pub fn start(now: LiveSources) -> Self {
        Listening {
            baseline: now,
            settling: SETTLE_READS,
            noisy: Vec::new(),
        }
    }

    /// Whether it is still finding what moves by itself.
    pub fn settling(&self) -> bool {
        self.settling > 0
    }

    /// Takes one read. While settling it answers nothing and notes what moved; after,
    /// it answers the candidates: what has moved since it settled, farthest first,
    /// without what was moving already.
    pub fn hear(&mut self, now: &LiveSources) -> Vec<(Moved, f32)> {
        let moved = travelled(&self.baseline, now);
        if self.settling > 0 {
            for (source, _) in moved {
                if !self.noisy.iter().any(|n| n.same(&source)) {
                    self.noisy.push(source);
                }
            }
            self.settling -= 1;
            if self.settling == 0 {
                self.baseline = now.clone();
            }
            return Vec::new();
        }
        moved
            .into_iter()
            .filter(|(source, _)| !self.noisy.iter().any(|n| n.same(source)))
            .collect()
    }
}

/// A message's leaves as `(field, value)` rows, in order, for picking a field: numbers,
/// switches and texts by their dotted path (ADR-0171 decision 2), at most `limit` of
/// them and no deeper than 8. A text is cut to 40 characters with its control
/// characters removed, since a sender's message is untrusted.
pub fn fields_of(message: &serde_json::Value, limit: usize) -> Vec<(String, String)> {
    fn walk(
        value: &serde_json::Value,
        path: &str,
        depth: usize,
        limit: usize,
        out: &mut Vec<(String, String)>,
    ) {
        if out.len() >= limit || depth > 8 {
            return;
        }
        let join = |name: &str| {
            if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}.{name}")
            }
        };
        match value {
            serde_json::Value::Object(map) => {
                for (name, v) in map {
                    walk(v, &join(name), depth + 1, limit, out);
                }
            }
            serde_json::Value::Array(list) => {
                for (n, v) in list.iter().enumerate() {
                    walk(v, &join(&n.to_string()), depth + 1, limit, out);
                }
            }
            serde_json::Value::Null => {}
            serde_json::Value::String(text) => {
                let text: String = text.chars().filter(|c| !c.is_control()).take(40).collect();
                out.push((path.to_owned(), format!("“{text}”")));
            }
            other => out.push((path.to_owned(), other.to_string())),
        }
    }
    let mut out = Vec::new();
    walk(message, "", 0, limit, &mut out);
    out
}

/// One sender's newest message for `key`, or `None` when it has none.
pub async fn message(session: &Session, input: &str, key: &str) -> Option<serde_json::Value> {
    let asked = Query::DataGet {
        input: input.to_owned(),
        key: key.to_owned(),
    };
    match query(session, asked).await {
        Ok(QueryResult::DataGet { remote_value }) => remote_value,
        _ => None,
    }
}

/// Everything arriving now. An engine that answers none of it reads as nothing arriving.
pub async fn read(session: &Session) -> LiveSources {
    let mut senders = Vec::new();
    if let Ok(QueryResult::DataList { inputs }) = query(session, Query::DataList).await {
        for info in inputs {
            let (mods, mod_names) = if info.remote_keys.iter().any(|k| k == MODS_KEY) {
                message(session, &info.input, MODS_KEY)
                    .await
                    .map(|m| mods_of(&m))
                    .unwrap_or_default()
            } else {
                Default::default()
            };
            senders.push(Sender {
                input: info.input,
                keys: info.remote_keys,
                mods,
                mod_names,
            });
        }
    }
    let controls = crate::panels::learn::controls(session)
        .await
        .unwrap_or_default();
    LiveSources {
        senders,
        controls,
        loaded: true,
    }
}

/// What is arriving, read every 150 ms for as long as the calling component lives.
pub fn live_sources() -> RwSignal<LiveSources> {
    let live = RwSignal::new(LiveSources::default());
    if let Some(session) = use_context::<SessionContext>() {
        poll(session, live, true);
    }
    live
}

/// Reads at once when `first`, else after [`POLL`], then again, until `live` is disposed.
fn poll(session: SessionContext, live: RwSignal<LiveSources>, first: bool) {
    let step = move || {
        spawn_local(async move {
            let Some(s) = session.0.try_get_value().flatten() else {
                return;
            };
            if live.try_get_untracked().is_none() {
                return;
            }
            let now = read(&s).await;
            // `try_update` answers `None` once the component is gone.
            let alive = live.try_update(|l| {
                if *l != now {
                    *l = now;
                }
            });
            if alive.is_some() {
                poll(session, live, false);
            }
        });
    };
    if first {
        step();
    } else {
        set_timeout(step, POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sender(name: &str, mods: &[f32]) -> Sender {
        Sender {
            input: name.into(),
            keys: vec![MODS_KEY.into()],
            mods: mods.to_vec(),
            mod_names: Vec::new(),
        }
    }

    fn control(input: &str, controller: u8, count: u64) -> MidiControlInfo {
        MidiControlInfo {
            input: input.into(),
            channel: 1,
            controller,
            value: 64,
            count,
        }
    }

    #[test]
    fn a_params_message_gives_its_mods_within_bounds() {
        let message = json!({
            "values": [0.25, 7, "x", -1],
            "names": ["Cutoff", "A\u{7}B", 3],
        });
        let (values, names) = mods_of(&message);
        assert_eq!(values, vec![0.25, 1.0, 0.0, 0.0]);
        assert_eq!(names, vec!["Cutoff", "AB", ""]);
        // Not a params message at all.
        assert_eq!(mods_of(&json!("x")), (Vec::new(), Vec::new()));
        // More than a sender has: cut to 32.
        let many = json!({ "values": vec![0.5; 100] });
        assert_eq!(mods_of(&many).0.len(), MOD_COUNT);
    }

    #[test]
    fn a_mod_reader_reads_values_n_of_params_and_names_itself() {
        let any = mod_reader(None, 19);
        assert!(matches!(
            &any,
            MotionKind::Data { input: None, key, field, .. } if key == "params" && field == "values.19"
        ));
        assert_eq!(mod_of(&any), Some((None, 19)));
        assert_eq!(reader_name(None, &any), "Mod 20");
        let named = mod_reader(Some("Bitwig"), 0);
        assert_eq!(mod_of(&named), Some((Some("Bitwig"), 0)));
        assert_eq!(reader_name(None, &named), "Mod 1 · Bitwig");
        // An empty sender name is any sender.
        assert_eq!(mod_of(&mod_reader(Some(""), 3)), Some((None, 3)));
        // Other data isn't a Mod, and neither is a slot a sender doesn't have.
        let mut other = mod_reader(None, 0);
        if let MotionKind::Data { key, field, .. } = &mut other {
            *key = "transport".into();
            *field = "playing".into();
        }
        assert_eq!(mod_of(&other), None);
        assert_eq!(reader_name(None, &other), "transport · playing");
        let mut past = mod_reader(None, 0);
        if let MotionKind::Data { field, .. } = &mut past {
            *field = "values.32".into();
        }
        assert_eq!(mod_of(&past), None);
    }

    #[test]
    fn learn_takes_a_midi_control_first_then_the_mod_that_travelled_farthest() {
        let baseline = LiveSources {
            loaded: true,
            senders: vec![sender("Bitwig", &[0.0, 0.5, 0.5])],
            controls: vec![control("Pads", 7, 3)],
        };
        assert_eq!(moved(&baseline, &baseline), None);
        // Noise under the threshold is not a move.
        let noise = LiveSources {
            loaded: true,
            senders: vec![sender("Bitwig", &[0.01, 0.5, 0.5])],
            ..baseline.clone()
        };
        assert_eq!(moved(&baseline, &noise), None);
        let swept = LiveSources {
            loaded: true,
            senders: vec![sender("Bitwig", &[0.05, 0.5, 0.9])],
            ..baseline.clone()
        };
        assert_eq!(
            moved(&baseline, &swept),
            Some(Moved::Mod {
                sender: "Bitwig".into(),
                slot: 2
            })
        );
        let both = LiveSources {
            loaded: true,
            controls: vec![control("Pads", 21, 4)],
            ..swept.clone()
        };
        assert!(matches!(moved(&baseline, &both), Some(Moved::Midi(c)) if c.controller == 21));
        // A sender heard for the first time only says where its Mods rest.
        let arrived = LiveSources {
            loaded: true,
            senders: vec![sender("Bitwig", &[0.0, 0.5, 0.5]), sender("Live", &[1.0])],
            ..baseline.clone()
        };
        assert_eq!(moved(&baseline, &arrived), None);
    }

    #[test]
    fn learn_leaves_out_what_was_moving_before_it_listened() {
        let at = |mods: &[f32]| LiveSources {
            loaded: true,
            senders: vec![sender("Bitwig", mods)],
            controls: Vec::new(),
        };
        // Mod 1 is automated: it moves on every read.
        let mut listening = Listening::start(at(&[0.0, 0.5, 0.5]));
        assert!(listening.settling());
        assert!(listening.hear(&at(&[0.1, 0.5, 0.5])).is_empty());
        assert!(listening.hear(&at(&[0.2, 0.5, 0.5])).is_empty());
        assert!(!listening.settling());
        // Still only the automation: no candidate.
        assert!(listening.hear(&at(&[0.4, 0.5, 0.5])).is_empty());
        // The person moves Mod 3, then Mod 2 a little: both are candidates, farthest first.
        let heard = listening.hear(&at(&[0.6, 0.55, 0.9]));
        let slots: Vec<_> = heard
            .iter()
            .map(|(m, _)| match m {
                Moved::Mod { slot, .. } => *slot,
                Moved::Midi(_) => 99,
            })
            .collect();
        assert_eq!(slots, vec![2, 1]);
    }

    #[test]
    fn a_midi_control_that_sent_a_change_travelled_by_its_value() {
        let at = |value: u8, count: u64| LiveSources {
            loaded: true,
            senders: Vec::new(),
            controls: vec![MidiControlInfo {
                value,
                ..control("Pads", 7, count)
            }],
        };
        let far = travelled(&at(0, 1), &at(127, 9));
        assert!(matches!(&far[..], [(Moved::Midi(_), t)] if (*t - 1.0).abs() < 1e-6));
        // The same value sent again still counts as moved.
        let nudge = travelled(&at(64, 1), &at(64, 2));
        assert!(matches!(&nudge[..], [(Moved::Midi(_), t)] if *t > 0.0));
        assert!(travelled(&at(64, 2), &at(64, 2)).is_empty());
        assert!(Moved::Midi(control("Pads", 7, 1)).same(&Moved::Midi(control("Pads", 7, 5))));
        assert!(!Moved::Midi(control("Pads", 7, 1)).same(&Moved::Midi(control("Pads", 8, 1))));
    }

    #[test]
    fn what_moved_becomes_a_reader_over_the_whole_range() {
        let pad = Moved::Midi(control("Pads", 21, 1)).reader();
        assert!(matches!(
            &pad,
            MotionKind::Midi { controller: Some(21), channel: Some(1), input: Some(i), .. } if i == "Pads"
        ));
        assert_eq!(reader_name(None, &pad), "CC 21 · Pads");
        let knob = Moved::Mod {
            sender: "Bitwig".into(),
            slot: 4,
        }
        .reader();
        assert_eq!(mod_of(&knob), Some((Some("Bitwig"), 4)));
    }

    #[test]
    fn a_messages_fields_are_its_leaves_by_dotted_path() {
        let message = json!({
            "playing": true,
            "tempo": 120.5,
            "name": "Verse\n1",
            "slots": [{ "state": "playing" }, { "state": null }],
        });
        let fields = fields_of(&message, 100);
        assert!(fields.contains(&("playing".into(), "true".into())));
        assert!(fields.contains(&("tempo".into(), "120.5".into())));
        assert!(fields.contains(&("name".into(), "“Verse1”".into())));
        assert!(fields.contains(&("slots.0.state".into(), "“playing”".into())));
        assert!(!fields.iter().any(|(path, _)| path == "slots.1.state"));
        assert_eq!(fields_of(&message, 2).len(), 2);
        // A message that is one number is its own field.
        assert_eq!(fields_of(&json!(3), 10), vec![(String::new(), "3".into())]);
    }

    #[test]
    fn a_short_line_names_the_first_reader_and_counts_the_rest() {
        assert_eq!(drives_line(&[]), "");
        assert_eq!(drives_line(&["Zoom".into()]), "Zoom");
        assert_eq!(
            drives_line(&["Zoom".into(), "Roll".into(), "Pan".into()]),
            "Zoom +2"
        );
    }
}
