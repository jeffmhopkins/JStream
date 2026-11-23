//! The engine process's GPU scheduling priority class (ADR-0117 §9).
//!
//! Windows schedules every process's GPU work on an adapter by a per-process class, as
//! it schedules CPU threads by a priority class. Every process starts in the normal
//! class, so on a GPU that is busy a compositor's frame waits its turn behind the
//! desktop's, a browser's and any other program's work.
//!
//! Whether the engine should ask for a higher class is a question for the bench
//! (ADR-0117 §9), so it asks only when told to. The environment variable [`VARIABLE`]
//! names the class: `high` or `above_normal`, which opening the engine's device
//! ([`WindowsGpu::open_device`](crate::WindowsGpu)) then asks for, once for the process,
//! through `D3DKMTSetProcessSchedulingPriorityClass`. Unset, empty or `normal`, the
//! process is left in the class it started in and no call is made, so a run can be
//! compared with and without it. A higher class also puts the engine ahead of the
//! desktop compositor, which capture and the UI depend on; that is what the bench has
//! to weigh. Where Windows refuses, the process stays in the class it had, and a line in
//! the log says so: nothing fails.

/// The environment variable that names the class to ask for.
#[cfg(any(windows, test))]
pub(crate) const VARIABLE: &str = "JSTREAM_GPU_PRIORITY";

/// A GPU scheduling priority class the engine may ask for.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// The class every process starts in: nothing is asked for. The default.
    Normal,
    /// Ahead of normal processes by a little.
    AboveNormal,
    /// Ahead of normal processes.
    High,
}

#[cfg(any(windows, test))]
impl Class {
    /// The class `value`, [`VARIABLE`]'s, names: `normal` when it is unset or empty, and
    /// when it names none (the second field then says so, for the log).
    pub(crate) fn named(value: Option<&str>) -> (Self, bool) {
        match value.map(str::trim) {
            None | Some("") => (Self::Normal, true),
            Some(name) if name.eq_ignore_ascii_case("high") => (Self::High, true),
            Some(name) if name.eq_ignore_ascii_case("above_normal") => (Self::AboveNormal, true),
            Some(name) if name.eq_ignore_ascii_case("normal") => (Self::Normal, true),
            Some(_) => (Self::Normal, false),
        }
    }

    /// Its name, as [`VARIABLE`] spells it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::AboveNormal => "above_normal",
            Self::High => "high",
        }
    }
}

#[cfg(windows)]
pub(crate) use os::raise_once;

#[cfg(windows)]
mod os {
    use std::sync::Once;

    use windows::Wdk::Graphics::Direct3D::{
        D3DKMT_SCHEDULINGPRIORITYCLASS_ABOVE_NORMAL, D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
        D3DKMTSetProcessSchedulingPriorityClass,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    use super::{Class, VARIABLE};

    /// Asks Windows for the GPU scheduling class [`VARIABLE`] names, the first time it is
    /// called in the process; later calls do nothing, and neither does a first one with
    /// no class named. Never fails: a refusal is logged.
    pub(crate) fn raise_once() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let value = std::env::var(VARIABLE).ok();
            let (class, known) = Class::named(value.as_deref());
            if !known {
                tracing::warn!(
                    variable = VARIABLE,
                    value = value.as_deref().unwrap_or_default(),
                    "not a GPU scheduling class (high, above_normal or normal); leaving it as it is"
                );
            }
            let asked = match class {
                Class::Normal => {
                    tracing::info!(class = class.name(), "GPU scheduling class left as it is");
                    return;
                }
                Class::AboveNormal => D3DKMT_SCHEDULINGPRIORITYCLASS_ABOVE_NORMAL,
                Class::High => D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
            };
            // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid
            // and needs no closing, and the class is passed by value.
            let status =
                unsafe { D3DKMTSetProcessSchedulingPriorityClass(GetCurrentProcess(), asked) };
            if status.is_ok() {
                tracing::info!(class = class.name(), "GPU scheduling class set");
            } else {
                tracing::warn!(
                    class = class.name(),
                    status = format_args!("{:#010x}", status.0),
                    "Windows refused the GPU scheduling class; the process keeps the one it had"
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Class;

    #[test]
    fn the_class_is_normal_unless_the_variable_names_another() {
        assert_eq!(Class::named(None), (Class::Normal, true));
        assert_eq!(Class::named(Some("")), (Class::Normal, true));
        assert_eq!(Class::named(Some(" HIGH ")), (Class::High, true));
        assert_eq!(
            Class::named(Some("above_normal")),
            (Class::AboveNormal, true)
        );
        assert_eq!(Class::named(Some("Normal")), (Class::Normal, true));
    }

    #[test]
    fn a_value_that_names_no_class_asks_for_nothing_and_says_so() {
        assert_eq!(Class::named(Some("realtime")), (Class::Normal, false));
    }

    #[test]
    fn a_class_is_named_as_the_variable_spells_it() {
        for class in [Class::Normal, Class::AboveNormal, Class::High] {
            assert_eq!(Class::named(Some(class.name())), (class, true));
        }
    }
}
