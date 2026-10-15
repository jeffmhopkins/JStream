//! `--register-vcam`: registers the virtual camera's media source, the
//! `jstream_vcam_windows.dll` shipped beside the daemon, for the Windows camera Frame
//! Server. That registration is machine-wide, so a non-elevated daemon relaunches itself
//! elevated for it: one UAC prompt (ARCHITECTURE › Packaging). The work itself is
//! `jstream-platform-windows`'s.
//!
//! At startup the daemon also checks where the registration points and logs a warning
//! when it names another folder, such as the one a portable install was moved from.

#[cfg(windows)]
pub use os::{log_status, register};

/// The virtual camera exists only on Windows.
#[cfg(not(windows))]
pub fn register() -> anyhow::Result<String> {
    anyhow::bail!("`--register-vcam` is only available on Windows")
}

#[cfg(windows)]
mod os {
    use std::path::PathBuf;

    use anyhow::{Context, bail};
    use jstream_platform_windows::{
        VCAM_DLL, VcamRegistration, is_elevated, register_vcam, run_elevated, vcam_registration,
    };

    /// The media source shipped beside the daemon's executable.
    fn shipped_dll() -> anyhow::Result<PathBuf> {
        let exe = std::env::current_exe().context("could not find the daemon's executable")?;
        let dir = exe
            .parent()
            .context("the daemon's executable has no folder")?;
        Ok(dir.join(VCAM_DLL))
    }

    /// Registers the shipped media source unless the registration already names it, and
    /// returns what to print. A non-elevated daemon runs `--register-vcam` again elevated
    /// and checks the registration it left.
    pub fn register() -> anyhow::Result<String> {
        let dll = shipped_dll()?;
        if !dll.is_file() {
            bail!(
                "the virtual camera DLL is missing: expected `{}` beside the daemon",
                dll.display()
            );
        }
        let before = vcam_registration(&dll)?;
        if before == VcamRegistration::Current {
            return Ok(format!(
                "virtual camera: already registered ({})\n",
                dll.display()
            ));
        }
        if is_elevated() {
            register_vcam(&dll)
                .with_context(|| format!("could not register `{}`", dll.display()))?;
        } else {
            let code = run_elevated("--register-vcam")?;
            if code != 0 {
                bail!(
                    "the elevated registration failed (exit code {code}); run \
                     `jstream-daemon --register-vcam` from an administrator prompt to see why"
                );
            }
        }
        match vcam_registration(&dll)? {
            VcamRegistration::Current => {}
            other => bail!("the registration did not take: it is now {other:?}"),
        }
        let moved = match before {
            VcamRegistration::Moved { registered } => {
                format!(" (it was registered for {})", registered.display())
            }
            _ => String::new(),
        };
        Ok(format!(
            "virtual camera: registered {}{moved}\n",
            dll.display()
        ))
    }

    /// Logs where the virtual camera's registration points: a warning when it names
    /// another folder, so a moved install is noticed before the camera is used, and an
    /// `info` line when there is none. Never fails the daemon.
    pub fn log_status() {
        let Ok(dll) = shipped_dll() else { return };
        if !dll.is_file() {
            tracing::debug!(dll = %dll.display(), "no virtual camera DLL beside the daemon");
            return;
        }
        match vcam_registration(&dll) {
            Ok(VcamRegistration::Current) => {
                tracing::debug!(dll = %dll.display(), "virtual camera registered");
            }
            Ok(VcamRegistration::Missing) => tracing::info!(
                "virtual camera not registered; run `jstream-daemon --register-vcam` to use it"
            ),
            Ok(VcamRegistration::Moved { registered }) => tracing::warn!(
                registered = %registered.display(),
                dll = %dll.display(),
                "the virtual camera is registered for another folder (was JStream moved?); \
                 run `jstream-daemon --register-vcam` to register this one"
            ),
            Err(error) => {
                tracing::debug!(%error, "could not read the virtual camera's registration");
            }
        }
    }
}
