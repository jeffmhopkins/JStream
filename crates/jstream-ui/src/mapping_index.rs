//! Every mapping in the collection in one place, for the Mappings panel
//! (`panels/mappings.rs`) and a channel's Drives list: each reader that follows a MIDI
//! control, a sender's live data or a channel, wherever it is held (a channel's inputs, a
//! value binding, an item's or a camera's motion), grouped by what drives it, and every
//! trigger that runs something (a key, a MIDI message, a sequence's starts and stops).
//!
//! It is pure: it reads a [`Collection`] and gives back rows and the [`Edit`] that writes
//! a changed reader back where it is held, so it is tested natively.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#mappings

use crate::store::Collection;
use jstream_control::protocol::{
    Action, CameraMotionModifier, CameraProperty, Chord, Edit, ItemId, MotionKind,
    MotionMidiControl, MotionModifier, MotionProperty, SceneId, TimelineId, TimelineTrigger,
    Trigger, ValueTarget,
};

/// Where a reader is held, so a change to it can be written back there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Holder {
    /// A channel's input, by its index in `inputs`.
    ChannelInput {
        /// The channel.
        channel_id: String,
        /// The input's index.
        index: usize,
    },
    /// A value binding.
    Binding {
        /// The binding.
        binding_id: String,
    },
    /// A motion modifier of an item, as `scene_id` shows it: the item's own scene, or a
    /// keyframe scene that overrides the item's motion.
    ItemMotion {
        /// The scene whose placement holds it.
        scene_id: SceneId,
        /// The item.
        item_id: ItemId,
        /// The modifier.
        modifier_id: String,
    },
    /// A motion modifier of a scene's camera.
    CameraMotion {
        /// The scene.
        scene_id: SceneId,
        /// The modifier.
        modifier_id: String,
    },
}

/// What a reader drives.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A channel, which reads it as an input.
    Channel(String),
    /// A setting, fader or send, through a value binding.
    Value(ValueTarget),
    /// A property of an item, as `scene_id` shows it.
    Item {
        /// The scene.
        scene_id: SceneId,
        /// The item.
        item_id: ItemId,
        /// The property.
        property: MotionProperty,
        /// The effect's or source's setting, for those properties.
        setting: Option<String>,
    },
    /// A property of a scene's camera.
    Camera {
        /// The scene.
        scene_id: SceneId,
        /// The property.
        property: CameraProperty,
    },
}

/// One reader: what it reads, what it drives, where it is held, and the channel that
/// scales it, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Reader {
    /// Where it is held.
    pub holder: Holder,
    /// What it reads, with its range, smoothing and curve. For a motion row that reads
    /// nothing (a wiggle scaled by a channel), what it does.
    pub input: MotionKind,
    /// What it drives.
    pub target: Target,
    /// The channel that scales it (`amount`), if any.
    pub amount: Option<String>,
}

impl Reader {
    /// The channel it follows, if it reads one.
    pub fn follows(&self) -> Option<&str> {
        match &self.input {
            MotionKind::Channel { channel_id, .. } => Some(channel_id),
            _ => None,
        }
    }

    /// Whether it reads a source: a MIDI control, live data or a channel.
    pub fn reads_source(&self) -> bool {
        source_key(&self.input).is_some()
    }
}

/// Every reader in the collection: the channels' inputs, the value bindings, then the
/// items' and the cameras' motion, scene by scene. A motion row counts when it reads a
/// source or a channel scales it. A keyframe scene's item motion counts only where it
/// differs from its parent's, so a row is listed once.
pub fn readers(collection: &Collection) -> Vec<Reader> {
    let mut out = Vec::new();
    for channel in &collection.channels {
        for (index, input) in channel.inputs.iter().enumerate() {
            out.push(Reader {
                holder: Holder::ChannelInput {
                    channel_id: channel.channel_id.clone(),
                    index,
                },
                input: input.clone(),
                target: Target::Channel(channel.channel_id.clone()),
                amount: None,
            });
        }
    }
    for binding in &collection.value_bindings {
        out.push(Reader {
            holder: Holder::Binding {
                binding_id: binding.binding_id.clone(),
            },
            input: binding.input.clone(),
            target: Target::Value(binding.target.clone()),
            amount: binding.amount.clone(),
        });
    }
    let counts =
        |kind: &MotionKind, amount: &Option<String>| source_key(kind).is_some() || amount.is_some();
    for scene in &collection.scenes {
        let parent = scene.variant_of.as_ref().and_then(|p| collection.scene(p));
        for item in &scene.items {
            let inherited: &[MotionModifier] = parent
                .and_then(|p| p.items.iter().find(|i| i.item_id == item.item_id))
                .map_or(&[], |i| &i.motion);
            for m in &item.motion {
                if !counts(&m.kind, &m.amount) || (parent.is_some() && inherited.contains(m)) {
                    continue;
                }
                let setting = m
                    .effect
                    .as_ref()
                    .map(|e| e.param.clone())
                    .or_else(|| m.source.as_ref().map(|s| s.param.clone()));
                out.push(Reader {
                    holder: Holder::ItemMotion {
                        scene_id: scene.scene_id.clone(),
                        item_id: item.item_id.clone(),
                        modifier_id: m.modifier_id.clone(),
                    },
                    input: m.kind.clone(),
                    target: Target::Item {
                        scene_id: scene.scene_id.clone(),
                        item_id: item.item_id.clone(),
                        property: m.property,
                        setting,
                    },
                    amount: m.amount.clone(),
                });
            }
        }
        for m in &scene.camera_motion {
            if !counts(&m.kind, &m.amount) {
                continue;
            }
            out.push(Reader {
                holder: Holder::CameraMotion {
                    scene_id: scene.scene_id.clone(),
                    modifier_id: m.modifier_id.clone(),
                },
                input: m.kind.clone(),
                target: Target::Camera {
                    scene_id: scene.scene_id.clone(),
                    property: m.property,
                },
                amount: m.amount.clone(),
            });
        }
    }
    out
}

/// The range a reader maps its source onto, `from` and `to`, for the kinds that read a
/// source.
pub fn range_of(kind: &MotionKind) -> Option<(f32, f32)> {
    match kind {
        MotionKind::Midi { from, to, .. }
        | MotionKind::Data { from, to, .. }
        | MotionKind::Channel { from, to, .. } => Some((from.get(), to.get())),
        _ => None,
    }
}

/// How far a reader moves its target across its source's whole travel, signed: `to`
/// minus `from`.
pub fn span(kind: &MotionKind) -> Option<f32> {
    range_of(kind).map(|(from, to)| to - from)
}

/// `kind` mapping onto `from..to` instead; `None` for a kind that reads no source or an
/// end that isn't finite.
pub fn with_range(kind: &MotionKind, from: f32, to: f32) -> Option<MotionKind> {
    let (from, to) = (
        jstream_control::protocol::Finite32::new(from)?,
        jstream_control::protocol::Finite32::new(to)?,
    );
    let mut next = kind.clone();
    match &mut next {
        MotionKind::Midi { from: f, to: t, .. }
        | MotionKind::Data { from: f, to: t, .. }
        | MotionKind::Channel { from: f, to: t, .. } => {
            *f = from;
            *t = to;
        }
        _ => return None,
    }
    Some(next)
}

/// How a reader bends its source, for the kinds that read one.
pub fn curve_of(kind: &MotionKind) -> Option<jstream_control::protocol::MotionCurve> {
    match kind {
        MotionKind::Midi { curve, .. }
        | MotionKind::Data { curve, .. }
        | MotionKind::Channel { curve, .. } => Some(*curve),
        _ => None,
    }
}

/// `kind` bending its source by `curve` instead.
pub fn with_curve(
    kind: &MotionKind,
    next: jstream_control::protocol::MotionCurve,
) -> Option<MotionKind> {
    let mut kind = kind.clone();
    match &mut kind {
        MotionKind::Midi { curve, .. }
        | MotionKind::Data { curve, .. }
        | MotionKind::Channel { curve, .. } => *curve = next,
        _ => return None,
    }
    Some(kind)
}

/// Whether a reader of `reader` hears source `heard`: the same source, where a reader
/// with no input or MIDI channel hears every one.
pub fn hears(reader: &SourceKey, heard: &SourceKey) -> bool {
    let any = |r: &Option<String>, h: &Option<String>| r.is_none() || r == h;
    match (reader, heard) {
        (
            SourceKey::Midi {
                control,
                number,
                channel,
                input,
            },
            SourceKey::Midi {
                control: c2,
                number: n2,
                channel: ch2,
                input: i2,
            },
        ) => {
            control == c2 && number == n2 && (channel.is_none() || channel == ch2) && any(input, i2)
        }
        (
            SourceKey::Data { input, key, field },
            SourceKey::Data {
                input: i2,
                key: k2,
                field: f2,
            },
        ) => key == k2 && field == f2 && any(input, i2),
        _ => reader == heard,
    }
}

/// What a source drives, as short names: the channels that read it and the targets
/// that read it directly, each once.
pub fn drives(collection: &Collection, heard: &SourceKey) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for reader in readers(collection) {
        let Some(key) = source_key(&reader.input) else {
            continue;
        };
        if !hears(&key, heard) {
            continue;
        }
        let name = target_name(collection, &reader.target).0;
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// A list of names as one short line: "Camera roll", "Camera roll +2", or `none`.
pub fn drives_line(names: &[String], none: &str) -> String {
    match names {
        [] => none.to_owned(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

/// Which source a reader reads, without its range, smoothing or curve: two readers with
/// the same key read the same control, field or channel.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceKey {
    /// A MIDI control: its kind, number, channel and input (`None` for any).
    Midi {
        /// "cc", "note", "bend" or "pressure".
        control: &'static str,
        /// The controller or note number.
        number: Option<u8>,
        /// The MIDI channel, 1 to 16.
        channel: Option<u8>,
        /// The input.
        input: Option<String>,
    },
    /// A field of a sender's live data.
    Data {
        /// The sender, `None` for any.
        input: Option<String>,
        /// The key.
        key: String,
        /// The field.
        field: String,
    },
    /// A channel.
    Channel(String),
}

impl SourceKey {
    /// The kind of source it is.
    pub fn kind(&self) -> SourceKind {
        match self {
            SourceKey::Midi { .. } => SourceKind::Midi,
            SourceKey::Data { .. } => SourceKind::Data,
            SourceKey::Channel(_) => SourceKind::Channel,
        }
    }
}

/// The source `kind` reads, or `None` for a motion that reads none (a wiggle, a drift).
pub fn source_key(kind: &MotionKind) -> Option<SourceKey> {
    Some(match kind {
        MotionKind::Midi {
            control,
            controller,
            channel,
            input,
            ..
        } => SourceKey::Midi {
            control: match control {
                MotionMidiControl::Cc => "cc",
                MotionMidiControl::Note => "note",
                MotionMidiControl::PitchBend => "bend",
                MotionMidiControl::Pressure => "pressure",
                _ => "other",
            },
            number: *controller,
            channel: *channel,
            input: input.clone(),
        },
        MotionKind::Data {
            input, key, field, ..
        } => SourceKey::Data {
            input: input.clone(),
            key: key.clone(),
            field: field.clone(),
        },
        MotionKind::Channel { channel_id, .. } => SourceKey::Channel(channel_id.clone()),
        _ => return None,
    })
}

/// The kinds of source the panel filters by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// Every kind.
    #[default]
    All,
    /// MIDI controls.
    Midi,
    /// A sender's live data, its Mods among it.
    Data,
    /// Channels.
    Channel,
    /// Keys.
    Key,
}

/// The JTunnel Mod a data source reads, 1 to 32: key `params`, field `values.N-1`
/// (ADR-0181).
pub fn mod_slot(key: &SourceKey) -> Option<u8> {
    let SourceKey::Data { key, field, .. } = key else {
        return None;
    };
    if key != "params" {
        return None;
    }
    let n: u8 = field.strip_prefix("values.")?.parse().ok()?;
    (n < 32).then_some(n + 1)
}

/// A source as a chip names it: "CC 21 · Pads", "Note 36", "Mod 20 · Bitwig",
/// "track/2 · volume · Bitwig", or a channel's name.
pub fn source_name(collection: &Collection, key: &SourceKey) -> String {
    let from = |input: &Option<String>| {
        input
            .as_ref()
            .map_or_else(String::new, |i| format!(" · {i}"))
    };
    match key {
        SourceKey::Midi {
            control,
            number,
            channel,
            input,
        } => {
            let what = match (*control, number) {
                ("cc", Some(n)) => format!("CC {n}"),
                ("note", Some(n)) => format!("Note {n}"),
                ("bend", _) => "Bend".into(),
                ("pressure", _) => "Pressure".into(),
                _ => "Control".into(),
            };
            let on = channel.map_or_else(String::new, |c| format!(" ch {c}"));
            let input = input
                .as_ref()
                .map_or(" · any input".to_owned(), |i| format!(" · {i}"));
            format!("{what}{on}{input}")
        }
        SourceKey::Data {
            input,
            key: k,
            field,
        } => match mod_slot(key) {
            Some(n) => format!("Mod {n}{}", from(input)),
            None if field.is_empty() => format!("{k}{}", from(input)),
            None => format!("{k} · {field}{}", from(input)),
        },
        SourceKey::Channel(id) => collection.channel_name(id),
    }
}

/// What a reader drives, as the table's Target column says it, and a note on how it
/// drives it ("item motion", "camera"), if any.
pub fn target_name(collection: &Collection, target: &Target) -> (String, Option<&'static str>) {
    let scene_name = |id: &SceneId| {
        collection
            .scene(id)
            .map_or_else(|| id.to_string(), |s| s.name.clone())
    };
    match target {
        Target::Channel(id) => (collection.channel_name(id), Some("channel")),
        Target::Value(value) => (
            crate::panels::channels::target_name(collection, value),
            None,
        ),
        Target::Item {
            scene_id,
            item_id,
            property,
            setting,
        } => {
            let item = collection.item_in(Some(scene_id), item_id).map_or_else(
                || item_id.to_string(),
                |(_, i)| collection.source_name(&i.source_id),
            );
            let what = match setting {
                Some(param) => param.clone(),
                None => crate::panels::motion::property_name(*property).to_owned(),
            };
            (format!("{item} · {what}"), Some("item motion"))
        }
        Target::Camera { scene_id, property } => (
            format!(
                "{} · camera · {}",
                scene_name(scene_id),
                crate::panels::camera_moves::property_name(*property)
            ),
            None,
        ),
    }
}

/// The scene a target belongs to, if it belongs to one: an item's or camera's scene, or
/// a channel's.
pub fn target_scene<'a>(collection: &'a Collection, target: &'a Target) -> Option<&'a SceneId> {
    match target {
        Target::Item { scene_id, .. } | Target::Camera { scene_id, .. } => Some(scene_id),
        Target::Channel(id) => collection.channel(id).and_then(|c| c.scene_id.as_ref()),
        Target::Value(_) => None,
    }
}

/// What sets a channel besides its inputs: a key or MIDI message that runs one of its
/// actions, or a sequence with a lane on it.
#[derive(Clone, Debug, PartialEq)]
pub enum Setter {
    /// A key chord.
    Key(Chord),
    /// A MIDI message.
    Midi(Trigger),
    /// A sequence with a lane on the channel.
    Sequence(TimelineId),
}

/// One row group of the Mappings table.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A channel: its inputs and the keys, MIDI and sequences that set it, then
    /// everything that follows it or that it scales.
    Channel {
        /// The channel.
        channel_id: String,
        /// Its inputs, as readers.
        inputs: Vec<Reader>,
        /// What else sets it.
        setters: Vec<Setter>,
        /// What follows it or is scaled by it.
        followers: Vec<Reader>,
    },
    /// A source read directly, with no channel between: every reader of it.
    Direct {
        /// The source.
        source: SourceKey,
        /// Its readers.
        readers: Vec<Reader>,
    },
}

/// The channel an action switches, sets or nudges.
pub fn action_channel(action: &Action) -> Option<&str> {
    match action {
        Action::ChannelSwitch { channel_id, .. }
        | Action::ChannelSetValue { channel_id, .. }
        | Action::ChannelNudge { channel_id, .. } => Some(channel_id),
        _ => None,
    }
}

/// The Mappings table: every channel (by name), then every source read directly (by
/// name), each with all its readers, so a second and third target sit under the first.
pub fn rows(collection: &Collection) -> Vec<Row> {
    let all = readers(collection);
    let mut channels: Vec<&jstream_control::protocol::ChannelInfo> =
        collection.channels.iter().collect();
    channels.sort_by_key(|c| collection.channel_name(&c.channel_id).to_lowercase());
    let mut out: Vec<Row> = channels
        .into_iter()
        .map(|channel| {
            let id = &channel.channel_id;
            let inputs = all
                .iter()
                .filter(|r| matches!(&r.holder, Holder::ChannelInput { channel_id, .. } if channel_id == id))
                .cloned()
                .collect();
            let mut setters: Vec<Setter> = collection
                .hotkeys
                .iter()
                .filter(|h| h.action().as_ref().and_then(action_channel) == Some(id.as_str()))
                .map(|h| Setter::Key(h.chord.clone()))
                .collect();
            setters.extend(
                collection
                    .bindings
                    .iter()
                    .filter(|b| b.action().as_ref().and_then(action_channel) == Some(id.as_str()))
                    .map(|b| Setter::Midi(b.trigger.clone())),
            );
            setters.extend(
                collection
                    .scenes
                    .iter()
                    .flat_map(|s| &s.timelines)
                    .filter(|t| t.lanes.iter().any(|l| &l.target.channel == id))
                    .map(|t| Setter::Sequence(t.timeline_id.clone())),
            );
            let followers = all
                .iter()
                .filter(|r| r.follows() == Some(id.as_str()) || r.amount.as_deref() == Some(id.as_str()))
                .cloned()
                .collect();
            Row::Channel {
                channel_id: id.clone(),
                inputs,
                setters,
                followers,
            }
        })
        .collect();
    let mut direct: Vec<(SourceKey, Vec<Reader>)> = Vec::new();
    for reader in &all {
        if matches!(reader.holder, Holder::ChannelInput { .. }) {
            continue;
        }
        let Some(key) = source_key(&reader.input).filter(|k| k.kind() != SourceKind::Channel)
        else {
            continue;
        };
        match direct.iter_mut().find(|(k, _)| *k == key) {
            Some((_, readers)) => readers.push(reader.clone()),
            None => direct.push((key, vec![reader.clone()])),
        }
    }
    direct.sort_by_key(|(k, _)| source_name(collection, k).to_lowercase());
    out.extend(
        direct
            .into_iter()
            .map(|(source, readers)| Row::Direct { source, readers }),
    );
    out
}

/// What the Mappings table shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Words a source, channel or target name must hold, any case.
    pub text: String,
    /// The scene a target or channel must be in: the program scene, for "On program
    /// now".
    pub scene: Option<SceneId>,
    /// The kind of source.
    pub kind: SourceKind,
    /// One source, as "find by moving a control" caught it.
    pub source: Option<SourceKey>,
}

/// Whether `row` passes `filter`.
pub fn shows(collection: &Collection, row: &Row, filter: &Filter) -> bool {
    let text = filter.text.trim().to_lowercase();
    let names = |readers: &[Reader]| -> Vec<String> {
        readers
            .iter()
            .flat_map(|r| {
                let mut names = vec![target_name(collection, &r.target).0];
                if let Some(k) = source_key(&r.input) {
                    names.push(source_name(collection, &k));
                }
                names
            })
            .collect()
    };
    let keys = |readers: &[Reader]| -> Vec<SourceKey> {
        readers
            .iter()
            .filter_map(|r| source_key(&r.input))
            .collect()
    };
    let in_scene = |readers: &[Reader], scene: &SceneId| {
        readers.iter().any(|r| drives_in(collection, r, scene))
    };
    match row {
        Row::Channel {
            channel_id,
            inputs,
            setters,
            followers,
        } => {
            let mut all_names = names(inputs);
            all_names.extend(names(followers));
            all_names.push(collection.channel_name(channel_id));
            let input_keys = keys(inputs);
            let has_key = setters.iter().any(|s| matches!(s, Setter::Key(_)));
            let has_midi = setters.iter().any(|s| matches!(s, Setter::Midi(_)));
            (text.is_empty() || all_names.iter().any(|n| n.to_lowercase().contains(&text)))
                && filter.scene.as_ref().is_none_or(|s| {
                    collection
                        .channel(channel_id)
                        .and_then(|c| c.scene_id.as_ref())
                        == Some(s)
                        || in_scene(followers, s)
                })
                && match filter.kind {
                    SourceKind::All | SourceKind::Channel => true,
                    SourceKind::Key => has_key,
                    SourceKind::Midi => {
                        has_midi || input_keys.iter().any(|k| k.kind() == SourceKind::Midi)
                    }
                    SourceKind::Data => input_keys.iter().any(|k| k.kind() == SourceKind::Data),
                }
                && filter
                    .source
                    .as_ref()
                    .is_none_or(|k| input_keys.contains(k))
        }
        Row::Direct { source, readers } => {
            let mut all_names = names(readers);
            all_names.push(source_name(collection, source));
            (text.is_empty() || all_names.iter().any(|n| n.to_lowercase().contains(&text)))
                && filter.scene.as_ref().is_none_or(|s| in_scene(readers, s))
                && (filter.kind == SourceKind::All || filter.kind == source.kind())
                && filter.source.as_ref().is_none_or(|k| k == source)
        }
    }
}

/// The edit that makes the reader at `holder` read `input` instead, keeping everything
/// else where it is held; `None` if it is gone.
pub fn write(collection: &Collection, holder: &Holder, input: MotionKind) -> Option<Edit> {
    write_all(collection, vec![(holder.clone(), input)])
        .ok()?
        .into_iter()
        .next()
}

/// The edits that make each reader read its new input, one per place that holds them
/// (one `channel.set` per channel, one `item.set_motion` per item), so several readers
/// in one list change together; `Err` with nothing sent if any of them is gone.
pub fn write_all(
    collection: &Collection,
    changes: Vec<(Holder, MotionKind)>,
) -> Result<Vec<Edit>, Holder> {
    let mut work = collection.clone();
    let mut touched: Vec<Holder> = Vec::new();
    for (holder, input) in changes {
        if put(&mut work, &holder, |kind| *kind = input.clone()).is_none() {
            return Err(holder);
        }
        let place = container(&holder);
        if !touched.iter().any(|t| container(t) == place) {
            touched.push(holder);
        }
    }
    Ok(touched
        .iter()
        .filter_map(|h| container_edit(&work, h))
        .collect())
}

/// The place that holds a reader, so readers in one place are written together.
fn container(holder: &Holder) -> Holder {
    match holder {
        Holder::ChannelInput { channel_id, .. } => Holder::ChannelInput {
            channel_id: channel_id.clone(),
            index: 0,
        },
        Holder::ItemMotion {
            scene_id, item_id, ..
        } => Holder::ItemMotion {
            scene_id: scene_id.clone(),
            item_id: item_id.clone(),
            modifier_id: String::new(),
        },
        Holder::CameraMotion { scene_id, .. } => Holder::CameraMotion {
            scene_id: scene_id.clone(),
            modifier_id: String::new(),
        },
        Holder::Binding { .. } => holder.clone(),
    }
}

/// Changes the reader at `holder` in `work` in place; `None` if it is gone.
fn put(work: &mut Collection, holder: &Holder, change: impl FnOnce(&mut MotionKind)) -> Option<()> {
    let kind = match holder {
        Holder::ChannelInput { channel_id, index } => work
            .channels
            .iter_mut()
            .find(|c| &c.channel_id == channel_id)?
            .inputs
            .get_mut(*index)?,
        Holder::Binding { binding_id } => {
            &mut work
                .value_bindings
                .iter_mut()
                .find(|b| &b.binding_id == binding_id)?
                .input
        }
        Holder::ItemMotion {
            scene_id,
            item_id,
            modifier_id,
        } => {
            &mut item_motion_mut(work, scene_id, item_id)?
                .iter_mut()
                .find(|m| &m.modifier_id == modifier_id)?
                .kind
        }
        Holder::CameraMotion {
            scene_id,
            modifier_id,
        } => {
            &mut work
                .scenes
                .iter_mut()
                .find(|s| &s.scene_id == scene_id)?
                .camera_motion
                .iter_mut()
                .find(|m| &m.modifier_id == modifier_id)?
                .kind
        }
    };
    change(kind);
    Some(())
}

fn item_motion_mut<'a>(
    work: &'a mut Collection,
    scene_id: &SceneId,
    item_id: &ItemId,
) -> Option<&'a mut Vec<MotionModifier>> {
    let scene = work.scenes.iter_mut().find(|s| &s.scene_id == scene_id)?;
    scene
        .items
        .iter_mut()
        .find(|i| &i.item_id == item_id)
        .map(|i| &mut i.motion)
}

/// The edit that writes the place holding `holder` as `work` has it.
fn container_edit(work: &Collection, holder: &Holder) -> Option<Edit> {
    Some(match holder {
        Holder::ChannelInput { channel_id, .. } => Edit::ChannelSet {
            channel: work.channel(channel_id)?.clone(),
        },
        Holder::Binding { binding_id } => Edit::BindingSetValue {
            binding: work
                .value_bindings
                .iter()
                .find(|b| &b.binding_id == binding_id)?
                .clone(),
        },
        Holder::ItemMotion {
            scene_id, item_id, ..
        } => Edit::ItemSetMotion {
            item_id: item_id.clone(),
            scene_id: Some(scene_id.clone()),
            motion: item_motion(work, scene_id, item_id)?,
        },
        Holder::CameraMotion { scene_id, .. } => Edit::SceneSetCameraMotion {
            scene_id: scene_id.clone(),
            motion: camera_motion(work, scene_id)?,
        },
    })
}

/// How a reader drives its target today: a motion row adds to the stored value, a value
/// binding replaces it, and a channel's input sets the channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Adds to the stored value.
    Offset,
    /// Replaces the value.
    Override,
    /// Sets a channel, as one of its inputs.
    Input,
}

/// How the reader at `holder` drives its target.
pub fn mode_of(holder: &Holder) -> Mode {
    match holder {
        Holder::ChannelInput { .. } => Mode::Input,
        Holder::Binding { .. } => Mode::Override,
        Holder::ItemMotion { .. } | Holder::CameraMotion { .. } => Mode::Offset,
    }
}

/// Whether the reader at `holder` applies, where it can be bypassed: a motion row's
/// `enabled`. `None` where it can't be.
pub fn enabled_of(collection: &Collection, holder: &Holder) -> Option<bool> {
    match holder {
        Holder::ItemMotion {
            scene_id,
            item_id,
            modifier_id,
        } => item_motion(collection, scene_id, item_id)?
            .iter()
            .find(|m| &m.modifier_id == modifier_id)
            .map(|m| m.enabled),
        Holder::CameraMotion {
            scene_id,
            modifier_id,
        } => collection
            .scene(scene_id)?
            .camera_motion
            .iter()
            .find(|m| &m.modifier_id == modifier_id)
            .map(|m| m.enabled),
        Holder::ChannelInput { .. } | Holder::Binding { .. } => None,
    }
}

/// The edit that bypasses the reader at `holder`, or stops bypassing it.
pub fn set_enabled(collection: &Collection, holder: &Holder, enabled: bool) -> Option<Edit> {
    let mut work = collection.clone();
    match holder {
        Holder::ItemMotion {
            scene_id,
            item_id,
            modifier_id,
        } => {
            item_motion_mut(&mut work, scene_id, item_id)?
                .iter_mut()
                .find(|m| &m.modifier_id == modifier_id)?
                .enabled = enabled;
        }
        Holder::CameraMotion {
            scene_id,
            modifier_id,
        } => {
            work.scenes
                .iter_mut()
                .find(|s| &s.scene_id == scene_id)?
                .camera_motion
                .iter_mut()
                .find(|m| &m.modifier_id == modifier_id)?
                .enabled = enabled;
        }
        Holder::ChannelInput { .. } | Holder::Binding { .. } => return None,
    }
    container_edit(&work, holder)
}

/// The unit a target's amount is in: the property's for an item or a camera, none for
/// a setting (whose range the binding names).
pub fn unit(target: &Target) -> &'static str {
    match target {
        Target::Item {
            property, setting, ..
        } => {
            if setting.is_some() {
                ""
            } else {
                crate::panels::motion::property_unit(*property)
            }
        }
        Target::Camera { property, .. } => crate::panels::camera_moves::unit(*property),
        Target::Channel(_) | Target::Value(_) => "",
    }
}

/// Whether a range is centred: it runs from minus to plus the same amount.
pub fn centred(from: f32, to: f32) -> bool {
    from != 0.0 && (from + to).abs() <= f32::EPSILON * from.abs().max(1.0) * 4.0
}

/// A reader's amount as the table says it: "+15°", "−8 px", "±20°" for a centred one,
/// or "0 → 6.5" for an override.
pub fn amount_text(kind: &MotionKind, mode: Mode, unit: &str) -> String {
    let Some((from, to)) = range_of(kind) else {
        return String::new();
    };
    let n = |v: f32| crate::panels::number::format_number(f64::from(v), 2, false);
    let space = if unit.is_empty() || unit == "°" {
        ""
    } else {
        " "
    };
    match mode {
        Mode::Override | Mode::Input => format!("{} → {}{space}{unit}", n(from), n(to)),
        Mode::Offset if centred(from, to) => format!("±{}{space}{unit}", n(to.abs())),
        Mode::Offset => {
            let d = to - from;
            let sign = if d < 0.0 { "−" } else { "+" };
            format!("{sign}{}{space}{unit}", n(d.abs()))
        }
    }
}

/// `kind` moving its target by `amount` across its source's travel: one-sided from where
/// it starts, or centred (`−amount` to `+amount`) when it was centred.
pub fn with_amount(kind: &MotionKind, amount: f32) -> Option<MotionKind> {
    let (from, to) = range_of(kind)?;
    if centred(from, to) {
        with_range(kind, -amount.abs(), amount.abs())
    } else {
        with_range(kind, from, from + amount)
    }
}

/// Whether `reader` drives something in `scene`: an item or camera there, a channel
/// that belongs to it, or a source's setting or fader where the scene shows the source.
pub fn drives_in(collection: &Collection, reader: &Reader, scene: &SceneId) -> bool {
    if target_scene(collection, &reader.target) == Some(scene) {
        return true;
    }
    let shows = |source: &jstream_control::protocol::SourceId| {
        collection
            .scene(scene)
            .is_some_and(|s| s.items.iter().any(|i| &i.source_id == source))
    };
    match &reader.target {
        Target::Value(ValueTarget::SourceSetting { source_id, .. })
        | Target::Value(ValueTarget::SourceGain { source_id })
        | Target::Value(ValueTarget::SendGain { source_id, .. }) => shows(source_id),
        _ => false,
    }
}

/// The Mod a channel is a name for: a channel whose only input reads one Mod of one
/// sender (or any), as naming a Mod in the Sources tab makes it.
pub fn named_mod(channel: &jstream_control::protocol::ChannelInfo) -> Option<SourceKey> {
    let [input] = channel.inputs.as_slice() else {
        return None;
    };
    let key = source_key(input)?;
    mod_slot(&key).map(|_| key)
}

/// The channel that names Mod `slot` of `sender`, if one does.
pub fn mod_channel<'a>(
    collection: &'a Collection,
    sender: &str,
    slot: u8,
) -> Option<&'a jstream_control::protocol::ChannelInfo> {
    collection.channels.iter().find(|c| {
        named_mod(c).is_some_and(|k| {
            mod_slot(&k) == Some(slot)
                && matches!(&k, SourceKey::Data { input, .. } if input.as_deref() == Some(sender))
        })
    })
}

/// The least smoothing a mapping that follows a channel keeps, in milliseconds: a
/// channel is resampled at about 60 a second out of step with the render clock, so a
/// mapping with none can judder on a slow sweep.
pub const MIN_CHANNEL_SMOOTHING_MS: u32 = 20;

/// The least smoothing the reader `kind` may be given: [`MIN_CHANNEL_SMOOTHING_MS`] for
/// one that follows a channel (not a channel's own input), else 0.
pub fn min_smoothing(holder: &Holder, kind: &MotionKind) -> u32 {
    match (holder, kind) {
        (Holder::ChannelInput { .. }, _) => 0,
        (_, MotionKind::Channel { .. }) => MIN_CHANNEL_SMOOTHING_MS,
        _ => 0,
    }
}

/// The longest Mod name, in bytes: what a DAW shows (ADR-0181).
pub const MAX_MOD_NAME: usize = 40;

/// `name` cut to [`MAX_MOD_NAME`] bytes at a character boundary, trimmed.
pub fn mod_name(name: &str) -> String {
    let name = name.trim();
    let mut end = name.len().min(MAX_MOD_NAME);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].trim_end().to_owned()
}

/// The edits that name Mod `slot` of `sender` `name`: a rename of the channel that
/// names it, or a new channel with the Mod as its only input (input smoothing 0) and
/// every mapping that read the Mod directly moved onto it, keeping its range and curve
/// and at least [`MIN_CHANNEL_SMOOTHING_MS`] of smoothing. Sent as one batch, so one undo
/// step. Empty when nothing would change.
pub fn name_mod_edits(
    collection: &Collection,
    sender: &str,
    slot: u8,
    name: &str,
    reader: MotionKind,
) -> Vec<Edit> {
    name_mods_edits(collection, sender, &[(slot, name.to_owned(), reader)])
}

/// [`name_mod_edits`] for several Mods of one sender at once, as a pasted list of names
/// gives them: each `(slot, name, reader)`, in one batch.
pub fn name_mods_edits(
    collection: &Collection,
    sender: &str,
    names: &[(u8, String, MotionKind)],
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut taken = collection.channels.clone();
    let mut moves: Vec<(Holder, MotionKind)> = Vec::new();
    let all = readers(collection);
    for (slot, name, reader) in names {
        let name = mod_name(name);
        if name.is_empty() {
            continue;
        }
        if let Some(channel) = mod_channel(collection, sender, *slot) {
            if channel.name != name {
                let mut channel = channel.clone();
                channel.name = name;
                edits.push(Edit::ChannelSet { channel });
            }
            continue;
        }
        let mut channel = crate::panels::channels::new_channel(&name, None, 0.0, &taken);
        let mut input = reader.clone();
        crate::panels::learn::set_smoothing(&mut input, 0);
        channel.inputs.push(input);
        let heard = SourceKey::Data {
            input: Some(sender.to_owned()),
            key: "params".into(),
            field: format!("values.{}", slot.saturating_sub(1)),
        };
        moves.extend(
            all.iter()
                .filter(|r| !matches!(r.holder, Holder::ChannelInput { .. }))
                .filter(|r| source_key(&r.input).is_some_and(|k| hears(&k, &heard)))
                .filter_map(|r| {
                    let (from, to) = range_of(&r.input)?;
                    let next = MotionKind::Channel {
                        channel_id: channel.channel_id.clone(),
                        smoothing_ms: crate::panels::learn::smoothing_of(&r.input)
                            .unwrap_or(0)
                            .max(MIN_CHANNEL_SMOOTHING_MS),
                        curve: curve_of(&r.input).unwrap_or_default(),
                        from: jstream_control::protocol::Finite32::new(from)?,
                        to: jstream_control::protocol::Finite32::new(to)?,
                    };
                    Some((r.holder.clone(), next))
                }),
        );
        taken.push(channel.clone());
        edits.push(Edit::ChannelSet { channel });
    }
    edits.extend(write_all(collection, moves).unwrap_or_default());
    edits
}

/// The edit that removes the reader at `holder`; `None` if it is gone.
pub fn remove(collection: &Collection, holder: &Holder) -> Option<Edit> {
    match holder {
        Holder::ChannelInput { channel_id, index } => {
            let mut channel = collection.channel(channel_id)?.clone();
            (*index < channel.inputs.len()).then(|| channel.inputs.remove(*index))?;
            Some(Edit::ChannelSet { channel })
        }
        Holder::Binding { binding_id } => collection
            .value_bindings
            .iter()
            .any(|b| &b.binding_id == binding_id)
            .then(|| Edit::BindingClearValue {
                binding_id: binding_id.clone(),
            }),
        Holder::ItemMotion {
            scene_id,
            item_id,
            modifier_id,
        } => {
            let mut motion = item_motion(collection, scene_id, item_id)?;
            let n = motion.iter().position(|m| &m.modifier_id == modifier_id)?;
            motion.remove(n);
            Some(Edit::ItemSetMotion {
                item_id: item_id.clone(),
                scene_id: Some(scene_id.clone()),
                motion,
            })
        }
        Holder::CameraMotion {
            scene_id,
            modifier_id,
        } => {
            let mut motion = camera_motion(collection, scene_id)?;
            let n = motion.iter().position(|m| &m.modifier_id == modifier_id)?;
            motion.remove(n);
            Some(Edit::SceneSetCameraMotion {
                scene_id: scene_id.clone(),
                motion,
            })
        }
    }
}

/// The reader held at `holder` now, if it is still there.
pub fn reader_at(collection: &Collection, holder: &Holder) -> Option<Reader> {
    readers(collection)
        .into_iter()
        .find(|r| &r.holder == holder)
}

fn item_motion(
    collection: &Collection,
    scene_id: &SceneId,
    item_id: &ItemId,
) -> Option<Vec<MotionModifier>> {
    collection
        .item_in(Some(scene_id), item_id)
        .map(|(_, item)| item.motion.clone())
}

fn camera_motion(collection: &Collection, scene_id: &SceneId) -> Option<Vec<CameraMotionModifier>> {
    collection.scene(scene_id).map(|s| s.camera_motion.clone())
}

/// What fires a row of the Triggers tab.
#[derive(Clone, Debug, PartialEq)]
pub enum When {
    /// A key chord.
    Key(Chord),
    /// A MIDI message.
    Midi(Trigger),
    /// One of a sequence's starts or stops, by its index there.
    Sequence {
        /// The sequence.
        timeline_id: TimelineId,
        /// Whether it is one of its stops.
        stops: bool,
        /// Its index in `starts` or `stops`.
        index: usize,
        /// The trigger.
        trigger: TimelineTrigger,
    },
}

/// One row of the Triggers tab: what fires it and what it does.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerRow {
    /// What fires it.
    pub when: When,
    /// What it does: the action a key or MIDI message runs, or the sequence's play or
    /// stop. `None` for an action this version doesn't know.
    pub does: Option<Action>,
}

/// Every trigger: the keys (by chord), the MIDI bindings (by trigger), then each
/// sequence's starts and stops.
pub fn triggers(collection: &Collection) -> Vec<TriggerRow> {
    let mut out: Vec<TriggerRow> = collection
        .hotkeys
        .iter()
        .map(|h| TriggerRow {
            when: When::Key(h.chord.clone()),
            does: h.action(),
        })
        .collect();
    out.extend(collection.bindings.iter().map(|b| TriggerRow {
        when: When::Midi(b.trigger.clone()),
        does: b.action(),
    }));
    for t in collection.scenes.iter().flat_map(|s| &s.timelines) {
        for (stops, list) in [(false, &t.starts), (true, &t.stops)] {
            for (index, trigger) in list.iter().enumerate() {
                let timeline_id = t.timeline_id.clone();
                out.push(TriggerRow {
                    when: When::Sequence {
                        timeline_id: timeline_id.clone(),
                        stops,
                        index,
                        trigger: trigger.clone(),
                    },
                    does: Some(if stops {
                        Action::TimelineStop { timeline_id }
                    } else {
                        Action::TimelinePlay {
                            timeline_id,
                            delay_ms: None,
                        }
                    }),
                });
            }
        }
    }
    out
}

/// A MIDI trigger as a chip says it: "Note 36 · Pads", "CC 64 ch 10 · any input".
pub fn midi_text(trigger: &Trigger) -> String {
    let (what, n) = match trigger {
        Trigger::MidiNote { note, .. } => ("Note", note),
        Trigger::MidiCc { controller, .. } => ("CC", controller),
        Trigger::MidiProgram { program, .. } => ("Program", program),
    };
    let on = trigger
        .channel()
        .map_or_else(String::new, |c| format!(" ch {c}"));
    let input = trigger
        .input()
        .map_or(" · any input".to_owned(), |i| format!(" · {i}"));
    format!("{what} {n}{on}{input}")
}

/// What fires a trigger row, as the When column says it.
pub fn when_text(collection: &Collection, when: &When) -> String {
    match when {
        When::Key(chord) => format!("Key {}", crate::hotkeys::label(chord)),
        When::Midi(trigger) => midi_text(trigger),
        When::Sequence { trigger, .. } => {
            crate::panels::sequence::trigger_text(collection, trigger)
        }
    }
}

/// The edit that removes a trigger row; `None` if it is gone.
pub fn remove_trigger(collection: &Collection, when: &When) -> Option<Edit> {
    match when {
        When::Key(chord) => collection
            .hotkeys
            .iter()
            .any(|h| &h.chord == chord)
            .then(|| Edit::HotkeyClear {
                chord: chord.clone(),
            }),
        When::Midi(trigger) => collection
            .bindings
            .iter()
            .any(|b| &b.trigger == trigger)
            .then(|| Edit::BindingClear {
                trigger: trigger.clone(),
            }),
        When::Sequence {
            timeline_id,
            stops,
            index,
            trigger,
        } => {
            let (scene, t) = collection.timeline(timeline_id)?;
            let mut timeline = t.clone();
            let list = if *stops {
                &mut timeline.stops
            } else {
                &mut timeline.starts
            };
            // Only the trigger the row was drawn from: another client may have changed
            // the list since.
            (list.get(*index) == Some(trigger)).then(|| list.remove(*index))?;
            Some(Edit::TimelineSet {
                scene_id: scene.scene_id.clone(),
                timeline,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{
        ChannelInfo, Finite32, HotkeyBinding, MotionCurve, SourceId, ValueBinding,
    };

    fn cc(n: u8, input: Option<&str>) -> MotionKind {
        MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(n),
            channel: None,
            input: input.map(str::to_owned),
            smoothing_ms: 50,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        }
    }

    fn follow(id: &str) -> MotionKind {
        MotionKind::Channel {
            channel_id: id.into(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        }
    }

    fn fader(id: &str, source: &str, input: MotionKind) -> ValueBinding {
        ValueBinding {
            binding_id: id.into(),
            input,
            target: ValueTarget::SourceGain {
                source_id: SourceId::new(source),
            },
            amount: None,
        }
    }

    fn studio() -> Collection {
        let mut key = ChannelInfo::new("key");
        key.name = "Key light".into();
        key.inputs = vec![cc(21, Some("Pads"))];
        Collection {
            channels: vec![key],
            value_bindings: vec![
                fader("a", "mic", follow("key")),
                fader("b", "music", cc(7, None)),
                fader("c", "drums", cc(7, None)),
            ],
            hotkeys: vec![HotkeyBinding::new(
                Chord::new("F5"),
                &Action::ChannelSwitch {
                    channel_id: "key".into(),
                    to: jstream_control::protocol::ChannelSwitch::Toggle,
                },
            )],
            ..Collection::default()
        }
    }

    #[test]
    fn every_reader_is_listed_where_it_is_held() {
        let c = studio();
        let all = readers(&c);
        assert_eq!(all.len(), 4);
        assert_eq!(
            all[0].holder,
            Holder::ChannelInput {
                channel_id: "key".into(),
                index: 0
            }
        );
        assert_eq!(all[1].follows(), Some("key"));
        assert!(all.iter().all(Reader::reads_source));
    }

    #[test]
    fn rows_group_a_channel_and_share_a_source_between_targets() {
        let c = studio();
        let rows = rows(&c);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        let Row::Channel {
            inputs,
            setters,
            followers,
            ..
        } = &rows[0]
        else {
            panic!("the channel first");
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(setters, &[Setter::Key(Chord::new("F5"))]);
        assert_eq!(followers.len(), 1);
        let Row::Direct { source, readers } = &rows[1] else {
            panic!("then the direct source");
        };
        assert_eq!(source_name(&c, source), "CC 7 · any input");
        assert_eq!(readers.len(), 2, "one CC drives two faders");
    }

    #[test]
    fn a_filter_narrows_by_text_kind_and_source() {
        let c = studio();
        let rows = rows(&c);
        let only = |f: Filter| rows.iter().filter(|r| shows(&c, r, &f)).count();
        assert_eq!(only(Filter::default()), 2);
        assert_eq!(
            only(Filter {
                text: "key light".into(),
                ..Filter::default()
            }),
            1
        );
        assert_eq!(
            only(Filter {
                kind: SourceKind::Key,
                ..Filter::default()
            }),
            1
        );
        assert_eq!(
            only(Filter {
                source: source_key(&cc(21, Some("Pads"))),
                ..Filter::default()
            }),
            1,
            "moving CC 21 finds the channel it drives"
        );
    }

    #[test]
    fn a_changed_reader_is_written_back_and_removed_where_it_is_held() {
        let c = studio();
        let held = Holder::ChannelInput {
            channel_id: "key".into(),
            index: 0,
        };
        let Some(Edit::ChannelSet { channel }) = write(&c, &held, cc(22, None)) else {
            panic!("a channel.set");
        };
        assert_eq!(channel.inputs, vec![cc(22, None)]);
        let Some(Edit::ChannelSet { channel }) = remove(&c, &held) else {
            panic!("a channel.set");
        };
        assert!(channel.inputs.is_empty());
        let binding = Holder::Binding {
            binding_id: "b".into(),
        };
        assert_eq!(
            remove(&c, &binding),
            Some(Edit::BindingClearValue {
                binding_id: "b".into()
            })
        );
        assert_eq!(
            remove(
                &c,
                &Holder::Binding {
                    binding_id: "gone".into()
                }
            ),
            None
        );
    }

    #[test]
    fn a_source_lists_what_it_drives_and_any_input_hears_every_input() {
        let c = studio();
        let pads = source_key(&cc(21, Some("Pads"))).expect("a source");
        assert_eq!(drives(&c, &pads), vec!["Key light".to_owned()]);
        let seven = source_key(&cc(7, Some("Keys"))).expect("a source");
        assert_eq!(
            drives(&c, &seven).len(),
            2,
            "a reader of any input hears Keys"
        );
        assert_eq!(
            drives_line(&drives(&c, &seven), "free"),
            "music · volume +1"
        );
        assert_eq!(drives_line(&[], "free"), "free");
        let other = source_key(&cc(21, Some("Keys"))).expect("a source");
        assert!(drives(&c, &other).is_empty(), "the channel reads Pads only");
    }

    #[test]
    fn a_range_and_curve_are_read_and_replaced() {
        let k = cc(1, None);
        assert_eq!(range_of(&k), Some((0.0, 1.0)));
        let next = with_range(&k, -15.0, 15.0).expect("a range");
        assert_eq!(span(&next), Some(30.0));
        assert_eq!(with_range(&k, f32::NAN, 1.0), None);
        let eased = with_curve(&next, MotionCurve::EaseOut).expect("a curve");
        assert_eq!(curve_of(&eased), Some(MotionCurve::EaseOut));
    }

    fn mod_in(sender: Option<&str>, slot: u8) -> MotionKind {
        MotionKind::Data {
            input: sender.map(str::to_owned),
            key: "params".into(),
            field: format!("values.{}", slot - 1),
            range: [Finite32::ZERO, Finite32::ONE],
            states: Default::default(),
            smoothing_ms: 80,
            curve: MotionCurve::EaseOut,
            from: Finite32::ZERO,
            to: Finite32::new(4.0).expect("finite"),
            color_from: None,
            palette: Default::default(),
        }
    }

    #[test]
    fn naming_a_mod_makes_its_channel_and_moves_its_mappings_onto_it() {
        let mut c = studio();
        let mut quick = mod_in(None, 20);
        crate::panels::learn::set_smoothing(&mut quick, 0);
        c.value_bindings.push(fader("fast", "lamp", quick));
        c.value_bindings
            .push(fader("glow", "bars", mod_in(None, 20)));
        c.value_bindings
            .push(fader("other", "bars", mod_in(Some("Else"), 20)));
        let edits = name_mod_edits(&c, "DAW", 20, "  Camera roll ", mod_in(Some("DAW"), 20));
        assert_eq!(edits.len(), 3, "{edits:#?}");
        let Edit::ChannelSet { channel } = &edits[0] else {
            panic!("the channel first");
        };
        assert_eq!(channel.name, "Camera roll");
        assert_eq!(
            crate::panels::learn::smoothing_of(&channel.inputs[0]),
            Some(0)
        );
        let Edit::BindingSetValue { binding: fast } = &edits[1] else {
            panic!("then the mappings it takes over");
        };
        assert_eq!(
            crate::panels::learn::smoothing_of(&fast.input),
            Some(MIN_CHANNEL_SMOOTHING_MS),
            "a mapping through a channel keeps some smoothing"
        );
        let Edit::BindingSetValue { binding } = &edits[2] else {
            panic!("then the mappings it takes over");
        };
        assert_eq!(binding.binding_id, "glow");
        assert_eq!(
            binding.input,
            MotionKind::Channel {
                channel_id: "camera_roll".into(),
                smoothing_ms: 80,
                curve: MotionCurve::EaseOut,
                from: Finite32::ZERO,
                to: Finite32::new(4.0).expect("finite"),
            }
        );
        c.channels.push(channel.clone());
        assert_eq!(
            mod_channel(&c, "DAW", 20).map(|c| c.channel_id.as_str()),
            Some("camera_roll")
        );
        assert!(name_mod_edits(&c, "DAW", 20, "Camera roll", mod_in(Some("DAW"), 20)).is_empty());
        let renamed = name_mod_edits(&c, "DAW", 20, "Roll", mod_in(Some("DAW"), 20));
        assert!(matches!(&renamed[..], [Edit::ChannelSet { channel }] if channel.name == "Roll"));
        assert_eq!(
            mod_name(&"é".repeat(30)).len(),
            40,
            "cut at a character boundary"
        );
    }

    #[test]
    fn amounts_read_one_sided_centred_or_as_a_range() {
        let k = |from: f32, to: f32| with_range(&cc(1, None), from, to).expect("a range");
        assert_eq!(amount_text(&k(0.0, 15.0), Mode::Offset, "°"), "+15°");
        assert_eq!(amount_text(&k(0.0, -8.0), Mode::Offset, "px"), "−8 px");
        assert_eq!(amount_text(&k(-20.0, 20.0), Mode::Offset, "°"), "±20°");
        assert_eq!(amount_text(&k(0.0, 6.5), Mode::Override, ""), "0 → 6.5");
        assert_eq!(
            range_of(&with_amount(&k(-20.0, 20.0), 5.0).expect("centred")),
            Some((-5.0, 5.0))
        );
        assert_eq!(
            range_of(&with_amount(&k(1.0, 2.0), -3.0).expect("one-sided")),
            Some((1.0, -2.0))
        );
    }

    #[test]
    fn mods_are_named_by_slot() {
        let c = Collection::default();
        let m = SourceKey::Data {
            input: Some("Bitwig".into()),
            key: "params".into(),
            field: "values.19".into(),
        };
        assert_eq!(mod_slot(&m), Some(20));
        assert_eq!(source_name(&c, &m), "Mod 20 · Bitwig");
        let other = SourceKey::Data {
            input: None,
            key: "track/2".into(),
            field: "volume".into(),
        };
        assert_eq!(source_name(&c, &other), "track/2 · volume");
    }

    #[test]
    fn triggers_list_keys_midi_and_sequences() {
        let c = studio();
        let rows = triggers(&c);
        assert_eq!(rows.len(), 1);
        assert_eq!(when_text(&c, &rows[0].when), "Key F5");
        assert_eq!(
            remove_trigger(&c, &rows[0].when),
            Some(Edit::HotkeyClear {
                chord: Chord::new("F5")
            })
        );
        assert_eq!(
            midi_text(&Trigger::MidiNote {
                note: 36,
                channel: Some(10),
                input: Some("Pads".into())
            }),
            "Note 36 ch 10 · Pads"
        );
    }
}
