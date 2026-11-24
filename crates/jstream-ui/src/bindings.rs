//! Keys and MIDI triggers, and the actions they run (ADR-0024 §5, ADR-0122, ADR-0208):
//! the model behind the trigger list, the action picker and Settings › Keys & MIDI.
//!
//! A trigger is a key chord or a MIDI note, control change or program change. Each runs
//! one or more actions, in the order they were bound. This module is pure: it lists the
//! collection's triggers with what they run ([`rows`]), describes them for people
//! ([`When::label`], [`describe`]), names every kind of action a person can bind
//! ([`Pick`], [`Pick::ALL`]) and builds the edits that add and remove them.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#keyboard

use crate::store::Collection;
use jstream_control::protocol::{
    Action, BusId, ChannelSwitch, Chord, Edit, Finite32, ItemId, MuteTo, OutputId, OutputKind,
    SceneId, SourceId, TimelineId, Trigger,
};

/// What fires a row: a key chord, or a MIDI trigger.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum When {
    /// A key chord, heard while JStream has the focus or, in the desktop app, anywhere.
    Key(Chord),
    /// A MIDI note, control change or program change, which the engine hears.
    Midi(Trigger),
}

impl When {
    /// How people read it: `Ctrl+1`, or `Note 36 · ch 10 · Pads`.
    pub fn label(&self) -> String {
        match self {
            When::Key(chord) => crate::hotkeys::label(chord),
            When::Midi(trigger) => midi_label(trigger),
        }
    }

    /// The edit that binds `action` to it, adding it after what it runs already.
    pub fn bind(&self, action: Action) -> Edit {
        match self {
            When::Key(chord) => Edit::HotkeySet {
                chord: chord.clone(),
                action,
            },
            When::Midi(trigger) => Edit::BindingSet {
                trigger: trigger.clone(),
                action,
            },
        }
    }

    /// The edit that unbinds `action` from it, or, with `None`, every action it runs.
    pub fn unbind(&self, action: Option<Action>) -> Edit {
        match self {
            When::Key(chord) => Edit::HotkeyClear {
                chord: chord.clone(),
                action,
            },
            When::Midi(trigger) => Edit::BindingClear {
                trigger: trigger.clone(),
                action,
            },
        }
    }

    /// Whether `filter`, lowercased, is in its label or what it runs.
    fn matches(&self, filter: &str) -> bool {
        self.label().to_lowercase().contains(filter)
    }
}

/// A MIDI trigger as people read it: its kind and number, then its channel and input
/// when it names them.
pub fn midi_label(trigger: &Trigger) -> String {
    let what = match trigger {
        Trigger::MidiNote { note, .. } => format!("Note {note}"),
        Trigger::MidiCc { controller, .. } => format!("CC {controller}"),
        Trigger::MidiProgram { program, .. } => format!("Program {program}"),
        // A live-data trigger (ADR-0202) says what it reads as the protocol writes it.
        Trigger::Data { .. } => return trigger.to_string(),
    };
    let mut parts = vec![what];
    parts.push(match trigger.channel() {
        Some(channel) => format!("ch {channel}"),
        None => "any channel".into(),
    });
    parts.push(trigger.input().unwrap_or("any input").to_owned());
    parts.join(" · ")
}

/// One trigger and every action it runs, in the order they run.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// What fires it.
    pub when: When,
    /// What it runs, in order: only the actions this version knows.
    pub does: Vec<Action>,
}

/// Every key and MIDI trigger in the collection with what it runs: keys first, in chord
/// order, then MIDI triggers in trigger order.
pub fn rows(collection: &Collection) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut push = |when: When, action: Option<Action>| {
        let Some(action) = action else {
            return;
        };
        match rows.last_mut() {
            Some(row) if row.when == when => row.does.push(action),
            _ => rows.push(Row {
                when,
                does: vec![action],
            }),
        }
    };
    for binding in &collection.hotkeys {
        push(When::Key(binding.chord.clone()), binding.action());
    }
    for binding in &collection.bindings {
        push(When::Midi(binding.trigger.clone()), binding.action());
    }
    rows
}

/// The rows whose trigger or actions mention `filter`, ignoring case; all of them for an
/// empty filter.
pub fn filtered(rows: Vec<Row>, collection: &Collection, filter: &str) -> Vec<Row> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|row| {
            row.when.matches(&filter)
                || row
                    .does
                    .iter()
                    .any(|a| describe(a, collection).to_lowercase().contains(&filter))
        })
        .collect()
}

/// What `action` does, as people read it: "Switch to Intro", "Start or stop Twitch".
pub fn describe(action: &Action, c: &Collection) -> String {
    let scene = |id: &SceneId| {
        c.scene(id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let item = |id: &ItemId| {
        c.item(id)
            .map(|(_, item)| c.source_name(&item.source_id))
            .unwrap_or_else(|| id.to_string())
    };
    let output = |id: &OutputId| output_name(c, id);
    let timeline = |id: &TimelineId| {
        c.timeline(id)
            .map(|(_, t)| t.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string())
    };
    match action {
        Action::SceneSwitch { scene_id, .. } => format!("Switch to {}", scene(scene_id)),
        Action::StudioSetPreview { scene_id } => format!("Preview {}", scene(scene_id)),
        Action::OutputStart { output_id } => format!("Start {}", output(output_id)),
        Action::OutputStop { output_id } => format!("Stop {}", output(output_id)),
        Action::OutputToggle { output_id } => format!("Start or stop {}", output(output_id)),
        Action::ItemShow { item_id, .. } => format!("Show {}", item(item_id)),
        Action::ItemHide { item_id, .. } => format!("Hide {}", item(item_id)),
        Action::ItemToggle { item_id, .. } => format!("Show or hide {}", item(item_id)),
        Action::ChannelSwitch { channel_id, to } => format!(
            "{} {}",
            match to {
                ChannelSwitch::On => "Switch on",
                ChannelSwitch::Off => "Switch off",
                ChannelSwitch::Toggle => "Switch on or off",
                ChannelSwitch::HoldOn => "Hold on",
                ChannelSwitch::HoldOff => "Hold off",
            },
            c.channel_name(channel_id)
        ),
        Action::ChannelSetValue {
            channel_id, value, ..
        } => format!(
            "Set {} to {}",
            c.channel_name(channel_id),
            percent(value.get())
        ),
        Action::ChannelNudge { channel_id, by, .. } => format!(
            "Nudge {} by {}{}",
            c.channel_name(channel_id),
            if by.get() >= 0.0 { "+" } else { "" },
            percent(by.get())
        ),
        Action::TimelinePlay { timeline_id, .. } => format!("Play {}", timeline(timeline_id)),
        Action::TimelineStop { timeline_id } => format!("Stop {}", timeline(timeline_id)),
        Action::SourceRestart { source_id } => format!("Restart {}", c.source_name(source_id)),
        Action::SceneZoomCamera { scene_id, steps } => format!(
            "Zoom {}'s camera {} {}",
            scene(scene_id),
            if *steps >= 0 { "in" } else { "out" },
            steps.unsigned_abs()
        ),
        Action::AudioResetLoudness {} => "Reset the loudness meter".into(),
        Action::SourceMute { source_id, to } => mute_words(*to, &c.source_name(source_id)),
        Action::BusMute { bus_id, to } => mute_words(*to, &bus_name(c, bus_id)),
        other => humanize(other.name()),
    }
}

/// A mute action on `name` as people read it (ADR-0207): "Mute Mic", "Push to talk on Mic".
fn mute_words(to: MuteTo, name: &str) -> String {
    match to {
        MuteTo::Muted => format!("Mute {name}"),
        MuteTo::Unmuted => format!("Unmute {name}"),
        MuteTo::Toggle => format!("Mute or unmute {name}"),
        MuteTo::PushToTalk => format!("Push to talk on {name}"),
        MuteTo::PushToMute => format!("Push to mute {name}"),
        _ => format!("Mute {name}"),
    }
}

/// A bus as people read it: its name, or its ID while it has none.
fn bus_name(c: &Collection, id: &BusId) -> String {
    c.buses
        .iter()
        .find(|b| &b.bus_id == id)
        .map_or_else(|| id.to_string(), |b| b.name.clone())
}

/// `value`, 0 to 1, as a percent: `0.5` is `50 %`.
fn percent(value: f32) -> String {
    format!("{} %", (value * 100.0).round())
}

/// A command name as words: `scene.set_keyframe` is `Scene set keyframe`.
fn humanize(name: &str) -> String {
    let words = name.replace(['.', '_'], " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// An output as people read it: its ID and what it is.
pub fn output_name(c: &Collection, id: &OutputId) -> String {
    match c.output(id).map(|o| &o.kind) {
        Some(OutputKind::Rtmp { .. }) => format!("stream {id}"),
        Some(OutputKind::Record { .. }) => format!("recording {id}"),
        Some(OutputKind::VirtualCam) => "the virtual camera".into(),
        _ => id.to_string(),
    }
}

/// A kind of action a person can bind, as the picker lists it. Each builds one or more
/// actions from what the picker's fields hold ([`Pick::actions`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Every stream output: start them all if any is stopped, else stop them.
    GoLive,
    /// Every recording output, toggled.
    Record,
    /// One output, started or stopped.
    OutputToggle,
    /// One output, started.
    OutputStart,
    /// One output, stopped.
    OutputStop,
    /// A scene to program.
    SceneSwitch,
    /// A scene to preview.
    ScenePreview,
    /// An item shown or hidden in turn.
    ItemToggle,
    /// An item shown.
    ItemShow,
    /// An item hidden.
    ItemHide,
    /// A channel switched on or off in turn.
    ChannelToggle,
    /// A channel switched on.
    ChannelOn,
    /// A channel switched off.
    ChannelOff,
    /// A channel set to a value, 0 to 100 %.
    ChannelSet,
    /// A channel moved by an amount, −100 to 100 %.
    ChannelNudge,
    /// A sequence played.
    TimelinePlay,
    /// A sequence stopped.
    TimelineStop,
    /// A source restarted: a browser page reloaded, a media file replayed.
    SourceRestart,
    /// The loudness meter restarted.
    ResetLoudness,
}

/// What a [`Pick`] needs chosen before it can build its action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// Nothing: it acts on the whole show.
    Nothing,
    /// An output.
    Output,
    /// A scene.
    Scene,
    /// An item.
    Item,
    /// A channel.
    Channel,
    /// A channel and a number, in percent.
    ChannelAmount,
    /// A sequence.
    Timeline,
    /// A source.
    Source,
}

impl Pick {
    /// Every pick, grouped as the picker lists them.
    pub const ALL: [Pick; 19] = [
        Pick::GoLive,
        Pick::Record,
        Pick::OutputToggle,
        Pick::OutputStart,
        Pick::OutputStop,
        Pick::SceneSwitch,
        Pick::ScenePreview,
        Pick::ItemToggle,
        Pick::ItemShow,
        Pick::ItemHide,
        Pick::ChannelToggle,
        Pick::ChannelOn,
        Pick::ChannelOff,
        Pick::ChannelSet,
        Pick::ChannelNudge,
        Pick::TimelinePlay,
        Pick::TimelineStop,
        Pick::SourceRestart,
        Pick::ResetLoudness,
    ];

    /// The group it is listed under.
    pub fn group(self) -> &'static str {
        match self {
            Pick::GoLive
            | Pick::Record
            | Pick::OutputToggle
            | Pick::OutputStart
            | Pick::OutputStop => "Go live and record",
            Pick::SceneSwitch | Pick::ScenePreview => "Scenes",
            Pick::ItemToggle | Pick::ItemShow | Pick::ItemHide => "Items",
            Pick::ChannelToggle
            | Pick::ChannelOn
            | Pick::ChannelOff
            | Pick::ChannelSet
            | Pick::ChannelNudge => "Channels",
            Pick::TimelinePlay | Pick::TimelineStop => "Sequences",
            Pick::SourceRestart | Pick::ResetLoudness => "Sources and audio",
        }
    }

    /// Its name in the picker.
    pub fn label(self) -> &'static str {
        match self {
            Pick::GoLive => "Go live / End stream",
            Pick::Record => "Record / Stop recording",
            Pick::OutputToggle => "Start or stop an output",
            Pick::OutputStart => "Start an output",
            Pick::OutputStop => "Stop an output",
            Pick::SceneSwitch => "Switch to a scene",
            Pick::ScenePreview => "Preview a scene",
            Pick::ItemToggle => "Show or hide an item",
            Pick::ItemShow => "Show an item",
            Pick::ItemHide => "Hide an item",
            Pick::ChannelToggle => "Switch a channel on or off",
            Pick::ChannelOn => "Switch a channel on",
            Pick::ChannelOff => "Switch a channel off",
            Pick::ChannelSet => "Set a channel to a value",
            Pick::ChannelNudge => "Nudge a channel up or down",
            Pick::TimelinePlay => "Play a sequence",
            Pick::TimelineStop => "Stop a sequence",
            Pick::SourceRestart => "Restart a source",
            Pick::ResetLoudness => "Reset the loudness meter",
        }
    }

    /// A stable key, for the picker's `<select>` and tests.
    pub fn key(self) -> &'static str {
        match self {
            Pick::GoLive => "go_live",
            Pick::Record => "record",
            Pick::OutputToggle => "output_toggle",
            Pick::OutputStart => "output_start",
            Pick::OutputStop => "output_stop",
            Pick::SceneSwitch => "scene_switch",
            Pick::ScenePreview => "scene_preview",
            Pick::ItemToggle => "item_toggle",
            Pick::ItemShow => "item_show",
            Pick::ItemHide => "item_hide",
            Pick::ChannelToggle => "channel_toggle",
            Pick::ChannelOn => "channel_on",
            Pick::ChannelOff => "channel_off",
            Pick::ChannelSet => "channel_set",
            Pick::ChannelNudge => "channel_nudge",
            Pick::TimelinePlay => "timeline_play",
            Pick::TimelineStop => "timeline_stop",
            Pick::SourceRestart => "source_restart",
            Pick::ResetLoudness => "reset_loudness",
        }
    }

    /// The pick with this key.
    pub fn from_key(key: &str) -> Option<Pick> {
        Pick::ALL.into_iter().find(|p| p.key() == key)
    }

    /// What it needs chosen.
    pub fn needs(self) -> Needs {
        match self {
            Pick::GoLive | Pick::Record | Pick::ResetLoudness => Needs::Nothing,
            Pick::OutputToggle | Pick::OutputStart | Pick::OutputStop => Needs::Output,
            Pick::SceneSwitch | Pick::ScenePreview => Needs::Scene,
            Pick::ItemToggle | Pick::ItemShow | Pick::ItemHide => Needs::Item,
            Pick::ChannelToggle | Pick::ChannelOn | Pick::ChannelOff => Needs::Channel,
            Pick::ChannelSet | Pick::ChannelNudge => Needs::ChannelAmount,
            Pick::TimelinePlay | Pick::TimelineStop => Needs::Timeline,
            Pick::SourceRestart => Needs::Source,
        }
    }

    /// The actions it runs with `target` chosen (an output, scene, item, channel,
    /// sequence or source ID, as [`Pick::needs`] says) and `amount` in percent, against
    /// `c`. Empty when something it needs is missing: no target, or, for Go live and
    /// Record, no output of that kind.
    pub fn actions(self, c: &Collection, target: &str, amount: f64) -> Vec<Action> {
        let one = |action: Action| vec![action];
        let target_ok = !target.is_empty();
        if self.needs() != Needs::Nothing && !target_ok {
            return Vec::new();
        }
        let channel_id = || target.to_owned();
        let fraction = |percent: f64| Finite32::new((percent / 100.0) as f32);
        match self {
            Pick::GoLive => toggles(c, |kind| matches!(kind, OutputKind::Rtmp { .. })),
            Pick::Record => toggles(c, |kind| matches!(kind, OutputKind::Record { .. })),
            Pick::OutputToggle => one(Action::OutputToggle {
                output_id: OutputId::new(target),
            }),
            Pick::OutputStart => one(Action::OutputStart {
                output_id: OutputId::new(target),
            }),
            Pick::OutputStop => one(Action::OutputStop {
                output_id: OutputId::new(target),
            }),
            Pick::SceneSwitch => one(crate::hotkeys::switch_to(&SceneId::new(target))),
            Pick::ScenePreview => one(Action::StudioSetPreview {
                scene_id: SceneId::new(target),
            }),
            Pick::ItemToggle => one(Action::ItemToggle {
                item_id: ItemId::new(target),
                scene_id: None,
            }),
            Pick::ItemShow => one(Action::ItemShow {
                item_id: ItemId::new(target),
                scene_id: None,
            }),
            Pick::ItemHide => one(Action::ItemHide {
                item_id: ItemId::new(target),
                scene_id: None,
            }),
            Pick::ChannelToggle | Pick::ChannelOn | Pick::ChannelOff => {
                one(Action::ChannelSwitch {
                    channel_id: channel_id(),
                    to: match self {
                        Pick::ChannelOn => ChannelSwitch::On,
                        Pick::ChannelOff => ChannelSwitch::Off,
                        _ => ChannelSwitch::Toggle,
                    },
                })
            }
            Pick::ChannelSet => fraction(amount.clamp(0.0, 100.0))
                .map(|value| {
                    one(Action::ChannelSetValue {
                        channel_id: channel_id(),
                        value,
                        fade_ms: None,
                    })
                })
                .unwrap_or_default(),
            Pick::ChannelNudge => fraction(amount.clamp(-100.0, 100.0))
                .filter(|by| by.get() != 0.0)
                .map(|by| {
                    one(Action::ChannelNudge {
                        channel_id: channel_id(),
                        by,
                        fade_ms: None,
                    })
                })
                .unwrap_or_default(),
            Pick::TimelinePlay => one(Action::TimelinePlay {
                timeline_id: TimelineId::new(target),
                delay_ms: None,
            }),
            Pick::TimelineStop => one(Action::TimelineStop {
                timeline_id: TimelineId::new(target),
            }),
            Pick::SourceRestart => one(Action::SourceRestart {
                source_id: SourceId::new(target),
            }),
            Pick::ResetLoudness => one(Action::AudioResetLoudness {}),
        }
    }
}

/// `output.toggle` of every output whose kind `kind` picks, in ID order.
fn toggles(c: &Collection, kind: impl Fn(&OutputKind) -> bool) -> Vec<Action> {
    c.outputs
        .iter()
        .filter(|o| kind(&o.kind))
        .map(|o| Action::OutputToggle {
            output_id: o.output_id.clone(),
        })
        .collect()
}

/// The choices for a pick's target in `c`: each as its ID and how people read it, in
/// the collection's order.
pub fn targets(c: &Collection, needs: Needs) -> Vec<(String, String)> {
    match needs {
        Needs::Nothing => Vec::new(),
        Needs::Output => c
            .outputs
            .iter()
            .map(|o| (o.output_id.to_string(), output_name(c, &o.output_id)))
            .collect(),
        Needs::Scene => c
            .listed_scenes()
            .map(|s| (s.scene_id.to_string(), s.name.clone()))
            .collect(),
        Needs::Item => c
            .listed_scenes()
            .flat_map(|scene| {
                scene.items.iter().map(move |item| {
                    (
                        item.item_id.to_string(),
                        format!("{} › {}", scene.name, c.source_name(&item.source_id)),
                    )
                })
            })
            .collect(),
        Needs::Channel | Needs::ChannelAmount => c
            .channels
            .iter()
            .map(|ch| (ch.channel_id.clone(), c.channel_name(&ch.channel_id)))
            .collect(),
        Needs::Timeline => c
            .scenes
            .iter()
            .flat_map(|scene| {
                scene.timelines.iter().map(move |t| {
                    let name = if t.name.is_empty() {
                        t.timeline_id.to_string()
                    } else {
                        t.name.clone()
                    };
                    (
                        t.timeline_id.to_string(),
                        format!("{} › {name}", scene.name),
                    )
                })
            })
            .collect(),
        Needs::Source => c
            .sources
            .iter()
            .map(|s| (s.source_id.to_string(), s.name.clone()))
            .collect(),
    }
}

/// The edits that add `actions` to `when`, leaving out any it runs already, so adding
/// the same thing twice changes nothing rather than failing.
pub fn add_edits(c: &Collection, when: &When, actions: Vec<Action>) -> Vec<Edit> {
    let has = match when {
        When::Key(chord) => c.hotkey_actions(chord),
        When::Midi(trigger) => c.trigger_actions(trigger),
    };
    let mut seen = has;
    let mut edits = Vec::new();
    for action in actions {
        if !seen.contains(&action) {
            seen.push(action.clone());
            edits.push(when.bind(action));
        }
    }
    edits
}

/// What `when` runs already, as a note for a person about to bind it: "Runs: Switch to
/// Intro, Start or stop stream yt", or `None` when it runs nothing.
pub fn in_use(c: &Collection, when: &When) -> Option<String> {
    let does = match when {
        When::Key(chord) => c.hotkey_actions(chord),
        When::Midi(trigger) => c.trigger_actions(trigger),
    };
    (!does.is_empty()).then(|| {
        format!(
            "Runs: {}",
            does.iter()
                .map(|a| describe(a, c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// The MIDI trigger a person typed: `kind` is `note`, `cc` or `program`, `number` 0 to
/// 127, `channel` 1 to 16 or 0 for any, and `input` blank for any. `None` when it isn't
/// a valid trigger.
pub fn midi_trigger(kind: &str, number: u32, channel: u32, input: &str) -> Option<Trigger> {
    let number = u8::try_from(number).ok().filter(|n| *n <= 127)?;
    let channel = match channel {
        0 => None,
        c => Some(u8::try_from(c).ok().filter(|c| (1..=16).contains(c))?),
    };
    let input = Some(input.trim().to_owned()).filter(|i| !i.is_empty());
    let trigger = match kind {
        "note" => Trigger::MidiNote {
            note: number,
            channel,
            input,
        },
        "cc" => Trigger::MidiCc {
            controller: number,
            channel,
            input,
        },
        "program" => Trigger::MidiProgram {
            program: number,
            channel,
            input,
        },
        _ => return None,
    };
    trigger.problem().is_none().then_some(trigger)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Binding, HotkeyBinding, OutputInfo, RtmpTarget};

    fn output(id: &str, kind: OutputKind) -> OutputInfo {
        serde_json::from_value(serde_json::json!({
            "output_id": id,
            "kind": serde_json::to_value(kind).expect("kind"),
        }))
        .expect("output")
    }

    fn rtmp() -> OutputKind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic {
                url: "rtmp://a.example/live".into(),
            },
        }
    }

    fn pad() -> Trigger {
        Trigger::MidiNote {
            note: 36,
            channel: Some(10),
            input: Some("Pads".into()),
        }
    }

    fn collection() -> Collection {
        let toggle = |id: &str| Action::OutputToggle {
            output_id: OutputId::new(id),
        };
        Collection {
            outputs: vec![output("yt", rtmp()), output("tw", rtmp())],
            hotkeys: vec![
                HotkeyBinding::new(Chord::new("F1"), &toggle("yt")),
                HotkeyBinding::new(Chord::new("F1"), &toggle("tw")),
                HotkeyBinding::new(Chord::new("F2"), &Action::AudioResetLoudness {}),
            ],
            bindings: vec![Binding::new(pad(), &toggle("yt"))],
            ..Collection::default()
        }
    }

    #[test]
    fn rows_group_every_action_of_a_trigger_in_order() {
        let c = collection();
        let rows = rows(&c);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].when, When::Key(Chord::new("F1")));
        assert_eq!(rows[0].does.len(), 2);
        assert_eq!(rows[2].when, When::Midi(pad()));
        assert_eq!(describe(&rows[0].does[1], &c), "Start or stop stream tw");
    }

    #[test]
    fn go_live_toggles_every_stream_output() {
        let c = collection();
        assert_eq!(
            Pick::GoLive.actions(&c, "", 0.0),
            vec![
                Action::OutputToggle {
                    output_id: OutputId::new("yt")
                },
                Action::OutputToggle {
                    output_id: OutputId::new("tw")
                },
            ]
        );
        // No recording output: Record has nothing to run.
        assert!(Pick::Record.actions(&c, "", 0.0).is_empty());
        // A pick that needs a target builds nothing without one.
        assert!(Pick::SceneSwitch.actions(&c, "", 0.0).is_empty());
    }

    #[test]
    fn channel_picks_take_percent() {
        let c = collection();
        assert_eq!(
            Pick::ChannelSet.actions(&c, "fog", 50.0),
            vec![Action::ChannelSetValue {
                channel_id: "fog".into(),
                value: Finite32::new(0.5).expect("finite"),
                fade_ms: None,
            }]
        );
        assert!(Pick::ChannelNudge.actions(&c, "fog", 0.0).is_empty());
        assert_eq!(
            describe(&Pick::ChannelNudge.actions(&c, "fog", -10.0)[0], &c),
            "Nudge fog by -10 %"
        );
    }

    #[test]
    fn a_mute_reads_as_what_it_does() {
        let c = collection();
        let mic = |to| Action::SourceMute {
            source_id: SourceId::new("mic"),
            to,
        };
        assert_eq!(
            describe(&mic(MuteTo::PushToTalk), &c),
            "Push to talk on mic"
        );
        assert_eq!(describe(&mic(MuteTo::Toggle), &c), "Mute or unmute mic");
        assert_eq!(
            describe(
                &Action::BusMute {
                    bus_id: BusId::master(),
                    to: MuteTo::PushToMute,
                },
                &c
            ),
            "Push to mute master"
        );
    }

    #[test]
    fn adding_leaves_out_what_a_trigger_runs_already() {
        let c = collection();
        let f1 = When::Key(Chord::new("F1"));
        let edits = add_edits(&c, &f1, Pick::GoLive.actions(&c, "", 0.0));
        assert!(edits.is_empty());
        let edits = add_edits(&c, &f1, vec![Action::AudioResetLoudness {}]);
        assert_eq!(
            edits,
            vec![Edit::HotkeySet {
                chord: Chord::new("F1"),
                action: Action::AudioResetLoudness {}
            }]
        );
        assert_eq!(
            in_use(&c, &f1).as_deref(),
            Some("Runs: Start or stop stream yt, Start or stop stream tw")
        );
        assert_eq!(in_use(&c, &When::Key(Chord::new("F9"))), None);
    }

    #[test]
    fn midi_triggers_read_and_parse() {
        assert_eq!(midi_label(&pad()), "Note 36 · ch 10 · Pads");
        assert_eq!(
            midi_trigger("cc", 64, 0, " "),
            Some(Trigger::MidiCc {
                controller: 64,
                channel: None,
                input: None
            })
        );
        assert_eq!(midi_trigger("note", 128, 0, ""), None);
        assert_eq!(midi_trigger("note", 1, 17, ""), None);
        assert_eq!(midi_trigger("pitch", 1, 1, ""), None);
    }

    #[test]
    fn every_pick_has_a_unique_key_and_round_trips() {
        for pick in Pick::ALL {
            assert_eq!(Pick::from_key(pick.key()), Some(pick));
        }
    }

    #[test]
    fn filtering_matches_the_trigger_or_what_it_runs() {
        let c = collection();
        let all = rows(&c);
        assert_eq!(filtered(all.clone(), &c, "loudness").len(), 1);
        assert_eq!(filtered(all.clone(), &c, "note 36").len(), 1);
        assert_eq!(filtered(all, &c, "").len(), 3);
    }
}
