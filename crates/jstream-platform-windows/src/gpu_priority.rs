//! The engine process's scheduling priority on the GPU and the CPU (ADR-0195, which
//! amends ADR-0117 §9).
//!
//! A machine that runs the engine is there to run it: the program's frames come before
//! whatever else the machine is doing. So the first time the engine opens its GPU device
//! (`WindowsGpu::open_device`), the process asks Windows for:
//!
//! - **The highest GPU scheduling class it is given.** Windows schedules every
//!   process's GPU work on an adapter by a per-process class, and every process starts
//!   in the normal one, so on a busy GPU a compositor's frame waits its turn behind the
//!   desktop's, a browser's and any other program's work. The process asks for
//!   `realtime` through `D3DKMTSetProcessSchedulingPriorityClass`, and for `high` when
//!   Windows refuses that (it keeps `realtime` for processes that hold a privilege).
//! - **The `high` CPU priority class** (`SetPriorityClass`), which puts every thread of
//!   the process (render, readback, encode, output, control) ahead of normal programs'
//!   threads. The audio and render threads' MMCSS tasks sit above it as before. Never
//!   the CPU's own `realtime` class: a busy thread there can stop the mouse, the disk
//!   and the audio devices' own threads.
//!
//! Two environment variables lower either one, so a run can be compared and a shared
//! machine can opt out: `JSTREAM_GPU_PRIORITY` takes `realtime`, `high`, `above_normal` or
//! `normal`, and `JSTREAM_CPU_PRIORITY` takes `high`, `above_normal` or `normal`. `normal`
//! makes no call. A value that names no class is warned about and the default kept.
//! Where Windows refuses, the process stays in the class it had and the log says so:
//! nothing fails. Each line in the log names the class that was set.

/// The environment variable that names the GPU scheduling class to ask for.
#[cfg(windows)]
pub(crate) const GPU_VARIABLE: &str = "JSTREAM_GPU_PRIORITY";

/// The environment variable that names the CPU priority class to ask for.
#[cfg(windows)]
pub(crate) const CPU_VARIABLE: &str = "JSTREAM_CPU_PRIORITY";

/// A scheduling priority class the engine may ask for, on the GPU or the CPU.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// The class every process starts in: nothing is asked for.
    Normal,
    /// Ahead of normal processes by a little.
    AboveNormal,
    /// Ahead of normal processes. The CPU's default.
    High,
    /// Ahead of everything else. The GPU's default; never asked for on the CPU.
    Realtime,
}

#[cfg(any(windows, test))]
impl Class {
    /// The GPU class when `JSTREAM_GPU_PRIORITY` names none.
    pub(crate) const GPU_DEFAULT: Self = Self::Realtime;
    /// The CPU class when `JSTREAM_CPU_PRIORITY` names none, and the highest it may name.
    pub(crate) const CPU_DEFAULT: Self = Self::High;

    /// The class a variable's `value` names, none above `highest`: `default` when it is
    /// unset or empty, and when it names no class or one above `highest` (the second
    /// field then says so, for the log).
    pub(crate) fn named(value: Option<&str>, default: Self, highest: Self) -> (Self, bool) {
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
    pub(crate) fn name(self) -> &'static str {
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
    pub(crate) fn tries(self) -> &'static [Self] {
        match self {
            Self::Normal => &[],
            Self::AboveNormal => &[Self::AboveNormal],
            Self::High => &[Self::High],
            Self::Realtime => &[Self::Realtime, Self::High],
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
        D3DKMT_SCHEDULINGPRIORITYCLASS_REALTIME, D3DKMTSetProcessSchedulingPriorityClass,
    };
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, HIGH_PRIORITY_CLASS, SetPriorityClass,
    };

    use super::{CPU_VARIABLE, Class, GPU_VARIABLE};

    /// Asks Windows for the process's GPU scheduling class and CPU priority class, the
    /// first time it is called in the process; later calls do nothing. Never fails: a
    /// refusal is logged.
    pub(crate) fn raise_once() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let gpu = chosen(GPU_VARIABLE, Class::GPU_DEFAULT, Class::Realtime);
            raise("GPU scheduling class", gpu, set_gpu);
            let cpu = chosen(CPU_VARIABLE, Class::CPU_DEFAULT, Class::High);
            raise("CPU priority class", cpu, set_cpu);
        });
    }

    /// The class `variable` names, or `default`, with a warning for a value that names
    /// none it may.
    fn chosen(variable: &str, default: Class, highest: Class) -> Class {
        let value = std::env::var(variable).ok();
        let (class, known) = Class::named(value.as_deref(), default, highest);
        if !known {
            tracing::warn!(
                variable,
                value = value.as_deref().unwrap_or_default(),
                highest = highest.name(),
                default = default.name(),
                "not a priority class this variable takes; keeping the default"
            );
        }
        class
    }

    /// Asks for `class` with `set`, then for each class after it in [`Class::tries`]
    /// until Windows grants one, and logs which was set, or that none was.
    fn raise(what: &str, class: Class, set: fn(Class) -> Result<(), String>) {
        if class.tries().is_empty() {
            tracing::info!(what, class = class.name(), "left as it is");
            return;
        }
        for &asked in class.tries() {
            match set(asked) {
                Ok(()) => {
                    tracing::info!(what, class = asked.name(), "set");
                    return;
                }
                Err(error) => {
                    tracing::warn!(what, class = asked.name(), %error, "Windows refused it");
                }
            }
        }
        tracing::warn!(what, "the process keeps the class it had");
    }

    fn set_gpu(class: Class) -> Result<(), String> {
        let asked = match class {
            Class::Normal => return Ok(()),
            Class::AboveNormal => D3DKMT_SCHEDULINGPRIORITYCLASS_ABOVE_NORMAL,
            Class::High => D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
            Class::Realtime => D3DKMT_SCHEDULINGPRIORITYCLASS_REALTIME,
        };
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid and
        // needs no closing, and the class is passed by value.
        let status = unsafe { D3DKMTSetProcessSchedulingPriorityClass(GetCurrentProcess(), asked) };
        if status.is_ok() {
            Ok(())
        } else {
            Err(format!("NTSTATUS {:#010x}", status.0))
        }
    }

    fn set_cpu(class: Class) -> Result<(), String> {
        let asked = match class {
            Class::Normal => return Ok(()),
            Class::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            // The CPU's realtime class is never asked for (module docs).
            Class::High | Class::Realtime => HIGH_PRIORITY_CLASS,
        };
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid and
        // needs no closing, and the class is passed by value.
        unsafe { SetPriorityClass(GetCurrentProcess(), asked) }.map_err(|error| error.to_string())
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
