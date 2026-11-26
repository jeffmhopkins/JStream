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
//!
//! The classes, and how the variables name them, are `jstream_platform::priority`'s,
//! where they are tested: the browser helper reads `JSTREAM_GPU_PRIORITY` the same way
//! and asks for the same GPU class for its copies (ADR-0252).

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

    use jstream_platform::priority::{CPU_VARIABLE, Class, GPU_VARIABLE};

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
