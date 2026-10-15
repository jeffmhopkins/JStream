//! The mock's audio (ADR-0028): each audio source's settings, the buses and the sends,
//! with `jstream-graph`'s rules, error codes and fields. Which builtin kinds have audio,
//! and video, comes from [`MockEngine::with_kinds`](super::MockEngine::with_kinds); a
//! kind it wasn't told about has neither.

use super::{Collection, already_exists, check_id, check_name, invalid, not_found};
use jstream_protocol::{
    AudioFollow, BusId, BusInfo, Edit, EntityKind, Finite32, ProtocolError, SendInfo, SourceAudio,
    SourceId, SourceKind, gain_db_problem,
};

/// The most buses a collection holds, the master included, as in `jstream-graph`.
const MAX_BUSES: usize = 16;

/// One bus: its name, fader and mute.
#[derive(Clone)]
pub(super) struct Bus {
    name: String,
    gain_db: Finite32,
    muted: bool,
}

impl Bus {
    fn new(name: String) -> Self {
        Self {
            name,
            gain_db: Finite32::ZERO,
            muted: false,
        }
    }

    /// The master bus of a new collection.
    pub(super) fn master() -> (BusId, Self) {
        (BusId::master(), Self::new("Master".into()))
    }
}

impl Collection {
    /// The audio a new source of `kind` starts with: `when_shown` for a `scene` source
    /// and for a kind with audio and video, `always` for one with audio only, and none
    /// for every other source.
    pub(super) fn default_audio(&self, kind: &SourceKind) -> Option<SourceAudio> {
        match kind {
            SourceKind::Scene { .. } => Some(SourceAudio::new(AudioFollow::WhenShown)),
            SourceKind::Builtin { kind } => {
                let video = *self.audio_kinds.get(kind)?;
                Some(SourceAudio::new(if video {
                    AudioFollow::WhenShown
                } else {
                    AudioFollow::Always
                }))
            }
            _ => None,
        }
    }

    /// Gives the new source `id` its master send at 0 dB if it has a channel of its own.
    pub(super) fn audio_created(&mut self, id: &SourceId) {
        if self.has_channel(id) {
            self.sends
                .insert((BusId::master(), id.clone()), Finite32::ZERO);
        }
    }

    /// Gives the copy `to` the audio and sends of `from`.
    pub(super) fn audio_copied(&mut self, from: &SourceId, to: &SourceId) {
        let audio = self.sources.get(from).and_then(|s| s.audio);
        if let Some(copy) = self.sources.get_mut(to) {
            copy.audio = audio;
        }
        self.sends.retain(|(_, source), _| source != to);
        let copies: Vec<_> = self
            .sends
            .iter()
            .filter(|((_, source), _)| source == from)
            .map(|((bus, _), gain)| ((bus.clone(), to.clone()), *gain))
            .collect();
        self.sends.extend(copies);
    }

    /// Takes away the sends of a source that is being deleted.
    pub(super) fn audio_deleted(&mut self, id: &SourceId) {
        self.sends.retain(|(_, source), _| source != id);
    }

    fn has_channel(&self, id: &SourceId) -> bool {
        self.sources
            .get(id)
            .is_some_and(|s| s.audio.is_some() && !matches!(s.kind, SourceKind::Scene { .. }))
    }

    /// Applies an audio edit. `None` for any other edit.
    pub(super) fn apply_audio(
        &mut self,
        edit: &Edit,
        at: usize,
    ) -> Option<Result<(), ProtocolError>> {
        Some(match edit {
            Edit::SourceSetAudio {
                source_id,
                enabled,
                follow,
                gain_db,
                muted,
            } => self.set_source_audio(source_id, *enabled, *follow, *gain_db, *muted, at),
            Edit::BusCreate { bus_id, name } => self.create_bus(bus_id, name, at),
            Edit::BusRename { bus_id, name } => self.rename_bus(bus_id, name, at),
            Edit::BusSetFader {
                bus_id,
                gain_db,
                muted,
            } => self.set_fader(bus_id, *gain_db, *muted, at),
            Edit::BusDelete { bus_id } => self.delete_bus(bus_id, at),
            Edit::SendSet {
                source_id,
                bus_id,
                gain_db,
            } => self.check_send(source_id, bus_id, at).and_then(|()| {
                check_gain(*gain_db, at)?;
                self.sends
                    .insert((bus_id.clone(), source_id.clone()), *gain_db);
                Ok(())
            }),
            Edit::SendRemove { source_id, bus_id } => {
                self.check_send(source_id, bus_id, at).and_then(|()| {
                    self.sends
                        .remove(&(bus_id.clone(), source_id.clone()))
                        .map(|_| ())
                        .ok_or_else(|| {
                            invalid(
                                at,
                                "bus_id",
                                format!("source `{source_id}` has no send to bus `{bus_id}`"),
                            )
                        })
                })
            }
            _ => return None,
        })
    }

    fn set_source_audio(
        &mut self,
        id: &SourceId,
        enabled: Option<bool>,
        follow: Option<AudioFollow>,
        gain_db: Option<Finite32>,
        muted: Option<bool>,
        at: usize,
    ) -> Result<(), ProtocolError> {
        let source = self
            .sources
            .get_mut(id)
            .ok_or_else(|| not_found(EntityKind::Source, id.as_str()))?;
        let is_scene = matches!(source.kind, SourceKind::Scene { .. });
        let audio = source
            .audio
            .as_mut()
            .ok_or_else(|| invalid(at, "source_id", format!("source `{id}` has no audio")))?;
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
            check_gain(gain_db, at)?;
            audio.gain_db = gain_db;
        }
        if let Some(enabled) = enabled {
            audio.enabled = enabled;
        }
        if let Some(follow) = follow {
            audio.follow = follow;
        }
        if let Some(muted) = muted {
            audio.muted = muted;
        }
        Ok(())
    }

    fn create_bus(&mut self, id: &BusId, name: &str, at: usize) -> Result<(), ProtocolError> {
        check_id(id.as_str(), at, "bus_id")?;
        if self.buses.contains_key(id) {
            return Err(already_exists(EntityKind::Bus, id.as_str()));
        }
        check_name(name, at)?;
        if self.buses.len() >= MAX_BUSES {
            return Err(invalid(
                at,
                "bus_id",
                "a collection holds at most 16 buses, the master included",
            ));
        }
        self.buses.insert(id.clone(), Bus::new(name.to_owned()));
        Ok(())
    }

    fn rename_bus(&mut self, id: &BusId, name: &str, at: usize) -> Result<(), ProtocolError> {
        self.bus(id)?;
        check_name(name, at)?;
        if let Some(bus) = self.buses.get_mut(id) {
            name.clone_into(&mut bus.name);
        }
        Ok(())
    }

    fn delete_bus(&mut self, id: &BusId, at: usize) -> Result<(), ProtocolError> {
        self.bus(id)?;
        if id.is_master() {
            return Err(invalid(at, "bus_id", "the master bus can't be deleted"));
        }
        self.buses.remove(id);
        self.sends.retain(|(bus, _), _| bus != id);
        Ok(())
    }

    fn bus(&self, id: &BusId) -> Result<&Bus, ProtocolError> {
        self.buses
            .get(id)
            .ok_or_else(|| not_found(EntityKind::Bus, id.as_str()))
    }

    fn set_fader(
        &mut self,
        id: &BusId,
        gain_db: Option<Finite32>,
        muted: Option<bool>,
        at: usize,
    ) -> Result<(), ProtocolError> {
        self.bus(id)?;
        if gain_db.is_none() && muted.is_none() {
            return Err(invalid(at, "gain_db", "send `gain_db`, `muted` or both"));
        }
        if let Some(gain_db) = gain_db {
            check_gain(gain_db, at)?;
        }
        if let Some(bus) = self.buses.get_mut(id) {
            if let Some(gain_db) = gain_db {
                bus.gain_db = gain_db;
            }
            if let Some(muted) = muted {
                bus.muted = muted;
            }
        }
        Ok(())
    }

    /// The order `jstream-graph` checks a send in: the source, the bus, then whether the
    /// source has a channel.
    fn check_send(
        &self,
        source_id: &SourceId,
        bus_id: &BusId,
        at: usize,
    ) -> Result<(), ProtocolError> {
        let source = self
            .sources
            .get(source_id)
            .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
        self.bus(bus_id)?;
        if matches!(source.kind, SourceKind::Scene { .. }) {
            return Err(invalid(
                at,
                "source_id",
                "a `scene` source has no sends of its own; what it shows sends",
            ));
        }
        if source.audio.is_none() {
            return Err(invalid(
                at,
                "source_id",
                format!("source `{source_id}` has no audio"),
            ));
        }
        Ok(())
    }

    /// Every bus, ordered by ID, as queries report them.
    pub(super) fn bus_infos(&self) -> Vec<BusInfo> {
        self.buses
            .iter()
            .map(|(id, bus)| BusInfo {
                bus_id: id.clone(),
                name: bus.name.clone(),
                gain_db: bus.gain_db,
                muted: bus.muted,
            })
            .collect()
    }

    /// Every send, ordered by bus, then by source, as queries report them.
    pub(super) fn send_infos(&self) -> Vec<SendInfo> {
        self.sends
            .iter()
            .map(|((bus, source), gain_db)| SendInfo {
                source_id: source.clone(),
                bus_id: bus.clone(),
                gain_db: *gain_db,
            })
            .collect()
    }
}

fn check_gain(gain_db: Finite32, at: usize) -> Result<(), ProtocolError> {
    match gain_db_problem(gain_db) {
        Some(reason) => Err(invalid(at, "gain_db", reason)),
        None => Ok(()),
    }
}
