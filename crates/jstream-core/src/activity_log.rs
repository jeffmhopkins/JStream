//! The activity log (ADR-0083): the last [`MAX_ACTIVITY_ENTRIES`] audit entries, kept in
//! memory for `activity.list`, each with one English sentence that
//! says what the call did.
//!
//! The sentence is written from the collection just before the call runs, so a deleted
//! object keeps its name and a setter can say what it changed from. It names objects and
//! gives numeric, on/off and enum values only: never a source's or filter's settings, an
//! output's target or a stream key (R-SEC-001). It is not written to the audit file.
//!
//! Docs: docs/adr/0083-activity-log-over-the-protocol.md, crates/jstream-core/README.md#design

use crate::audit::AuditEntry;
use jstream_graph::State;
use jstream_protocol::{
    Action, ActivityEntry, Call, Edit, FilterOwner, Finite32, HistoryOp, ItemId, LiveOutcome,
    MAX_ACTIVITY_ENTRIES, MAX_ACTIVITY_SUMMARY_CHARS, SceneId, SourceId,
};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// The recent entries, oldest first, and the number the next one takes.
#[derive(Debug, Default)]
pub(crate) struct Activity {
    entries: VecDeque<ActivityEntry>,
    last_seq: u64,
}

impl Activity {
    /// Keeps `entry` as the newest, dropping the oldest past the limit, and returns it
    /// as the protocol carries it.
    pub(crate) fn push(&mut self, entry: &AuditEntry) -> ActivityEntry {
        self.last_seq += 1;
        let time_ms = unix_ms(entry.time);
        let added = ActivityEntry {
            seq: self.last_seq,
            time_ms,
            client_id: entry.client_id.clone(),
            client_name: entry.client_name.clone(),
            call: entry.call.to_owned(),
            summary: cut(&entry.summary),
            error: entry.error.map(str::to_owned),
        };
        if self.entries.len() >= MAX_ACTIVITY_ENTRIES as usize {
            self.entries.pop_front();
        }
        self.entries.push_back(added.clone());
        added
    }

    /// The newest entries first: at most `limit` (at most [`MAX_ACTIVITY_ENTRIES`]), and
    /// only those after `after_seq` when it is given.
    pub(crate) fn list(&self, after_seq: Option<u64>, limit: Option<u32>) -> Vec<ActivityEntry> {
        let limit = limit.map_or(MAX_ACTIVITY_ENTRIES, |l| l.min(MAX_ACTIVITY_ENTRIES)) as usize;
        let after = after_seq.unwrap_or(0);
        self.entries
            .iter()
            .rev()
            .take_while(|entry| entry.seq > after)
            .take(limit)
            .cloned()
            .collect()
    }
}

/// `summary` cut to [`MAX_ACTIVITY_SUMMARY_CHARS`], ending with `…` when it was cut, so a
/// name an agent sent can't make the list large.
fn cut(summary: &str) -> String {
    match summary.char_indices().nth(MAX_ACTIVITY_SUMMARY_CHARS - 1) {
        Some((end, _)) if summary[end..].chars().nth(1).is_some() => {
            format!("{}…", &summary[..end])
        }
        _ => summary.to_owned(),
    }
}

/// `time` in milliseconds since the Unix epoch; 0 before it.
pub(crate) fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// The sentence for `call`, which is about to run on `state`.
pub(crate) fn describe(call: &Call, state: &State) -> String {
    match call {
        Call::Batch(batch) => describe_batch(&batch.edits, state),
        Call::History(history) => match history.op {
            HistoryOp::Undo => "Undid its last change".to_owned(),
            HistoryOp::Redo => "Redid its last undone change".to_owned(),
            _ => capitalize(&humanize(history.op.name())),
        },
        Call::Action { action } => describe_action(action, state),
        _ => "Called the engine".to_owned(),
    }
}

/// The sentence for a gesture that ended without a commit, holding values for `edits`.
pub(crate) fn describe_gesture(outcome: LiveOutcome, edits: &[&str]) -> String {
    let what = match edits {
        [edit] => format!(" to {}", humanize(edit)),
        _ => String::new(),
    };
    match outcome {
        LiveOutcome::Cancelled => format!("Cancelled a live change{what}"),
        _ => format!("A live change{what} ended with its session"),
    }
}

/// A batch: its delete, if it has one (a delete plan's other edits only clear the way),
/// else its first edit, and how many more it made.
fn describe_batch(edits: &[Edit], state: &State) -> String {
    let lead = edits
        .iter()
        .rposition(is_delete)
        .unwrap_or(0)
        .min(edits.len().saturating_sub(1));
    let Some(edit) = edits.get(lead) else {
        return "Applied an empty batch".to_owned();
    };
    let sentence = describe_edit(edit, state);
    match edits.len() {
        0 | 1 => sentence,
        2 => format!("{sentence} and 1 more change"),
        n => format!("{sentence} and {} more changes", n - 1),
    }
}

fn is_delete(edit: &Edit) -> bool {
    matches!(
        edit,
        Edit::SceneDelete { .. }
            | Edit::SourceDelete { .. }
            | Edit::FolderDelete { .. }
            | Edit::LabelDelete { .. }
            | Edit::BusDelete { .. }
            | Edit::OutputDelete { .. }
            | Edit::EncoderDelete { .. }
            | Edit::VideoMixDelete { .. }
            | Edit::TransitionDelete { .. }
    )
}

fn describe_action(action: &Action, state: &State) -> String {
    match action {
        Action::SceneSwitch { scene_id, .. } => format!("Switched to {}", scene(state, scene_id)),
        Action::StudioSetPreview { scene_id } => {
            format!("Put {} on preview", scene(state, scene_id))
        }
        Action::OutputStart { output_id } => {
            format!("Started output {}", quote(output_id.as_str()))
        }
        Action::OutputStop { output_id } => format!("Stopped output {}", quote(output_id.as_str())),
        Action::OutputSetKey { output_id, .. } => {
            format!("Set the stream key of output {}", quote(output_id.as_str()))
        }
        Action::OutputClearKey { output_id } => {
            format!(
                "Cleared the stream key of output {}",
                quote(output_id.as_str())
            )
        }
        Action::AudioResetLoudness {} => "Reset the loudness meter".to_owned(),
        Action::PreviewSubscribe { .. } => "Opened a preview stream".to_owned(),
        Action::PreviewUnsubscribe { .. } => "Closed a preview stream".to_owned(),
        Action::YoutubeSignIn => "Started a YouTube sign-in".to_owned(),
        Action::YoutubeSignOut => "Signed out of YouTube".to_owned(),
        Action::YoutubeCreateBroadcast { .. } => "Created a YouTube broadcast".to_owned(),
        Action::YoutubeBindBroadcast { .. } => "Bound a YouTube broadcast".to_owned(),
        Action::YoutubeTransitionBroadcast { .. } => "Moved a YouTube broadcast on".to_owned(),
        Action::PairingStart => "Started pairing a device".to_owned(),
        Action::DeviceRevoke { .. } => "Revoked a paired device".to_owned(),
        Action::PluginInstall { plugin_id, .. } => {
            format!("Installed plugin {}", quote(plugin_id.as_str()))
        }
        Action::PluginGrant { plugin_id, .. } => {
            format!("Changed what plugin {} may do", quote(plugin_id.as_str()))
        }
        Action::PluginEnable { plugin_id } => {
            format!("Enabled plugin {}", quote(plugin_id.as_str()))
        }
        other => capitalize(&humanize(other.name())),
    }
}

/// One edit's sentence.
fn describe_edit(edit: &Edit, state: &State) -> String {
    match edit {
        Edit::SceneCreate { name, .. } => format!("Created scene {}", quote(name)),
        Edit::SceneCreateVariant {
            name, variant_of, ..
        } => format!(
            "Created keyframe {} of {}",
            quote(name),
            scene(state, variant_of)
        ),
        Edit::SceneRename { scene_id, name } => {
            format!(
                "Renamed scene {} to {}",
                scene(state, scene_id),
                quote(name)
            )
        }
        Edit::SceneSetRole { scene_id, role } => format!(
            "Made {} a {}",
            scene(state, scene_id),
            if role.is_scene() { "scene" } else { "group" }
        ),
        Edit::SceneDelete { scene_id } => format!("Deleted scene {}", scene(state, scene_id)),
        Edit::SceneReorder { scene_id, .. } => format!("Moved scene {}", scene(state, scene_id)),
        Edit::SceneSetFolder { scene_id, .. } => {
            format!("Moved scene {} to another folder", scene(state, scene_id))
        }
        Edit::SceneSetMeta { scene_id, .. } => {
            format!("Changed the details of scene {}", scene(state, scene_id))
        }
        Edit::SceneSetTransitions { scene_id, .. } => {
            format!(
                "Changed the transitions of scene {}",
                scene(state, scene_id)
            )
        }
        Edit::ItemAdd {
            scene_id,
            source_id,
            ..
        } => format!(
            "Added {} to {}",
            source(state, source_id),
            scene(state, scene_id)
        ),
        Edit::ItemRemove { item_id } => format!("Removed {}", item(state, item_id)),
        Edit::ItemMove {
            item_id, scene_id, ..
        } => format!(
            "Moved {} to {}",
            item(state, item_id),
            scene(state, scene_id)
        ),
        Edit::ItemReorder { item_id, .. } => format!("Reordered {}", item(state, item_id)),
        Edit::ItemSetTransform { item_id, .. } | Edit::ItemSetGeometry { item_id, .. } => {
            format!("Moved or resized {}", item(state, item_id))
        }
        Edit::ItemSetCrop { item_id, .. } => format!("Cropped {}", item(state, item_id)),
        Edit::ItemSetVisible {
            item_id, visible, ..
        } => format!(
            "{} {}",
            if *visible { "Showed" } else { "Hid" },
            item(state, item_id)
        ),
        Edit::ItemSetOpacity {
            item_id, opacity, ..
        } => {
            let before = state.item(item_id).map(|(_, item)| item.opacity());
            format!(
                "{} opacity {}",
                item_plain(state, item_id),
                change(before, *opacity, percent, "%")
            )
        }
        Edit::ItemSetBlend { item_id, blend } => format!(
            "Set the blend mode of {} to {}",
            item(state, item_id),
            blend.as_str()
        ),
        Edit::ItemSetSource { item_id, source_id } => format!(
            "Made {} show {}",
            item(state, item_id),
            source(state, source_id)
        ),
        Edit::ItemSetTransition { item_id, .. } => {
            format!(
                "Changed the show and hide transition of {}",
                item(state, item_id)
            )
        }
        Edit::ItemClearOverride { item_id, .. } => {
            format!("Reset a keyframe override of {}", item(state, item_id))
        }
        Edit::SourceCreate { name, .. } => format!("Created source {}", quote(name)),
        Edit::SourceRename { source_id, name } => format!(
            "Renamed source {} to {}",
            source(state, source_id),
            quote(name)
        ),
        Edit::SourceSetSettings { source_id, .. } => {
            format!("Changed the settings of {}", source(state, source_id))
        }
        Edit::SourceDuplicate {
            source_id, name, ..
        } => format!("Duplicated {} as {}", source(state, source_id), quote(name)),
        Edit::SourceDelete { source_id } => format!("Deleted source {}", source(state, source_id)),
        Edit::SourceSetMeta { source_id, .. } => {
            format!("Changed the labels of {}", source(state, source_id))
        }
        Edit::SourceSetAudio {
            source_id,
            enabled,
            gain_db,
            muted,
            ..
        } => {
            let audio = state.source(source_id).and_then(|s| s.audio().copied());
            if let Some(gain) = gain_db {
                format!(
                    "{} gain {}",
                    source_plain(state, source_id),
                    change(audio.map(|a| a.gain_db), *gain, decibels, " dB")
                )
            } else if let Some(muted) = muted {
                format!(
                    "{} {}",
                    if *muted { "Muted" } else { "Unmuted" },
                    source(state, source_id)
                )
            } else if let Some(enabled) = enabled {
                format!(
                    "Turned the audio of {} {}",
                    source(state, source_id),
                    if *enabled { "on" } else { "off" }
                )
            } else {
                format!("Changed the audio of {}", source(state, source_id))
            }
        }
        Edit::FilterAdd { owner, kind, .. } => {
            format!("Added a {kind} filter to {}", owner_name(state, owner))
        }
        Edit::FilterRemove { owner, filter_id } => format!(
            "Removed filter {} from {}",
            quote(filter_id.as_str()),
            owner_name(state, owner)
        ),
        Edit::FilterSetEnabled {
            owner,
            filter_id,
            enabled,
        } => format!(
            "Turned filter {} of {} {}",
            quote(filter_id.as_str()),
            owner_name(state, owner),
            if *enabled { "on" } else { "off" }
        ),
        Edit::FilterSetSettings {
            owner, filter_id, ..
        } => format!(
            "Changed filter {} of {}",
            quote(filter_id.as_str()),
            owner_name(state, owner)
        ),
        Edit::FilterReorder {
            owner, filter_id, ..
        } => format!(
            "Reordered filter {} of {}",
            quote(filter_id.as_str()),
            owner_name(state, owner)
        ),
        Edit::OutputCreate { output_id, .. } => {
            format!("Created output {}", quote(output_id.as_str()))
        }
        Edit::OutputSetTarget { output_id, .. } => {
            format!("Changed where output {} sends", quote(output_id.as_str()))
        }
        Edit::OutputDelete { output_id } => format!("Deleted output {}", quote(output_id.as_str())),
        Edit::OutputSetEncoder { output_id, .. } => {
            format!(
                "Changed the encoder of output {}",
                quote(output_id.as_str())
            )
        }
        Edit::OutputSetVideoMix { output_id, .. } => {
            format!(
                "Changed the video mix of output {}",
                quote(output_id.as_str())
            )
        }
        Edit::CanvasSetSize { width, height } => format!("Set the canvas to {width}×{height}"),
        Edit::EncoderCreate { name, .. } => format!("Created encoder profile {}", quote(name)),
        Edit::EncoderUpdate { name, .. } => format!("Changed encoder profile {}", quote(name)),
        Edit::EncoderDelete { profile_id } => format!(
            "Deleted encoder profile {}",
            named(
                state.encoder(profile_id).map(|e| e.name()),
                profile_id.as_str()
            )
        ),
        Edit::VideoMixCreate { name, .. } => format!("Created video mix {}", quote(name)),
        Edit::VideoMixUpdate { name, .. } => format!("Changed video mix {}", quote(name)),
        Edit::VideoMixDelete { mix_id } => format!(
            "Deleted video mix {}",
            named(state.video_mix(mix_id).map(|m| m.name()), mix_id.as_str())
        ),
        Edit::FolderCreate { name, .. } => format!("Created folder {}", quote(name)),
        Edit::FolderRename { folder_id, name } => format!(
            "Renamed folder {} to {}",
            named(
                state.folder(folder_id).map(|f| f.name()),
                folder_id.as_str()
            ),
            quote(name)
        ),
        Edit::FolderSetParent { folder_id, .. } | Edit::FolderReorder { folder_id, .. } => {
            format!(
                "Moved folder {}",
                named(
                    state.folder(folder_id).map(|f| f.name()),
                    folder_id.as_str()
                )
            )
        }
        Edit::FolderDelete { folder_id } => format!(
            "Deleted folder {}",
            named(
                state.folder(folder_id).map(|f| f.name()),
                folder_id.as_str()
            )
        ),
        Edit::LabelCreate { name, .. } => format!("Created label {}", quote(name)),
        Edit::LabelUpdate { label_id, name, .. } => format!(
            "Changed label {}",
            named(state.label(label_id).map(|l| l.name()), name)
        ),
        Edit::LabelReorder { label_id, .. } => format!(
            "Moved label {}",
            named(state.label(label_id).map(|l| l.name()), label_id.as_str())
        ),
        Edit::LabelDelete { label_id } => format!(
            "Deleted label {}",
            named(state.label(label_id).map(|l| l.name()), label_id.as_str())
        ),
        Edit::HotkeySet { chord, action } => {
            format!("Bound {} to {}", chord.as_str(), humanize(action.name()))
        }
        Edit::HotkeyClear { chord } => format!("Cleared the hotkey {}", chord.as_str()),
        Edit::TransitionCreate { name, .. } => format!("Created transition {}", quote(name)),
        Edit::TransitionUpdate { preset_id, .. } => format!(
            "Changed transition {}",
            named(
                state.preset(preset_id).map(|p| p.name()),
                preset_id.as_str()
            )
        ),
        Edit::TransitionDelete { preset_id } => format!(
            "Deleted transition {}",
            named(
                state.preset(preset_id).map(|p| p.name()),
                preset_id.as_str()
            )
        ),
        Edit::TransitionSetDefault { preset_id } => format!(
            "Made {} the default transition",
            named(
                state.preset(preset_id).map(|p| p.name()),
                preset_id.as_str()
            )
        ),
        Edit::BusCreate { name, .. } => format!("Created bus {}", quote(name)),
        Edit::BusRename { bus_id, name } => format!(
            "Renamed bus {} to {}",
            named(state.bus(bus_id).map(|b| b.name()), bus_id.as_str()),
            quote(name)
        ),
        Edit::BusSetFader {
            bus_id,
            gain_db,
            muted,
        } => {
            let bus = state.bus(bus_id);
            let name = bus.map_or(bus_id.as_str(), |b| b.name());
            if let Some(gain) = gain_db {
                format!(
                    "{name} gain {}",
                    change(bus.map(|b| b.gain_db()), *gain, decibels, " dB")
                )
            } else if let Some(muted) = muted {
                format!(
                    "{} {}",
                    if *muted { "Muted" } else { "Unmuted" },
                    quote(name)
                )
            } else {
                format!("Changed the fader of {}", quote(name))
            }
        }
        Edit::BusSetMonitor { bus_id, .. } => format!(
            "Changed what {} plays to the monitor",
            named(state.bus(bus_id).map(|b| b.name()), bus_id.as_str())
        ),
        Edit::BusDelete { bus_id } => format!(
            "Deleted bus {}",
            named(state.bus(bus_id).map(|b| b.name()), bus_id.as_str())
        ),
        Edit::SendSet {
            source_id,
            bus_id,
            gain_db,
        } => {
            let before = state.send(source_id, bus_id);
            format!(
                "{} send to {} {}",
                source_plain(state, source_id),
                state.bus(bus_id).map_or(bus_id.as_str(), |b| b.name()),
                change(before, *gain_db, decibels, " dB")
            )
        }
        Edit::SendRemove { source_id, bus_id } => format!(
            "Stopped sending {} to {}",
            source(state, source_id),
            named(state.bus(bus_id).map(|b| b.name()), bus_id.as_str())
        ),
        other => capitalize(&humanize(other.name())),
    }
}

/// `before → after unit`, as the prototype writes it ("−1.5 → −3.0 dB"), or just
/// `after unit` when there was no value before or it didn't change.
fn change(
    before: Option<Finite32>,
    after: Finite32,
    number: impl Fn(f32) -> String,
    unit: &str,
) -> String {
    match before {
        Some(before) if before != after => {
            format!("{} → {}{unit}", number(before.get()), number(after.get()))
        }
        _ => format!("{}{unit}", number(after.get())),
    }
}

/// A gain to a tenth of a dB, with a true minus sign: `−3.0`.
fn decibels(db: f32) -> String {
    let text = format!("{db:.1}");
    match text.strip_prefix('-') {
        Some("0.0") => "0.0".to_owned(),
        Some(rest) => format!("−{rest}"),
        None => text,
    }
}

/// An opacity from 0 to 1 as a whole percentage, without the sign: `50`.
fn percent(opacity: f32) -> String {
    format!("{:.0}", opacity * 100.0)
}

/// `“name”`.
fn quote(name: &str) -> String {
    format!("“{name}”")
}

/// The quoted name, or the quoted ID when the object isn't known.
fn named(name: Option<&str>, id: &str) -> String {
    quote(name.unwrap_or(id))
}

fn scene(state: &State, scene_id: &SceneId) -> String {
    named(state.scene(scene_id).map(|s| s.name()), scene_id.as_str())
}

fn source(state: &State, source_id: &SourceId) -> String {
    quote(&source_plain(state, source_id))
}

/// The source's name unquoted, as a property line starts with it ("Mic gain").
fn source_plain(state: &State, source_id: &SourceId) -> String {
    state
        .source(source_id)
        .map_or_else(|| source_id.as_str().to_owned(), |s| s.name().to_owned())
}

/// An item, by the name of the source it shows.
fn item(state: &State, item_id: &ItemId) -> String {
    quote(&item_plain(state, item_id))
}

fn item_plain(state: &State, item_id: &ItemId) -> String {
    state
        .item(item_id)
        .map(|(_, item)| source_plain(state, item.source_id()))
        .unwrap_or_else(|| item_id.as_str().to_owned())
}

fn owner_name(state: &State, owner: &FilterOwner) -> String {
    match owner {
        FilterOwner::Source { source_id } => source(state, source_id),
        FilterOwner::Master => "the master output".to_owned(),
        FilterOwner::Output { output_id } => format!("output {}", quote(output_id.as_str())),
        FilterOwner::Scene { scene_id } => format!("scene {}", scene(state, scene_id)),
        FilterOwner::Item { item_id } => item(state, item_id),
        _ => "an object".to_owned(),
    }
}

/// A command name read as words: `source.set_audio` → `source set audio`.
fn humanize(name: &str) -> String {
    name.replace(['.', '_'], " ")
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::{Activity, change, cut, decibels, describe_gesture, percent};
    use crate::audit::AuditEntry;
    use jstream_protocol::{
        ClientId, Finite32, LiveOutcome, MAX_ACTIVITY_ENTRIES, MAX_ACTIVITY_SUMMARY_CHARS, Rev,
    };
    use std::time::{Duration, UNIX_EPOCH};

    fn entry(summary: &str) -> AuditEntry {
        AuditEntry {
            time: UNIX_EPOCH + Duration::from_millis(1_790_000_000_123),
            client_id: ClientId::new("client_2"),
            client_name: "claude-desktop".into(),
            request_id: None,
            call: "batch",
            edits: vec!["scene.rename"],
            output_id: None,
            scene_id: None,
            gesture_id: None,
            outcome: None,
            rev_before: Rev(1),
            rev_after: Rev(2),
            error: None,
            replayed: false,
            summary: summary.into(),
        }
    }

    fn db(value: f32) -> Finite32 {
        Finite32::new(value).expect("finite")
    }

    #[test]
    fn gains_read_with_a_true_minus_and_one_decimal() {
        assert_eq!(decibels(-3.0), "−3.0");
        assert_eq!(decibels(1.25), "1.2");
        assert_eq!(decibels(-0.01), "0.0");
        assert_eq!(
            change(Some(db(-1.5)), db(-3.0), decibels, " dB"),
            "−1.5 → −3.0 dB"
        );
        assert_eq!(change(Some(db(-3.0)), db(-3.0), decibels, " dB"), "−3.0 dB");
        assert_eq!(change(None, db(2.0), decibels, " dB"), "2.0 dB");
        assert_eq!(change(Some(db(1.0)), db(0.5), percent, "%"), "100 → 50%");
    }

    #[test]
    fn the_list_keeps_the_newest_entries_and_answers_newest_first() {
        let mut activity = Activity::default();
        let total = u64::from(MAX_ACTIVITY_ENTRIES) + 5;
        for n in 1..=total {
            let added = activity.push(&entry(&format!("change {n}")));
            assert_eq!(added.seq, n);
        }
        let all = activity.list(None, None);
        assert_eq!(all.len(), MAX_ACTIVITY_ENTRIES as usize);
        assert_eq!(all[0].seq, total);
        assert_eq!(all[0].time_ms, 1_790_000_000_123);
        assert_eq!(all.last().map(|e| e.seq), Some(6));
        let newer: Vec<u64> = activity
            .list(Some(total - 3), None)
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(newer, [total, total - 1, total - 2]);
        assert_eq!(activity.list(None, Some(2)).len(), 2);
        assert_eq!(
            activity.list(None, Some(10_000)).len(),
            MAX_ACTIVITY_ENTRIES as usize
        );
    }

    #[test]
    fn a_long_sentence_is_cut_to_the_limit() {
        let fits = "a".repeat(MAX_ACTIVITY_SUMMARY_CHARS);
        assert_eq!(cut(&fits), fits);
        let long = "é".repeat(MAX_ACTIVITY_SUMMARY_CHARS + 10);
        let kept = cut(&long);
        assert_eq!(kept.chars().count(), MAX_ACTIVITY_SUMMARY_CHARS);
        assert!(kept.ends_with('…'));
    }

    #[test]
    fn a_gesture_that_ends_without_a_commit_says_how() {
        assert_eq!(
            describe_gesture(LiveOutcome::Cancelled, &["bus.set_fader"]),
            "Cancelled a live change to bus set fader"
        );
        assert_eq!(
            describe_gesture(LiveOutcome::Expired, &["a", "b"]),
            "A live change ended with its session"
        );
    }
}
