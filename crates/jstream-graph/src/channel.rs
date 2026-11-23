//! Channels (ADR-0183): the `channel.set` and `channel.delete` edits.
//!
//! A channel is a named value, 0 to 1, that the engine follows and keeps; the collection
//! holds only its definition. `channel.set` checks its shape and that its `channel`
//! inputs don't lead back to it. A channel input, a `scene_id` or an `amount` naming a
//! channel or scene that doesn't exist is taken, as a timeline's cue is, so deleting a
//! channel never edits what reads it.

use crate::Graph;
use crate::apply::{invalid, not_found};
use crate::model::State;
use jstream_protocol::{ChannelInfo, Edit, EntityKind, MAX_CHANNELS, ProtocolError};
use std::collections::BTreeMap;

impl Graph {
    /// Every channel (ADR-0183), ordered by ID.
    pub fn channels(&self) -> impl ExactSizeIterator<Item = &ChannelInfo> {
        self.state.channels.values()
    }

    /// The channel with `channel_id`, if the collection holds it.
    pub fn channel(&self, channel_id: &str) -> Option<&ChannelInfo> {
        self.state.channel(channel_id)
    }
}

impl State {
    /// The channel with `channel_id`, if the collection holds it (ADR-0183).
    pub fn channel(&self, channel_id: &str) -> Option<&ChannelInfo> {
        self.channels.get(channel_id)
    }
}

/// Applies `channel.set` and `channel.delete`, and returns the edits that undo it.
/// `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::ChannelSet { channel } => set(state, channel, at),
        Edit::ChannelDelete { channel_id } => delete(state, channel_id),
        _ => return None,
    })
}

fn set(state: &mut State, channel: &ChannelInfo, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    check(&state.channels, channel, at)?;
    let old = state
        .channels
        .insert(channel.channel_id.clone(), channel.clone());
    Ok(vec![match old {
        Some(channel) => Edit::ChannelSet { channel },
        None => Edit::ChannelDelete {
            channel_id: channel.channel_id.clone(),
        },
    }])
}

/// Its shape, the collection's room for it, and no cycle through its `channel` inputs.
fn check(
    channels: &BTreeMap<String, ChannelInfo>,
    channel: &ChannelInfo,
    at: usize,
) -> Result<(), ProtocolError> {
    if let Some((field, reason)) = channel.problem() {
        return Err(invalid(at, &format!("channel.{field}"), reason));
    }
    let id = &channel.channel_id;
    if !channels.contains_key(id) && channels.len() >= MAX_CHANNELS {
        return Err(invalid(
            at,
            "channel",
            format!("a collection holds at most {MAX_CHANNELS} channels"),
        ));
    }
    if leads_back(channels, channel) {
        return Err(invalid(
            at,
            "channel.inputs",
            "a `channel` input leads back to this channel through others",
        ));
    }
    Ok(())
}

/// Whether following `channel`'s `channel` inputs through `channels`, with `channel` in
/// its place, reaches `channel` again.
fn leads_back(channels: &BTreeMap<String, ChannelInfo>, channel: &ChannelInfo) -> bool {
    let reads = |id: &str| -> Vec<String> {
        let read = if id == channel.channel_id {
            Some(channel)
        } else {
            channels.get(id)
        };
        read.map(|c| c.reads().map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut todo = reads(&channel.channel_id);
    while let Some(id) = todo.pop() {
        if id == channel.channel_id {
            return true;
        }
        if seen.insert(id.clone()) {
            todo.extend(reads(&id));
        }
    }
    false
}

/// Puts `channel` in `state` as a collection file saved it: its shape and cycles
/// checked, what it names not looked up.
pub(crate) fn load(state: &mut State, channel: &ChannelInfo) -> Result<(), ProtocolError> {
    check(&state.channels, channel, 0)?;
    state
        .channels
        .insert(channel.channel_id.clone(), channel.clone());
    Ok(())
}

fn delete(state: &mut State, channel_id: &str) -> Result<Vec<Edit>, ProtocolError> {
    let channel = state
        .channels
        .remove(channel_id)
        .ok_or_else(|| not_found(EntityKind::Channel, channel_id))?;
    Ok(vec![Edit::ChannelSet { channel }])
}
