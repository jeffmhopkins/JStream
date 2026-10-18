//! `--register-vcam` and `--unregister-vcam`: the virtual camera's optional registration
//! step, and its undo ([ADR-0066](../../../docs/adr/0066-portable-windows-distribution-v2.md)).
//! Registering copies the `jstream_vcam_windows.dll` shipped beside the daemon to an
//! administrators-only folder under `%ProgramFiles%` and registers that copy for the
//! Windows camera Frame Server. Both steps are machine-wide, so a non-elevated daemon
//! relaunches itself elevated for them: one UAC prompt each (ARCHITECTURE › Packaging).
//! The work itself is `jstream-platform-windows`'s.
//!
//! Only these explicit flags register, unregister or ask for elevation. At startup the
//! daemon only reports the registration's state ([`log_status`]): an `info` line when
//! there is none, and a warning when the registered copy is out of date, which says that
//! registering again is optional.

#[cfg(windows)]
pub use os::{log_status, register, unregister};

/// The virtual camera exists only on Windows.
#[cfg(not(windows))]
pub fn register() -> anyhow::Result<String> {
    anyhow::bail!("`--register-vcam` is only available on Windows")
}

/// The virtual camera exists only on Windows.
#[cfg(not(windows))]
pub fn unregister() -> anyhow::Result<String> {
    anyhow::bail!("`--unregister-vcam` is only available on Windows")
}

#[cfg(windows)]
mod os {
    use std::path::PathBuf;

    use anyhow::{Context, bail};
    use jstream_platform_windows::{
        Outdated, VCAM_DLL, VcamRegistration, is_elevated, program_files, register_vcam,
        run_elevated, unregister_vcam, vcam_copy_dir, vcam_registration,
    };

    /// The media source shipped beside the daemon's executable.
    fn shipped_dll() -> anyhow::Result<PathBuf> {
        let exe = std::env::current_exe().context("could not find the daemon's executable")?;
        let dir = exe
            .parent()
            .context("the daemon's executable has no folder")?;
        Ok(dir.join(VCAM_DLL))
    }

    /// Runs `flag` again in an elevated daemon (one UAC prompt) and fails unless it
    /// exits 0.
    fn elevate(flag: &str) -> anyhow::Result<()> {
        let code = run_elevated(flag)?;
        if code != 0 {
            bail!(
                "the elevated step failed (exit code {code}); run `jstream-daemon {flag}` \
                 from an administrator prompt to see why"
            );
        }
        Ok(())
    }

    /// The optional step: copies the shipped media source to the administrators-only
    /// folder and registers the copy, unless the registered copy already has its
    /// content. Returns what to print. A non-elevated daemon runs `--register-vcam` again
    /// elevated and checks the registration it left.
    pub fn register() -> anyhow::Result<String> {
        let dll = shipped_dll()?;
        if !dll.is_file() {
            bail!(
                "the virtual camera DLL is missing: expected `{}` beside the daemon",
                dll.display()
            );
        }
        let before = vcam_registration(&dll)?;
        if let VcamRegistration::Current { copy } = &before {
            return Ok(format!(
                "virtual camera: already registered ({})\n",
                copy.display()
            ));
        }
        if is_elevated() {
            register_vcam(&dll)
                .with_context(|| format!("could not register a copy of `{}`", dll.display()))?;
        } else {
            elevate("--register-vcam")?;
        }
        let copy = match vcam_registration(&dll)? {
            VcamRegistration::Current { copy } => copy,
            other => bail!("the registration did not take: it is now {other:?}"),
        };
        let was = match before {
            VcamRegistration::Outdated { registered, .. } => {
                format!(" (it was registered for {})", registered.display())
            }
            VcamRegistration::Missing | VcamRegistration::Current { .. } => String::new(),
        };
        Ok(format!(
            "virtual camera: registered {}, a copy of {}{was}\n",
            copy.display(),
            dll.display()
        ))
    }

    /// Removes the registration and the copy. Returns what to print. A non-elevated
    /// daemon runs `--unregister-vcam` again elevated, unless there is nothing to remove.
    pub fn unregister() -> anyhow::Result<String> {
        let copy_dir = vcam_copy_dir(&program_files()?);
        // The shipped DLL only matters for the comparison, which this doesn't need.
        let dll = shipped_dll().unwrap_or_else(|_| PathBuf::from(VCAM_DLL));
        if vcam_registration(&dll)? == VcamRegistration::Missing && !copy_dir.exists() {
            return Ok("virtual camera: not registered\n".to_owned());
        }
        if is_elevated() {
            let removed = unregister_vcam().context("could not unregister the virtual camera")?;
            tracing::debug!(?removed, "unregistered the virtual camera");
        } else {
            elevate("--unregister-vcam")?;
        }
        if vcam_registration(&dll)? != VcamRegistration::Missing {
            bail!("the registration is still there");
        }
        if copy_dir.exists() {
            bail!(
                "the registration is removed, but `{}` is not: close the apps using the \
                 camera, then run `jstream-daemon --unregister-vcam` again",
                copy_dir.display()
            );
        }
        Ok(format!(
            "virtual camera: unregistered, and {} removed\n",
            copy_dir.display()
        ))
    }

    /// Logs the virtual camera's registration state. Registering is optional, so this
    /// never prompts or registers: `debug` when current, `info` when there is none, and
    /// a warning when the registered copy is out of date. Never fails the daemon.
    pub fn log_status() {
        let Ok(dll) = shipped_dll() else { return };
        if !dll.is_file() {
            tracing::debug!(dll = %dll.display(), "no virtual camera DLL beside the daemon");
            return;
        }
        match vcam_registration(&dll) {
            Ok(VcamRegistration::Current { copy }) => {
                tracing::debug!(copy = %copy.display(), "virtual camera registered");
            }
            Ok(VcamRegistration::Missing) => tracing::info!(
                "virtual camera not registered; it is optional: run \
                 `jstream-daemon --register-vcam` to use it (asks for administrator rights)"
            ),
            Ok(VcamRegistration::Outdated { registered, why }) => {
                let why = match why {
                    Outdated::Elsewhere => {
                        "the virtual camera is registered for a DLL outside JStream's \
                         Program Files folder, which other programs may be able to change"
                    }
                    Outdated::Differs => {
                        "the virtual camera's registered copy differs from this JStream's \
                         DLL (was JStream updated?)"
                    }
                    Outdated::CopyMissing => "the virtual camera's registered copy is missing",
                };
                tracing::warn!(
                    registered = %registered.display(),
                    dll = %dll.display(),
                    "{why}; optional: run `jstream-daemon --register-vcam` to register a \
                     copy of this one (asks for administrator rights)"
                );
            }
            Err(error) => {
                tracing::debug!(%error, "could not read the virtual camera's registration");
            }
        }
    }
}
