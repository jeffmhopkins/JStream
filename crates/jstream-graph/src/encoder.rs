//! Encoder profiles: the `encoder.*` edits, `output.set_encoder`, and the check that a
//! profile holds an encoding the engine can open.
//!
//! An output refers to a profile by ID, so a profile can't be deleted while an output
//! uses it. A profile changed while an output runs doesn't touch that output's encoder
//! session: the engine reads the profile again on the output's next start.

use crate::apply::{already_exists, check_id, check_name, invalid, not_found};
use crate::model::{Encoder, State};
use jstream_protocol::{
    Edit, EncoderProfile, EncoderProfileId, EntityKind, OutputId, ProtocolError,
};

/// Applies `encoder.create`, `encoder.update`, `encoder.delete` and
/// `output.set_encoder`, and returns the edits that undo it. `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::EncoderCreate {
            profile_id,
            name,
            profile,
        } => create(state, profile_id, name, profile, at),
        Edit::EncoderUpdate {
            profile_id,
            name,
            profile,
        } => update(state, profile_id, name, profile, at),
        Edit::EncoderDelete { profile_id } => delete(state, profile_id, at),
        Edit::OutputSetEncoder {
            output_id,
            profile_id,
        } => set_encoder(state, output_id, profile_id.as_ref()),
        _ => return None,
    })
}

fn create(
    state: &mut State,
    id: &EncoderProfileId,
    name: &str,
    profile: &EncoderProfile,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(id.as_str(), at, "profile_id")?;
    if state.encoders.contains_key(id) {
        return Err(already_exists(EntityKind::Encoder, id.as_str()));
    }
    check_name(name, at)?;
    check_profile(profile, at)?;
    state.encoders.insert(
        id.clone(),
        Encoder {
            id: id.clone(),
            name: name.to_owned(),
            profile: *profile,
        },
    );
    Ok(vec![Edit::EncoderDelete {
        profile_id: id.clone(),
    }])
}

fn update(
    state: &mut State,
    id: &EncoderProfileId,
    name: &str,
    profile: &EncoderProfile,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let encoder = state
        .encoders
        .get_mut(id)
        .ok_or_else(|| not_found(EntityKind::Encoder, id.as_str()))?;
    check_name(name, at)?;
    check_profile(profile, at)?;
    let old_name = std::mem::replace(&mut encoder.name, name.to_owned());
    let old_profile = std::mem::replace(&mut encoder.profile, *profile);
    Ok(vec![Edit::EncoderUpdate {
        profile_id: id.clone(),
        name: old_name,
        profile: old_profile,
    }])
}

fn delete(state: &mut State, id: &EncoderProfileId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    if !state.encoders.contains_key(id) {
        return Err(not_found(EntityKind::Encoder, id.as_str()));
    }
    if let Some(user) = state
        .outputs
        .values()
        .find(|output| output.encoder_profile.as_ref() == Some(id))
    {
        return Err(invalid(
            at,
            "profile_id",
            format!(
                "encoder profile `{id}` is used by output `{}`; point it elsewhere first",
                user.id
            ),
        ));
    }
    let encoder = state
        .encoders
        .remove(id)
        .ok_or_else(|| not_found(EntityKind::Encoder, id.as_str()))?;
    Ok(vec![Edit::EncoderCreate {
        profile_id: encoder.id,
        name: encoder.name,
        profile: encoder.profile,
    }])
}

fn set_encoder(
    state: &mut State,
    output_id: &OutputId,
    profile_id: Option<&EncoderProfileId>,
) -> Result<Vec<Edit>, ProtocolError> {
    if !state.outputs.contains_key(output_id) {
        return Err(not_found(EntityKind::Output, output_id.as_str()));
    }
    if let Some(id) = profile_id
        && !state.encoders.contains_key(id)
    {
        return Err(not_found(EntityKind::Encoder, id.as_str()));
    }
    let output = state
        .outputs
        .get_mut(output_id)
        .ok_or_else(|| not_found(EntityKind::Output, output_id.as_str()))?;
    let old = std::mem::replace(&mut output.encoder_profile, profile_id.cloned());
    Ok(vec![Edit::OutputSetEncoder {
        output_id: output_id.clone(),
        profile_id: old,
    }])
}

/// Checks that `profile` is an encoding the engine can open.
fn check_profile(profile: &EncoderProfile, at: usize) -> Result<(), ProtocolError> {
    match profile.problem() {
        Some((name, reason)) => Err(invalid(at, &format!("profile.{name}"), reason)),
        None => Ok(()),
    }
}
