//! Audio in the collection (ADR-0028): each audio source's settings, the buses, and the
//! sends from sources into buses. The `source.set_audio`, `bus.*` and `send.*` edits,
//! and what `source.create`, `source.duplicate` and `source.delete` do to a source's
//! audio.
//!
//! Buses never feed other buses (ADR-0028), and a send always runs from a source into a
//! bus, so the audio graph can't have a cycle. Keys, which can close one, come with the
//! filters.

use crate::apply::{already_exists, check_id, check_name, invalid, not_found};
use crate::model::{Bus, MAX_BUSES, State};
use jstream_protocol::{
    AudioFollow, BusId, Edit, EntityKind, Finite32, KindDescriptor, ProtocolError, SourceAudio,
    SourceId, SourceKind, gain_db_problem,
};
use std::collections::BTreeMap;

/// The audio settings a new source of `kind` starts with, or `None` when it has no
/// audio. A `scene` source follows `when_shown`. A builtin kind with audio follows
/// `always` when it has no video (an audio input), else `when_shown` (ADR-0028's defaults
/// by kind: browser, media and webcam audio follow the program).
pub(crate) fn default_audio(
    kind: &SourceKind,
    kinds: &BTreeMap<String, KindDescriptor>,
) -> Option<SourceAudio> {
    match kind {
        SourceKind::Scene { .. } => Some(SourceAudio::new(AudioFollow::WhenShown)),
        SourceKind::Builtin { kind } => {
            let descriptor = kinds.get(kind).filter(|d| d.audio)?;
            Some(SourceAudio::new(if descriptor.video {
                AudioFollow::WhenShown
            } else {
                AudioFollow::Always
            }))
        }
        _ => None,
    }
}

/// Whether a source of `kind` with `audio` has a channel of its own, and so sends: an
/// audio source that is not a `scene` source.
pub(crate) fn has_channel(kind: &SourceKind, audio: Option<&SourceAudio>) -> bool {
    audio.is_some() && !matches!(kind, SourceKind::Scene { .. })
}

/// Gives the new source `id` its master send at 0 dB if it has a channel (ADR-0028).
/// Its audio settings are already in place.
pub(crate) fn created(state: &mut State, id: &SourceId) {
    if let Some(source) = state.sources.get(id)
        && has_channel(&source.kind, source.audio.as_ref())
    {
        state
            .sends
            .insert((BusId::master(), id.clone()), Finite32::ZERO);
    }
}

/// Gives the copy `to` the audio settings and sends of `from`, replacing what
/// `source.create` gave it. The copy's inverse is its `source.delete`, which takes them
/// away again.
pub(crate) fn copied(state: &mut State, from: &SourceId, to: &SourceId) {
    let audio = state.sources.get(from).and_then(|s| s.audio);
    if let Some(copy) = state.sources.get_mut(to) {
        copy.audio = audio;
    }
    state.sends.retain(|(_, source), _| source != to);
    let sends: Vec<(BusId, Finite32)> = state
        .sends
        .iter()
        .filter(|((_, source), _)| source == from)
        .map(|((bus, _), gain)| (bus.clone(), *gain))
        .collect();
    for (bus, gain) in sends {
        state.sends.insert((bus, to.clone()), gain);
    }
}

/// Takes away the sends of the source `id`, which `source.delete` has just removed
/// (with its `kind` and `audio`), and returns the edits that restore its audio after
/// the `source.create` that undoes the delete: that create gives it its kind's default
/// audio and a master send at 0 dB, so only what differs from those is restored.
pub(crate) fn deleted(
    state: &mut State,
    kinds: &BTreeMap<String, KindDescriptor>,
    id: &SourceId,
    kind: &SourceKind,
    audio: Option<SourceAudio>,
) -> Vec<Edit> {
    let mut sends = BTreeMap::new();
    state.sends.retain(|(bus, source), gain| {
        if source == id {
            sends.insert(bus.clone(), *gain);
            false
        } else {
            true
        }
    });
    let mut restore = Vec::new();
    let default = default_audio(kind, kinds);
    let gets_master = has_channel(kind, default.as_ref());
    if gets_master && !sends.contains_key(&BusId::master()) {
        restore.push(Edit::SendRemove {
            source_id: id.clone(),
            bus_id: BusId::master(),
        });
    }
    for (bus, gain) in sends {
        if gets_master && bus.is_master() && gain == Finite32::ZERO {
            continue;
        }
        restore.push(Edit::SendSet {
            source_id: id.clone(),
            bus_id: bus,
            gain_db: gain,
        });
    }
    if let (Some(audio), Some(default)) = (audio, default)
        && audio != default
    {
        restore.push(set_audio(id, &default, &audio));
    }
    restore
}

/// The `source.set_audio` that takes `from` to `to`, sending only the fields that
/// differ.
fn set_audio(id: &SourceId, from: &SourceAudio, to: &SourceAudio) -> Edit {
    Edit::SourceSetAudio {
        source_id: id.clone(),
        enabled: (from.enabled != to.enabled).then_some(to.enabled),
        follow: (from.follow != to.follow).then_some(to.follow),
        gain_db: (from.gain_db != to.gain_db).then_some(to.gain_db),
        muted: (from.muted != to.muted).then_some(to.muted),
    }
}

/// Applies `source.set_audio`, `bus.*`, `send.set` and `send.remove`, and returns the
/// edits that undo it. `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::SourceSetAudio {
            source_id,
            enabled,
            follow,
            gain_db,
            muted,
        } => set_source_audio(
            state,
            source_id,
            SourceAudioPatch {
                enabled: *enabled,
                follow: *follow,
                gain_db: *gain_db,
                muted: *muted,
            },
            at,
        ),
        Edit::BusCreate { bus_id, name } => create_bus(state, bus_id, name, at),
        Edit::BusRename { bus_id, name } => rename_bus(state, bus_id, name, at),
        Edit::BusSetFader {
            bus_id,
            gain_db,
            muted,
        } => set_fader(state, bus_id, *gain_db, *muted, at),
        Edit::BusDelete { bus_id } => delete_bus(state, bus_id, at),
        Edit::SendSet {
            source_id,
            bus_id,
            gain_db,
        } => set_send(state, source_id, bus_id, *gain_db, at),
        Edit::SendRemove { source_id, bus_id } => remove_send(state, source_id, bus_id, at),
        _ => return None,
    })
}

/// The fields of a `source.set_audio`.
struct SourceAudioPatch {
    enabled: Option<bool>,
    follow: Option<AudioFollow>,
    gain_db: Option<Finite32>,
    muted: Option<bool>,
}

fn check_gain(gain_db: Finite32, at: usize, name: &str) -> Result<(), ProtocolError> {
    match gain_db_problem(gain_db) {
        Some(reason) => Err(invalid(at, name, reason)),
        None => Ok(()),
    }
}

fn no_audio(at: usize, id: &SourceId) -> ProtocolError {
    invalid(at, "source_id", format!("source `{id}` has no audio"))
}

fn set_source_audio(
    state: &mut State,
    id: &SourceId,
    patch: SourceAudioPatch,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let source = state
        .sources
        .get_mut(id)
        .ok_or_else(|| not_found(EntityKind::Source, id.as_str()))?;
    let is_scene = matches!(source.kind, SourceKind::Scene { .. });
    let audio = source.audio.as_mut().ok_or_else(|| no_audio(at, id))?;
    let SourceAudioPatch {
        enabled,
        follow,
        gain_db,
        muted,
    } = patch;
    if enabled.is_none() && follow.is_none() && gain_db.is_none() && muted.is_none() {
        return Err(invalid(
            at,
            "enabled",
            "send at least one of `enabled`, `follow`, `gain_db` and `muted`",
        ));
    }
    if is_scene {
        let no_fader = "a `scene` source has no fader or mute of its own";
        if gain_db.is_some() {
            return Err(invalid(at, "gain_db", no_fader));
        }
        if muted.is_some() {
            return Err(invalid(at, "muted", no_fader));
        }
    }
    if let Some(gain_db) = gain_db {
        check_gain(gain_db, at, "gain_db")?;
    }
    let before = *audio;
    if let Some(enabled) = enabled {
        audio.enabled = enabled;
    }
    if let Some(follow) = follow {
        audio.follow = follow;
    }
    if let Some(gain_db) = gain_db {
        audio.gain_db = gain_db;
    }
    if let Some(muted) = muted {
        audio.muted = muted;
    }
    // The fields sent, back to what they were.
    Ok(vec![Edit::SourceSetAudio {
        source_id: id.clone(),
        enabled: enabled.map(|_| before.enabled),
        follow: follow.map(|_| before.follow),
        gain_db: gain_db.map(|_| before.gain_db),
        muted: muted.map(|_| before.muted),
    }])
}

fn create_bus(
    state: &mut State,
    id: &BusId,
    name: &str,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(id.as_str(), at, "bus_id")?;
    if state.buses.contains_key(id) {
        return Err(already_exists(EntityKind::Bus, id.as_str()));
    }
    check_name(name, at)?;
    if state.buses.len() >= MAX_BUSES {
        return Err(invalid(
            at,
            "bus_id",
            "a collection holds at most 16 buses, the master included",
        ));
    }
    state
        .buses
        .insert(id.clone(), Bus::new(id.clone(), name.to_owned()));
    Ok(vec![Edit::BusDelete { bus_id: id.clone() }])
}

fn existing_bus<'a>(state: &'a mut State, id: &BusId) -> Result<&'a mut Bus, ProtocolError> {
    state
        .buses
        .get_mut(id)
        .ok_or_else(|| not_found(EntityKind::Bus, id.as_str()))
}

fn rename_bus(
    state: &mut State,
    id: &BusId,
    name: &str,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let bus = existing_bus(state, id)?;
    check_name(name, at)?;
    let old = std::mem::replace(&mut bus.name, name.to_owned());
    Ok(vec![Edit::BusRename {
        bus_id: id.clone(),
        name: old,
    }])
}

fn set_fader(
    state: &mut State,
    id: &BusId,
    gain_db: Option<Finite32>,
    muted: Option<bool>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let bus = existing_bus(state, id)?;
    if gain_db.is_none() && muted.is_none() {
        return Err(invalid(at, "gain_db", "send `gain_db`, `muted` or both"));
    }
    if let Some(gain_db) = gain_db {
        check_gain(gain_db, at, "gain_db")?;
    }
    let inverse = Edit::BusSetFader {
        bus_id: id.clone(),
        gain_db: gain_db.map(|_| bus.gain_db),
        muted: muted.map(|_| bus.muted),
    };
    if let Some(gain_db) = gain_db {
        bus.gain_db = gain_db;
    }
    if let Some(muted) = muted {
        bus.muted = muted;
    }
    Ok(vec![inverse])
}

fn delete_bus(state: &mut State, id: &BusId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    if !state.buses.contains_key(id) {
        return Err(not_found(EntityKind::Bus, id.as_str()));
    }
    if id.is_master() {
        return Err(invalid(at, "bus_id", "the master bus can't be deleted"));
    }
    let bus = state
        .buses
        .remove(id)
        .ok_or_else(|| not_found(EntityKind::Bus, id.as_str()))?;
    let mut inverse = vec![Edit::BusCreate {
        bus_id: bus.id.clone(),
        name: bus.name,
    }];
    if bus.gain_db != Finite32::ZERO || bus.muted {
        inverse.push(Edit::BusSetFader {
            bus_id: bus.id.clone(),
            gain_db: (bus.gain_db != Finite32::ZERO).then_some(bus.gain_db),
            muted: bus.muted.then_some(true),
        });
    }
    let mut restore = Vec::new();
    state.sends.retain(|(bus_id, source_id), gain| {
        if bus_id == id {
            restore.push(Edit::SendSet {
                source_id: source_id.clone(),
                bus_id: bus_id.clone(),
                gain_db: *gain,
            });
            false
        } else {
            true
        }
    });
    inverse.extend(restore);
    Ok(inverse)
}

/// Checks that `source_id` can send into `bus_id`: both exist, and the source has a
/// channel of its own.
fn check_send(
    state: &State,
    source_id: &SourceId,
    bus_id: &BusId,
    at: usize,
) -> Result<(), ProtocolError> {
    let source = state
        .sources
        .get(source_id)
        .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
    if !state.buses.contains_key(bus_id) {
        return Err(not_found(EntityKind::Bus, bus_id.as_str()));
    }
    if matches!(source.kind, SourceKind::Scene { .. }) {
        return Err(invalid(
            at,
            "source_id",
            "a `scene` source has no sends of its own; what it shows sends",
        ));
    }
    if source.audio.is_none() {
        return Err(no_audio(at, source_id));
    }
    Ok(())
}

fn set_send(
    state: &mut State,
    source_id: &SourceId,
    bus_id: &BusId,
    gain_db: Finite32,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_send(state, source_id, bus_id, at)?;
    check_gain(gain_db, at, "gain_db")?;
    let old = state
        .sends
        .insert((bus_id.clone(), source_id.clone()), gain_db);
    Ok(vec![match old {
        Some(gain_db) => Edit::SendSet {
            source_id: source_id.clone(),
            bus_id: bus_id.clone(),
            gain_db,
        },
        None => Edit::SendRemove {
            source_id: source_id.clone(),
            bus_id: bus_id.clone(),
        },
    }])
}

fn remove_send(
    state: &mut State,
    source_id: &SourceId,
    bus_id: &BusId,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_send(state, source_id, bus_id, at)?;
    let gain_db = state
        .sends
        .remove(&(bus_id.clone(), source_id.clone()))
        .ok_or_else(|| {
            invalid(
                at,
                "bus_id",
                format!("source `{source_id}` has no send to bus `{bus_id}`"),
            )
        })?;
    Ok(vec![Edit::SendSet {
        source_id: source_id.clone(),
        bus_id: bus_id.clone(),
        gain_db,
    }])
}
