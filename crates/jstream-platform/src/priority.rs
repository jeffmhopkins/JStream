//! The scheduling priority classes JStream's processes ask Windows for (ADR-0195,
//! ADR-0252): their names, the environment variables that lower them, and the order a
//! refused class falls back in. Pure data, so the engine's platform crate and the
//! browser helper read the variables the same way; the calls that set a class are each
//! process's own.

/// The environment variable that names the GPU scheduling class to ask for.
pub const GPU_VARIABLE: &str = "JSTREAM_GPU_PRIORITY";

/// The environment variable that names the CPU priority class to ask for.
pub const CPU_VARIABLE: &str = "JSTREAM_CPU_PRIORITY";

/// A scheduling priority class a JStream process may ask for, on the GPU or the CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The class every process starts in: nothing is asked for.
    Normal,
    /// Ahead of normal processes by a little.
    AboveNormal,
    /// Ahead of normal processes. The CPU's default.
    High,
    /// Ahead of everything else. The GPU's default; never asked for on the CPU.
    Realtime,
}

impl Class {
    /// The GPU class when `JSTREAM_GPU_PRIORITY` names none.
    pub const GPU_DEFAULT: Self = Self::Realtime;
    /// The CPU class when `JSTREAM_CPU_PRIORITY` names none, and the highest it may name.
    pub const CPU_DEFAULT: Self = Self::High;

    /// The class a variable's `value` names, none above `highest`: `default` when it is
    /// unset or empty, and when it names no class or one above `highest` (the second
    /// field then says so, for the log).
    pub fn named(value: Option<&str>, default: Self, highest: Self) -> (Self, bool) {
        let name = match value.map(str::trim) {
            None | Some("") => return (default, true),
            Some(name) => name,
        };
        let class = [Self::Normal, Self::AboveNormal, Self::High, Self::Realtime]
            .into_iter()
            .find(|class| name.eq_ignore_ascii_case(class.name()));
        match class {
            Some(class) if class.rank() <= highest.rank() => (class, true),
            _ => (default, false),
        }
    }

    /// Its name, as the variables spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::AboveNormal => "above_normal",
            Self::High => "high",
            Self::Realtime => "realtime",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::AboveNormal => 1,
            Self::High => 2,
            Self::Realtime => 3,
        }
    }

    /// The classes to ask for in turn until Windows grants one: itself, then `high`
    /// after `realtime`, which Windows may keep for privileged processes. None for
    /// `normal`, which needs no call.
    pub fn tries(self) -> &'static [Self] {
        match self {
            Self::Normal => &[],
            Self::AboveNormal => &[Self::AboveNormal],
            Self::High => &[Self::High],
            Self::Realtime => &[Self::Realtime, Self::High],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Class;

    fn gpu(value: Option<&str>) -> (Class, bool) {
        Class::named(value, Class::GPU_DEFAULT, Class::Realtime)
    }

    fn cpu(value: Option<&str>) -> (Class, bool) {
        Class::named(value, Class::CPU_DEFAULT, Class::High)
    }

    #[test]
    fn the_defaults_are_the_gpus_realtime_and_the_cpus_high() {
        assert_eq!(gpu(None), (Class::Realtime, true));
        assert_eq!(gpu(Some("")), (Class::Realtime, true));
        assert_eq!(cpu(None), (Class::High, true));
        assert_eq!(cpu(Some(" ")), (Class::High, true));
    }

    #[test]
    fn a_variable_lowers_the_class() {
        assert_eq!(gpu(Some(" HIGH ")), (Class::High, true));
        assert_eq!(gpu(Some("above_normal")), (Class::AboveNormal, true));
        assert_eq!(gpu(Some("Normal")), (Class::Normal, true));
        assert_eq!(gpu(Some("realtime")), (Class::Realtime, true));
        assert_eq!(cpu(Some("normal")), (Class::Normal, true));
        assert_eq!(cpu(Some("above_normal")), (Class::AboveNormal, true));
    }

    #[test]
    fn a_value_that_names_no_class_keeps_the_default_and_says_so() {
        assert_eq!(gpu(Some("fastest")), (Class::Realtime, false));
        assert_eq!(cpu(Some("fastest")), (Class::High, false));
    }

    /// The CPU's realtime class can stop the machine's own threads, so the variable
    /// can't ask for it.
    #[test]
    fn the_cpu_class_is_never_realtime() {
        assert_eq!(cpu(Some("realtime")), (Class::High, false));
    }

    #[test]
    fn realtime_falls_back_to_high_and_normal_asks_for_nothing() {
        assert_eq!(Class::Realtime.tries(), [Class::Realtime, Class::High]);
        assert_eq!(Class::High.tries(), [Class::High]);
        assert_eq!(Class::AboveNormal.tries(), [Class::AboveNormal]);
        assert!(Class::Normal.tries().is_empty());
    }

    #[test]
    fn a_class_is_named_as_the_variables_spell_it() {
        for class in [
            Class::Normal,
            Class::AboveNormal,
            Class::High,
            Class::Realtime,
        ] {
            assert_eq!(gpu(Some(class.name())), (class, true));
        }
    }
}
