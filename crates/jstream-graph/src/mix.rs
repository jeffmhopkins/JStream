//! Video mixes: the `video_mix.*` edits and `output.set_video_mix`.
//!
//! An output refers to a mix by ID, so a mix can't be deleted while an output shows it,
//! and a `scene` mix refers to its scene, so that scene can't be deleted while the mix
//! shows it (a blocker of `scene.delete`, see [`crate::deps`]). A mix's size and rate
//! changed while an output runs don't touch that output's encoder session: the engine
//! reads them again on the output's next start.

use crate::apply::{already_exists, check_id, check_name, invalid, not_found};
use crate::model::{State, VideoMix};
use jstream_protocol::{
    Edit, EntityKind, OutputId, ProtocolError, VideoMix as MixSpec, VideoMixId,
};

/// Applies `video_mix.create`, `video_mix.update`, `video_mix.delete` and
/// `output.set_video_mix`, and returns the edits that undo it. `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::VideoMixCreate { mix_id, name, mix } => create(state, mix_id, name, mix, at),
        Edit::VideoMixUpdate { mix_id, name, mix } => update(state, mix_id, name, mix, at),
        Edit::VideoMixDelete { mix_id } => delete(state, mix_id, at),
        Edit::OutputSetVideoMix { output_id, mix_id } => {
            set_video_mix(state, output_id, mix_id.as_ref())
        }
        _ => return None,
    })
}

fn create(
    state: &mut State,
    id: &VideoMixId,
    name: &str,
    mix: &MixSpec,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(id.as_str(), at, "mix_id")?;
    if state.video_mixes.contains_key(id) {
        return Err(already_exists(EntityKind::VideoMix, id.as_str()));
    }
    check_name(name, at)?;
    check_mix(state, mix, at)?;
    state.video_mixes.insert(
        id.clone(),
        VideoMix {
            id: id.clone(),
            name: name.to_owned(),
            mix: mix.clone(),
        },
    );
    Ok(vec![Edit::VideoMixDelete { mix_id: id.clone() }])
}

fn update(
    state: &mut State,
    id: &VideoMixId,
    name: &str,
    mix: &MixSpec,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    if !state.video_mixes.contains_key(id) {
        return Err(not_found(EntityKind::VideoMix, id.as_str()));
    }
    check_name(name, at)?;
    check_mix(state, mix, at)?;
    let current = state
        .video_mixes
        .get_mut(id)
        .ok_or_else(|| not_found(EntityKind::VideoMix, id.as_str()))?;
    let old_name = std::mem::replace(&mut current.name, name.to_owned());
    let old_mix = std::mem::replace(&mut current.mix, mix.clone());
    Ok(vec![Edit::VideoMixUpdate {
        mix_id: id.clone(),
        name: old_name,
        mix: old_mix,
    }])
}

fn delete(state: &mut State, id: &VideoMixId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    if !state.video_mixes.contains_key(id) {
        return Err(not_found(EntityKind::VideoMix, id.as_str()));
    }
    if let Some(user) = state
        .outputs
        .values()
        .find(|output| output.video_mix.as_ref() == Some(id))
    {
        return Err(invalid(
            at,
            "mix_id",
            format!(
                "video mix `{id}` is used by output `{}`; point it elsewhere first",
                user.id
            ),
        ));
    }
    let mix = state
        .video_mixes
        .remove(id)
        .ok_or_else(|| not_found(EntityKind::VideoMix, id.as_str()))?;
    Ok(vec![Edit::VideoMixCreate {
        mix_id: mix.id,
        name: mix.name,
        mix: mix.mix,
    }])
}

fn set_video_mix(
    state: &mut State,
    output_id: &OutputId,
    mix_id: Option<&VideoMixId>,
) -> Result<Vec<Edit>, ProtocolError> {
    if !state.outputs.contains_key(output_id) {
        return Err(not_found(EntityKind::Output, output_id.as_str()));
    }
    if let Some(id) = mix_id
        && !state.video_mixes.contains_key(id)
    {
        return Err(not_found(EntityKind::VideoMix, id.as_str()));
    }
    let output = state
        .outputs
        .get_mut(output_id)
        .ok_or_else(|| not_found(EntityKind::Output, output_id.as_str()))?;
    let old = std::mem::replace(&mut output.video_mix, mix_id.cloned());
    Ok(vec![Edit::OutputSetVideoMix {
        output_id: output_id.clone(),
        mix_id: old,
    }])
}

/// Checks that `mix` is one the engine can render, and that the scene it shows exists.
fn check_mix(state: &State, mix: &MixSpec, at: usize) -> Result<(), ProtocolError> {
    if let Some((name, reason)) = mix.problem() {
        return Err(invalid(at, &format!("mix.{name}"), reason));
    }
    if let Some(scene_id) = mix.scene()
        && state.scene(scene_id).is_none()
    {
        return Err(not_found(EntityKind::Scene, scene_id.as_str()));
    }
    Ok(())
}
