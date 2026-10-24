//! The resources one plugin may use.
//!
//! The defaults are fixed and conservative. A plugin's configuration can lower them,
//! never raise them: each setter keeps the smaller of the current value and the one asked
//! for. Whether a user may raise them, and to what, is issue #348's decision.

/// The fuel, memory and table limits of one plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    fuel: u64,
    memory_bytes: usize,
    table_elements: usize,
}

impl Limits {
    /// The fuel one invocation gets by default: about ten million WASM instructions,
    /// milliseconds of work. The budget is refilled at the start of every invocation, and
    /// load gets one too (for the module's `start` function).
    pub const DEFAULT_FUEL: u64 = 10_000_000;

    /// The default cap on the plugin's linear memory: 16 MiB (256 WASM pages).
    pub const DEFAULT_MEMORY_BYTES: usize = 16 * 1024 * 1024;

    /// The default cap on the elements of the plugin's table.
    pub const DEFAULT_TABLE_ELEMENTS: usize = 10_000;

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
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            fuel: Self::DEFAULT_FUEL,
            memory_bytes: Self::DEFAULT_MEMORY_BYTES,
            table_elements: Self::DEFAULT_TABLE_ELEMENTS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Limits;

    #[test]
    fn limits_can_be_lowered_but_never_raised() {
        let lowered = Limits::default()
            .with_fuel(1_000)
            .with_memory_bytes(65_536)
            .with_table_elements(10);
        assert_eq!(lowered.fuel(), 1_000);
        assert_eq!(lowered.memory_bytes(), 65_536);
        assert_eq!(lowered.table_elements(), 10);

        let raised = Limits::default()
            .with_fuel(u64::MAX)
            .with_memory_bytes(usize::MAX)
            .with_table_elements(usize::MAX);
        assert_eq!(raised, Limits::default());
        assert_eq!(lowered.with_fuel(2_000).fuel(), 1_000);
    }
}
