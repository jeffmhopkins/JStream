//! Outputs: their runtime state and the `output.start`/`output.stop` actions.
//!
//! **Placeholder path.** When the engine has no platform backends, starting an output
//! does no media work: [`Outputs::transition`] moves through the states the protocol
//! reports (`starting` → `active`, `stopping` → `stopped`) at once, as
//! `jstream-control`'s mock does. With backends, the engine uses [`Outputs::begin`] and
//! [`Outputs::settle`] so `active` or `failed` is reported only once the pipeline is up,
//! and `reconnecting` while its publish link is being re-established.
//!
//! The states here are runtime state, not collection state: nothing here changes `rev`.
//! The engine knows an output either because the collection has it (`output.create`)
//! or because the host declared it (`Engine::with_output`). After each applied batch,
//! [`Outputs::sync`] gives a created output a `stopped` state and forgets a deleted one;
//! the graph refuses to delete one that is running.

use jstream_graph::State;
use jstream_protocol::{EntityKind, OutputId, OutputState, ProtocolError};
use std::collections::{BTreeMap, BTreeSet};

/// Every output the engine knows, by ID, with its current state.
#[derive(Default)]
pub(crate) struct Outputs {
    states: BTreeMap<OutputId, OutputState>,
    /// The outputs the host declared with `Engine::with_output`. They stay known
    /// whatever the collection holds.
    declared: BTreeSet<OutputId>,
}

impl Outputs {
    /// Declares `output_id` in the `stopped` state, or leaves an existing one as it is.
    pub(crate) fn add(&mut self, output_id: OutputId) {
        self.declared.insert(output_id.clone());
        self.states.entry(output_id).or_insert(OutputState::Stopped);
    }

    /// Follows the collection after a batch: an output it has gained starts `stopped`,
    /// and one it has lost is forgotten unless the host declared it. The graph refused
    /// to delete a running output, so only idle ones are forgotten.
    pub(crate) fn sync(&mut self, collection: &State) {
        for output in collection.outputs() {
            self.states
                .entry(output.id().clone())
                .or_insert(OutputState::Stopped);
        }
        let declared = &self.declared;
        self.states.retain(|id, state| {
            declared.contains(id)
                || collection.output(id).is_some()
                || !matches!(state, OutputState::Stopped | OutputState::Failed)
        });
    }

    /// The outputs that are running: every state but `stopped` and `failed`. The graph
    /// refuses to delete these.
    pub(crate) fn running(&self) -> BTreeSet<OutputId> {
        self.states
            .iter()
            .filter(|(_, state)| !matches!(state, OutputState::Stopped | OutputState::Failed))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The current state of `output_id`, or `not_found`.
    pub(crate) fn state(&self, output_id: &OutputId) -> Result<OutputState, ProtocolError> {
        self.states
            .get(output_id)
            .copied()
            .ok_or_else(|| not_found(output_id))
    }

    /// Starts (`start`) or stops an output and returns the states it went through, in
    /// order. The caller emits one `output.state_changed` event per step.
    ///
    /// Fails, changing nothing, with `not_found` for an unknown output, and with
    /// `action_failed` when starting an output that runs or stopping one that doesn't.
    /// `stopped` and `failed` count as not running; `starting`, `active`, `reconnecting`
    /// and `stopping` count as running.
    pub(crate) fn transition(
        &mut self,
        output_id: &OutputId,
        start: bool,
    ) -> Result<[OutputState; 2], ProtocolError> {
        let current = self.state(output_id)?;
        let idle = matches!(current, OutputState::Stopped | OutputState::Failed);
        let steps = match (start, idle) {
            (true, true) => [OutputState::Starting, OutputState::Active],
            (false, false) => [OutputState::Stopping, OutputState::Stopped],
            (true, false) => {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is already running"),
                });
            }
            (false, true) => {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is not running"),
                });
            }
        };
        if let Some(state) = self.states.get_mut(output_id) {
            *state = steps[1];
        }
        Ok(steps)
    }

    /// Moves an idle output to `starting`, or a running one to `stopping`. The caller
    /// emits that one step, then [`settle`](Self::settle) once the pipeline has caught up.
    ///
    /// Fails, changing nothing, with the same errors as [`transition`](Self::transition).
    pub(crate) fn begin(
        &mut self,
        output_id: &OutputId,
        start: bool,
    ) -> Result<OutputState, ProtocolError> {
        let current = self.state(output_id)?;
        let idle = matches!(current, OutputState::Stopped | OutputState::Failed);
        let next = match (start, idle) {
            (true, true) => OutputState::Starting,
            (false, false) => OutputState::Stopping,
            (true, false) => {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is already running"),
                });
            }
            (false, true) => {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is not running"),
                });
            }
        };
        if let Some(state) = self.states.get_mut(output_id) {
            *state = next;
        }
        Ok(next)
    }

    /// Records the state the pipeline actually reached (`active`, `reconnecting`,
    /// `failed` or `stopped`).
    pub(crate) fn settle(&mut self, output_id: &OutputId, state: OutputState) {
        if let Some(slot) = self.states.get_mut(output_id) {
            *slot = state;
        }
    }
}

fn not_found(output_id: &OutputId) -> ProtocolError {
    ProtocolError::NotFound {
        entity: EntityKind::Output,
        id: output_id.as_str().to_owned(),
    }
}
