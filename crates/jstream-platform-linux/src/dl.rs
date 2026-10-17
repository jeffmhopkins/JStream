//! Run-time loading of system libraries (ADR-0060 §8).
//!
//! A backend names the library's sonames and the package that ships it; a missing
//! library becomes [`PlatformError::MissingComponent`] with that remedy, never a build
//! failure.

use std::sync::Arc;

use jstream_platform::PlatformError;
use libloading::Library;

/// A loaded system library, shared by the function tables resolved from it so it stays
/// loaded for as long as any of them lives.
#[derive(Clone, Debug)]
pub(crate) struct SystemLibrary {
    lib: Arc<Library>,
    component: &'static str,
}

impl SystemLibrary {
    /// Loads the first of `sonames` that the dynamic loader finds.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] naming `component` and `remedy` when none
    /// loads.
    pub(crate) fn open(
        component: &'static str,
        sonames: &[&str],
        remedy: &str,
    ) -> Result<Self, PlatformError> {
        let mut tried = Vec::new();
        for soname in sonames {
            // SAFETY: loading runs the library's initialisers. Every soname this crate
            // passes names a well-known system library (libpipewire, libva) whose
            // initialisers have no preconditions on the caller, and none is unloaded
            // while a function resolved from it can still be called (the `Arc`).
            match unsafe { Library::new(*soname) } {
                Ok(lib) => {
                    return Ok(Self {
                        lib: Arc::new(lib),
                        component,
                    });
                }
                Err(e) => tried.push(format!("{soname}: {e}")),
            }
        }
        Err(PlatformError::MissingComponent {
            component,
            remedy: format!("{remedy} (tried {})", tried.join("; ")),
        })
    }

    /// Resolves `symbol` as a function pointer of type `F`.
    ///
    /// # Safety
    /// `F` must be the `extern "C"` function-pointer type matching the symbol's C
    /// declaration, and the pointer must not be called after every clone of this
    /// library has been dropped (callers keep a clone beside the pointers).
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when the library lacks the symbol, which means
    /// it is older than the backend needs.
    pub(crate) unsafe fn function<F: Copy>(&self, symbol: &str) -> Result<F, PlatformError> {
        // SAFETY: the caller guarantees `F` matches the symbol's type (this function's
        // contract); copying the pointer out is sound while `self.lib` is alive.
        let sym = unsafe { self.lib.get::<F>(symbol) };
        sym.map(|s| *s)
            .map_err(|e| PlatformError::MissingComponent {
                component: self.component,
                remedy: format!("the installed library lacks `{symbol}` ({e}); update it"),
            })
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;

    use super::SystemLibrary;

    #[test]
    fn a_missing_library_names_its_remedy() {
        let err = SystemLibrary::open(
            "libjstream-does-not-exist",
            &["libjstream-does-not-exist.so.0"],
            "install nothing",
        )
        .expect_err("no such library");
        let PlatformError::MissingComponent { component, remedy } = err else {
            panic!("expected MissingComponent, got {err:?}");
        };
        assert_eq!(component, "libjstream-does-not-exist");
        assert!(remedy.starts_with("install nothing (tried libjstream-does-not-exist.so.0"));
    }

    #[test]
    fn libc_resolves_and_a_missing_symbol_is_reported() {
        let lib = SystemLibrary::open("libc", &["libc.so.6"], "it is always installed")
            .expect("glibc is loaded in every Linux test process");
        // SAFETY: `getpid` is `pid_t getpid(void)`, `pid_t` being `int` on Linux.
        let getpid: unsafe extern "C" fn() -> i32 =
            unsafe { lib.function("getpid") }.expect("libc has getpid");
        // SAFETY: getpid has no preconditions; `lib` is alive.
        let pid = unsafe { getpid() };
        assert_eq!(u32::try_from(pid).ok(), Some(std::process::id()));
        // SAFETY: the symbol doesn't exist, so the type is never used.
        let missing = unsafe { lib.function::<unsafe extern "C" fn()>("jstream_no_such_fn") };
        assert!(matches!(
            missing,
            Err(PlatformError::MissingComponent { .. })
        ));
    }
}
