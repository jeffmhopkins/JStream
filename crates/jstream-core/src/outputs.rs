//! Outputs: their runtime state and the `output.start`/`output.stop` actions.
//!
//! **Placeholder.** There is no media pipeline yet, so starting an output does no work:
//! the state machine moves through the states the protocol reports (`starting` →
//! `active`, `stopping` → `stopped`) at once, emitting an `output.state_changed` event
//! for each step, as `jstream-control`'s mock does. When `jstream-outputs` lands, the
//! engine will report `active` (or `failed`) only once the output really runs.
//!
//! Outputs are runtime state, not collection state: nothing here changes `rev`.

use jstream_protocol::{EntityKind, OutputId, OutputState, ProtocolError};
use std::collections::BTreeMap;

/// Every output the engine knows, by ID, with its current state.
#[derive(Default)]
pub(crate) struct Outputs {
    states: BTreeMap<OutputId, OutputState>,
}

impl Outputs {
    /// Adds `output_id` in the `stopped` state, or leaves an existing one as it is.
    pub(crate) fn add(&mut self, output_id: OutputId) {
        self.states.entry(output_id).or_insert(OutputState::Stopped);
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
}

fn not_found(output_id: &OutputId) -> ProtocolError {
    ProtocolError::NotFound {
        entity: EntityKind::Output,
        id: output_id.as_str().to_owned(),
    }
}
