//! The resources one plugin may use.
//!
//! The defaults are ADR-0069's (#348): 100 ms of wall time per entry into a plugin and
//! 1 s for its load, 64 MiB of linear memory and 10,000 table elements, plus a fuel
//! budget. A plugin's configuration can lower them, never raise them: each setter keeps
//! the smaller of the current value and the one asked for.

use std::time::Duration;

/// The fuel, memory and table limits of one plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    fuel: u64,
    memory_bytes: usize,
    table_elements: usize,
    call_deadline: Duration,
    load_deadline: Duration,
}

impl Limits {
    /// The fuel one invocation gets by default: about ten million WASM instructions,
    /// milliseconds of work. The budget is refilled at the start of every invocation, and
    /// load gets one too (for the module's `start` function).
    pub const DEFAULT_FUEL: u64 = 10_000_000;

    /// The default cap on the plugin's linear memory: 64 MiB (1,024 WASM pages).
    pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;

    /// The default cap on the elements of the plugin's table.
    pub const DEFAULT_TABLE_ELEMENTS: usize = 10_000;

    /// The default wall time of one entry into the plugin: an invocation, re-armed after
    /// each of its `call`s returns, so the time the engine spends answering doesn't
    /// count. Measured in ticks of [`EPOCH_TICK`](crate::EPOCH_TICK), so a plugin gets at
    /// least this long and at most this rounded up to a whole tick, plus one tick, as
    /// the ticker keeps time.
    pub const DEFAULT_CALL_DEADLINE: Duration = Duration::from_millis(100);

    /// The default wall time of the load: instantiating the module and its `start`
    /// function, re-armed after each of its `call`s returns like an invocation's.
    pub const DEFAULT_LOAD_DEADLINE: Duration = Duration::from_secs(1);

    /// Lowers the fuel per invocation to `fuel`, if that is lower.
    #[must_use]
    pub fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = self.fuel.min(fuel);
        self
    }

    /// Lowers the linear-memory cap to `bytes`, if that is lower.
    #[must_use]
    pub fn with_memory_bytes(mut self, bytes: usize) -> Self {
        self.memory_bytes = self.memory_bytes.min(bytes);
        self
    }

    /// Lowers the table cap to `elements`, if that is lower.
    #[must_use]
    pub fn with_table_elements(mut self, elements: usize) -> Self {
        self.table_elements = self.table_elements.min(elements);
        self
    }

    /// Lowers the wall time of one entry into the plugin to `deadline`, if that is lower.
    #[must_use]
    pub fn with_call_deadline(mut self, deadline: Duration) -> Self {
        self.call_deadline = self.call_deadline.min(deadline);
        self
    }

    /// Lowers the wall time of the load to `deadline`, if that is lower.
    #[must_use]
    pub fn with_load_deadline(mut self, deadline: Duration) -> Self {
        self.load_deadline = self.load_deadline.min(deadline);
        self
    }

    /// The default limits with fuel that never runs out, so a test can reach the
    /// deadline: the plugin's code alone runs out of fuel within milliseconds.
    #[cfg(test)]
    pub(crate) fn unmetered() -> Self {
        Limits {
            fuel: u64::MAX,
            ..Limits::default()
        }
    }

    /// The fuel each invocation starts with.
    pub fn fuel(&self) -> u64 {
        self.fuel
    }

    /// The most linear memory the plugin may have, in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    /// The most elements the plugin's table may have.
    pub fn table_elements(&self) -> usize {
        self.table_elements
    }

    /// The wall time of one entry into the plugin.
    pub fn call_deadline(&self) -> Duration {
        self.call_deadline
    }

    /// The wall time of the load.
    pub fn load_deadline(&self) -> Duration {
        self.load_deadline
    }
}

/// The epoch deadline for `deadline`: its whole ticks, rounded up, plus one for the tick
/// already under way when it is armed, so the plugin gets at least `deadline` and at most
/// one tick more.
pub(crate) fn ticks(deadline: Duration, tick: Duration) -> u64 {
    let ticks = deadline.as_nanos().div_ceil(tick.as_nanos().max(1));
    u64::try_from(ticks).unwrap_or(u64::MAX).saturating_add(1)
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            fuel: Self::DEFAULT_FUEL,
            memory_bytes: Self::DEFAULT_MEMORY_BYTES,
            table_elements: Self::DEFAULT_TABLE_ELEMENTS,
            call_deadline: Self::DEFAULT_CALL_DEADLINE,
            load_deadline: Self::DEFAULT_LOAD_DEADLINE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Limits, ticks};
    use std::time::Duration;

    #[test]
    fn limits_can_be_lowered_but_never_raised() {
        let lowered = Limits::default()
            .with_fuel(1_000)
            .with_memory_bytes(65_536)
            .with_table_elements(10)
            .with_call_deadline(Duration::from_millis(20))
            .with_load_deadline(Duration::from_millis(30));
        assert_eq!(lowered.fuel(), 1_000);
        assert_eq!(lowered.call_deadline(), Duration::from_millis(20));
        assert_eq!(lowered.load_deadline(), Duration::from_millis(30));
        assert_eq!(lowered.memory_bytes(), 65_536);
        assert_eq!(lowered.table_elements(), 10);

        let raised = Limits::default()
            .with_fuel(u64::MAX)
            .with_memory_bytes(usize::MAX)
            .with_table_elements(usize::MAX)
            .with_call_deadline(Duration::MAX)
            .with_load_deadline(Duration::MAX);
        assert_eq!(raised, Limits::default());
        assert_eq!(lowered.with_fuel(2_000).fuel(), 1_000);
    }

    #[test]
    fn deadlines_round_up_to_whole_ticks_plus_the_current_one() {
        let tick = Duration::from_millis(10);
        assert_eq!(ticks(Limits::DEFAULT_CALL_DEADLINE, tick), 11);
        assert_eq!(ticks(Limits::DEFAULT_LOAD_DEADLINE, tick), 101);
        assert_eq!(ticks(Duration::from_millis(15), tick), 3);
        assert_eq!(ticks(Duration::ZERO, tick), 1);
        assert_eq!(ticks(Duration::MAX, tick), u64::MAX);
    }
}
